//! `SELECT @a = e FROM …` assigns row by row: with no row the variables keep their
//! values, with several the last row's values win. PostgreSQL's `SELECT … INTO`
//! sets NULL on no row and keeps the first row, so a query that can return any
//! other number of rows than one converts to a loop over it; a query that
//! always returns one row stays a plain `SELECT … INTO`.

use sqldiff_convert::{check, rewrite, Conversion, Status, Unit};
use sqldiff_core::{Dialect, Options};

fn convert(src: &str, to: Dialect) -> Conversion {
    check(src, rewrite(src, Dialect::Tsql, to), &Options::default())
}

fn select_unit<'a>(c: &'a Conversion, src: &str) -> &'a Unit {
    let at = src.find("SELECT").unwrap();
    c.units
        .iter()
        .find(|u| u.src.start <= at && at < u.src.end)
        .unwrap()
}

fn has_rule(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

const CARDINALITY: &str = "ASM-SELECT-ASSIGN-CARDINALITY";

#[test]
fn a_query_that_can_return_no_row_becomes_a_loop_that_keeps_the_old_value() {
    let src = "DECLARE @n int = 5;\nSELECT @n = c FROM t WHERE id = 1;\n";
    let c = convert(src, Dialect::Postgres);
    assert!(
        c.output
            .contains("DECLARE\n    sqldiff_r record;\n  BEGIN\n    FOR sqldiff_r IN SELECT c AS sqldiff_c1 FROM t WHERE id = 1 LOOP\n      v_n := sqldiff_r.sqldiff_c1;\n    END LOOP;\n  END;"),
        "{}",
        c.output
    );
    assert!(!c.output.contains("INTO"), "{}", c.output);
    assert!(!has_rule(select_unit(&c, src), CARDINALITY));
    assert!(sqldiff_syntax::parse(&c.output, Dialect::Postgres)
        .diagnostics
        .is_empty());
}

#[test]
fn several_targets_and_expressions_and_top_with_order() {
    let src =
        "DECLARE @a int, @b varchar(5);\nSELECT TOP 1 @a = x + 1, @b = y FROM t ORDER BY z;\n";
    let c = convert(src, Dialect::Postgres);
    assert!(
        c.output.contains(
            "FOR sqldiff_r IN SELECT x + 1 AS sqldiff_c1, y AS sqldiff_c2 FROM t ORDER BY z NULLS FIRST LIMIT 1 LOOP\n      v_a := sqldiff_r.sqldiff_c1;\n      v_b := sqldiff_r.sqldiff_c2;\n    END LOOP;"
        ),
        "{}",
        c.output
    );
}

#[test]
fn an_aggregate_without_grouping_always_returns_one_row_so_into_is_exact() {
    for q in [
        "SELECT @n = COUNT(*) FROM t;",
        "SELECT @n = MAX(c) FROM t WHERE a = 1;",
        "SELECT @n = NULLIF(COUNT(*), 0) FROM t;",
        "SELECT @n = SUM(a) + 1 FROM t;",
    ] {
        let src = format!("DECLARE @n int;\n{q}\n");
        let c = convert(&src, Dialect::Postgres);
        assert!(c.output.contains("INTO v_n"), "{q}: {}", c.output);
        assert!(!c.output.contains("FOR sqldiff_r"), "{q}: {}", c.output);
        assert!(
            !has_rule(select_unit(&c, &src), CARDINALITY),
            "{q}: nothing to assume"
        );
    }
}

#[test]
fn grouping_having_windows_and_subqueries_can_return_other_row_counts() {
    for q in [
        "SELECT @n = COUNT(*) FROM t GROUP BY a;",
        "SELECT @n = COUNT(*) FROM t HAVING COUNT(*) > 1;",
        "SELECT @n = COUNT(*) OVER () FROM t;",
        "SELECT @n = (SELECT COUNT(*) FROM u) FROM t;",
        "SELECT @n = a FROM t UNION SELECT b FROM u;",
    ] {
        let src = format!("DECLARE @n int;\n{q}\n");
        let c = convert(&src, Dialect::Postgres);
        let u = select_unit(&c, &src);
        // Either exact (a loop), or refused: never a plain INTO on a query that
        // can return no row or several.
        assert!(
            c.output.contains("FOR sqldiff_r") || c.output.contains("not converted"),
            "{q}: {}",
            c.output
        );
        assert!(!has_rule(u, CARDINALITY), "{q}");
    }
}

#[test]
fn an_expression_that_uses_its_own_target_keeps_into_and_says_why() {
    let src = "DECLARE @s varchar(100) = '';\nSELECT @s = @s + name FROM t;\n";
    let c = convert(src, Dialect::Postgres);
    assert!(c.output.contains("INTO v_s"), "{}", c.output);
    assert!(!c.output.contains("FOR sqldiff_r"), "{}", c.output);
    let u = select_unit(&c, src);
    let n = u.notes.iter().find(|n| n.rule == CARDINALITY).unwrap();
    assert!(n.text.contains("accumulates"), "{}", n.text);
}

#[test]
fn db2_for_i_is_unchanged() {
    let src = "DECLARE @n int;\nSELECT @n = c FROM t WHERE id = 1;\n";
    let c = convert(src, Dialect::Db2i);
    assert!(c.output.contains("INTO"), "{}", c.output);
    assert!(!c.output.contains("FOR sqldiff_r"), "{}", c.output);
    assert!(has_rule(select_unit(&c, src), CARDINALITY));
}

#[test]
fn the_engine_no_longer_finds_a_no_row_difference() {
    let src = "DECLARE @n int = 5;\nSELECT @n = c FROM t WHERE id = 1;\n";
    let c = convert(src, Dialect::Postgres);
    let rules: Vec<&str> = c.report.findings.iter().map(|f| f.rule.as_str()).collect();
    assert!(!rules.contains(&"equiv.no-rows"), "{rules:?}");
}

#[test]
fn reading_an_assigned_variable_outside_the_select_list_keeps_into() {
    for src in [
        "DECLARE @n int = 5;\nSELECT @n = c FROM t WHERE c > @n;\n",
        "DECLARE @n int = 5, @m int;\nSELECT @m = c FROM t ORDER BY ABS(c - @m);\n",
    ] {
        let c = convert(src, Dialect::Postgres);
        assert!(!c.output.contains("FOR sqldiff_r"), "{}", c.output);
        let n = select_unit(&c, src)
            .notes
            .iter()
            .find(|n| n.rule == CARDINALITY)
            .unwrap_or_else(|| panic!("{src}"));
        assert!(n.text.contains("row by row"), "{}", n.text);
    }
}

#[test]
fn top_zero_or_offset_can_drop_the_aggregate_row() {
    for src in [
        "DECLARE @n int = 5;\nSELECT TOP 0 @n = COUNT(*) FROM t;\n",
        "DECLARE @n int = 5;\nSELECT @n = COUNT(*) FROM t ORDER BY 1 OFFSET 1 ROWS;\n",
    ] {
        let c = convert(src, Dialect::Postgres);
        assert!(c.output.contains("FOR sqldiff_r"), "{}", c.output);
        assert!(!c.output.contains("INTO"), "{}", c.output);
    }
}

#[test]
fn names_of_the_loop_in_the_statement_keep_into() {
    for src in [
        "DECLARE @n int;\nSELECT @n = sqldiff_r FROM t;\n",
        "DECLARE @n int;\nSELECT @n = c FROM t ORDER BY [SQLDIFF_C1];\n",
        "DECLARE @n int;\nSELECT @n = c FROM sqldiff_c2;\n",
    ] {
        let c = convert(src, Dialect::Postgres);
        assert!(!c.output.contains("FOR sqldiff_r"), "{}", c.output);
        assert!(has_rule(select_unit(&c, src), CARDINALITY), "{src}");
    }
}

#[test]
fn the_engine_finds_no_no_row_difference_for_an_ungrouped_aggregate() {
    // The aggregate always returns one row, so INTO is exact (SQL Server 2022
    // and PostgreSQL 18 both assign 0 on an empty table).
    let src = "DECLARE @n int = 5;\nSELECT @n = COUNT(*) FROM t;\n";
    let c = convert(src, Dialect::Postgres);
    assert!(c.output.contains("INTO v_n"), "{}", c.output);
    let rules: Vec<&str> = c.report.findings.iter().map(|f| f.rule.as_str()).collect();
    assert!(!rules.contains(&"equiv.no-rows"), "{rules:?}");
}

#[test]
fn a_where_without_from_is_refused() {
    // SQL Server 2022 keeps @n when the condition is false; PL/pgSQL
    // `v_n := 1 WHERE …` would set NULL.
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let src = "DECLARE @n int = 5, @id int = 1;\nSELECT @n = 1 WHERE @id = 0;\n";
        let c = convert(src, to);
        let u = select_unit(&c, src);
        assert_eq!(u.status, Status::NotConverted, "{to:?}: {}", c.output);
        assert!(!c.output.contains(":= 1 WHERE"), "{}", c.output);
    }
}
