//! Dynamic SQL compared across dialects: T-SQL `EXEC (@sql)` /
//! `sp_executesql` against PL/pgSQL `EXECUTE '…'`, by the statement each
//! text runs. Text the code alone does not fix stays unknown.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const CAT_T: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, a int NULL);\n";
const CAT_P: &str = "CREATE TABLE t (id int NOT NULL PRIMARY KEY, a int NULL);\n";

fn run(a: &str, b: &str) -> Report {
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
        catalog_b: vec![Artifact::new("cat.pg.sql", P, CAT_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &o,
    )
}

fn tsql(body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    DECLARE @SQL nvarchar(max);\n{body}\nEND;\n"
    )
}

fn pg(body: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE p(p_x int)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n    v_sql text;\nBEGIN\n{body}\nEND;\n$$;\n"
    )
}

fn dynamic_reasons(r: &Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| x.contains("dynamic SQL"))
        .collect()
}

#[test]
fn known_text_runs_the_same_statement() {
    let r = run(
        &tsql("    SET @SQL = N'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE (@SQL);"),
        &pg("    v_sql := 'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE 'UPDATE t SET a = 1 WHERE id = 2';"),
    );
    assert!(dynamic_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?} {:?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r
        .assumptions
        .iter()
        .any(|a| a.id == "ASM-TSQL-DYNAMIC-SESSION-OPTIONS"));
}

#[test]
fn a_literal_and_sp_executesql_read_the_same() {
    let r = run(
        &tsql("    EXEC sp_executesql N'DELETE FROM dbo.t WHERE id = 3';"),
        &pg("    EXECUTE $q$DELETE FROM t WHERE id = 3$q$;"),
    );
    assert!(dynamic_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}",
        r.findings
    );
}

#[test]
fn another_statement_in_the_text_is_different() {
    let r = run(
        &tsql("    SET @SQL = N'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE (@SQL);"),
        &pg("    v_sql := 'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE 'UPDATE t SET a = 3 WHERE id = 2';"),
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.unknown_reasons);
    assert!(r
        .findings
        .iter()
        .any(|f| f.witness.contains("dynamic SQL") && f.witness.contains("(3, 2)")));
}

#[test]
fn a_witness_is_not_kept_after_statements_that_change_the_tables() {
    // The DELETE before leaves no row the witness needs: only unknown.
    let r = run(
        &tsql("    DELETE FROM dbo.t;\n    SET @SQL = N'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE (@SQL);"),
        &pg("    DELETE FROM t;\n    v_sql := 'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE 'UPDATE t SET a = 3 WHERE id = 2';"),
    );
    assert!(
        !r.findings
            .iter()
            .any(|f| f.verdict == Verdict::Different && f.witness.contains("dynamic SQL")),
        "{:#?}",
        r.findings
    );
    assert!(dynamic_reasons(&r)
        .iter()
        .any(|x| x.contains("may not leave")));
}

#[test]
fn text_from_a_parameter_is_unknown() {
    let r = run(
        "CREATE PROCEDURE dbo.p @s nvarchar(max)\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    EXECUTE (@s);\nEND;\n",
        "CREATE OR REPLACE PROCEDURE p(p_s text)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    EXECUTE p_s;\nEND;\n$$;\n",
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert!(
        dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("not known before it runs") || x.contains("not a constant")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn texts_the_batch_runs_differently_are_unknown() {
    for (t, p, why) in [
        ("SELECT a FROM dbo.t", "SELECT a FROM t", "query"),
        (
            "INSERT INTO #x VALUES (1)",
            "INSERT INTO x VALUES (1)",
            "#temp",
        ),
        (
            "DELETE FROM dbo.t; DELETE FROM dbo.t",
            "DELETE FROM t; DELETE FROM t",
            "2 statements",
        ),
    ] {
        let r = run(
            &tsql(&format!("    EXECUTE (N'{t}');")),
            &pg(&format!("    EXECUTE '{p}';")),
        );
        assert_ne!(r.verdict(), Verdict::Equal, "{t}");
        assert!(
            dynamic_reasons(&r).iter().any(|x| x.contains(why)),
            "{why}: {:?}",
            r.unknown_reasons
        );
    }
}

/// A procedure called through PostgreSQL `EXECUTE` cannot commit: when the
/// callee's conversion holds `COMMIT`, `CALL q()` fails with 2D000 and
/// writes nothing, where T-SQL `EXEC (N'EXEC dbo.q')` runs it and keeps its
/// writes. The callee's body may be outside the tree, so the pair is never
/// equal, whatever `q` holds.
#[test]
fn a_procedure_call_inside_the_text_is_not_equal() {
    let r = run(
        &tsql("    EXEC (N'EXEC dbo.q');"),
        &pg("    EXECUTE 'CALL q()';"),
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
    assert_ne!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?} {:?}",
        r.assumptions,
        r.findings
    );
    assert!(
        dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("calls a procedure") && x.contains("cannot commit")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn unequal_counts_are_unknown() {
    let r = run(
        &tsql("    EXECUTE (N'DELETE FROM dbo.t');\n    EXECUTE (N'DELETE FROM dbo.t');"),
        &pg("    EXECUTE 'DELETE FROM t';"),
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert!(dynamic_reasons(&r)
        .iter()
        .any(|x| x.contains("2 and 1 times")));
}

#[test]
fn dynamic_sql_inside_a_compared_if_is_not_compared() {
    let r = run(
        &tsql("    IF @x = 1\n    BEGIN\n        UPDATE dbo.t SET a = 1 WHERE id = 2;\n        EXECUTE (N'DELETE FROM dbo.t');\n    END;"),
        &pg("    IF p_x = 1 THEN\n        UPDATE t SET a = 9 WHERE id = 2;\n        EXECUTE 'DELETE FROM t';\n    END IF;"),
    );
    // The dynamic SQL inside the IF is not compared, so nothing is equal.
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.starts_with("side A (T-SQL) line 9: dynamic SQL inside an IF or a loop")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn a_call_moved_across_the_dynamic_sql_is_unknown() {
    // q sets v = 1 on row 3: T-SQL deletes it, PostgreSQL keeps it.
    let cat_t = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);\nGO\nCREATE PROCEDURE dbo.q AS BEGIN UPDATE dbo.t SET v = 1 WHERE id = 3; END;\n";
    let cat_p = "CREATE TABLE t (id int NOT NULL PRIMARY KEY, v int NULL);\nCREATE OR REPLACE PROCEDURE q() LANGUAGE plpgsql AS $$ BEGIN UPDATE t SET v = 1 WHERE id = 3; END; $$;\n";
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, cat_t)],
        catalog_b: vec![Artifact::new("cat.pg.sql", P, cat_p)],
        ..Options::default()
    };
    let r = diff(
        &Artifact::new(
            "a.sql",
            T,
            "CREATE PROCEDURE dbo.p\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    EXEC dbo.q;\n    EXEC (N'DELETE FROM dbo.t WHERE v = 1');\nEND;\n",
        ),
        &Artifact::new(
            "b.sql",
            P,
            "CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    EXECUTE 'DELETE FROM t WHERE v = 1';\n    CALL q();\nEND;\n$$;\n",
        ),
        &o,
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
    assert!(
        dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("another order")),
        "{:?}",
        r.unknown_reasons
    );
    // The same order on both sides is still compared.
    let r = diff(
        &Artifact::new(
            "a.sql",
            T,
            "CREATE PROCEDURE dbo.p\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    EXEC dbo.q;\n    EXEC (N'DELETE FROM dbo.t WHERE v = 1');\nEND;\n",
        ),
        &Artifact::new(
            "b.sql",
            P,
            "CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    CALL q();\n    EXECUTE 'DELETE FROM t WHERE v = 1';\nEND;\n$$;\n",
        ),
        &o,
    );
    assert!(
        !dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("another order")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn dynamic_sql_inside_an_if_is_not_compared() {
    // Only one side guards the text with the IF.
    let r = run(
        &tsql("    IF @x = 1\n        EXECUTE (N'DELETE FROM dbo.t');"),
        &pg("    EXECUTE 'DELETE FROM t';"),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("dynamic SQL inside an IF or a loop")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn sp_executesql_parameters_against_using() {
    let t = tsql("    DECLARE @a int = 1;\n    EXEC sp_executesql N'UPDATE dbo.t SET a = @v WHERE id = @k', N'@v int, @k int', @v = @a, @k = @x;");
    let p = pg("    DECLARE\n        v_a int := 1;\n    BEGIN\n    EXECUTE 'UPDATE t SET a = $1 WHERE id = $2' USING v_a, p_x;\n    END;");
    let r = run(&t, &p);
    assert!(dynamic_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?} {:?}",
        r.findings,
        r.unknown_reasons
    );
}

fn param_case(targs: &str, ttypes: &str, using: &str, ptext: &str) -> Report {
    run(
        &tsql(&format!("    DECLARE @a int = 1;\n    DECLARE @b bigint = 1;\n    EXEC sp_executesql N'UPDATE dbo.t SET a = @v WHERE id = @k', N'{ttypes}', {targs};")),
        &pg(&format!("    DECLARE\n        v_a int := 1;\n        v_b bigint := 1;\n    BEGIN\n    EXECUTE '{ptext}' USING {using};\n    END;")),
    )
}

#[test]
fn parameters_passed_differently_are_not_equal() {
    let same = "UPDATE t SET a = $1 WHERE id = $2";
    // Values swapped: the routine passes @x as the new value on one side only.
    let r = param_case("@v = @a, @k = @x", "@v int, @k int", "p_x, v_a", same);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
    // Placeholders swapped inside the text.
    let r = param_case(
        "@v = @a, @k = @x",
        "@v int, @k int",
        "v_a, p_x",
        "UPDATE t SET a = $2 WHERE id = $1",
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
    // A bigint passed to an int parameter: T-SQL converts it, $1 is bigint.
    let r = param_case("@v = @b, @k = @x", "@v int, @k int", "v_b, p_x", same);
    assert!(
        dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("not the parameter's")),
        "{:?}",
        r.unknown_reasons
    );
    // A literal value is not compared.
    let r = param_case("@v = 2, @k = @x", "@v int, @k int", "2, p_x", same);
    assert!(
        dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("only a variable")),
        "{:?}",
        r.unknown_reasons
    );
    // A declared parameter not passed fails the T-SQL call.
    let r = param_case("@v = @a", "@v int, @k int", "v_a", same);
    assert!(
        dynamic_reasons(&r).iter().any(|x| x.contains("not passed")),
        "{:?}",
        r.unknown_reasons
    );
    // Positional values bind in order.
    let r = param_case("@a, @x", "@v int, @k int", "v_a, p_x", same);
    assert!(dynamic_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    // $3 with two values.
    let r = param_case(
        "@v = @a, @k = @x",
        "@v int, @k int",
        "v_a, p_x",
        "UPDATE t SET a = $3 WHERE id = $2",
    );
    assert!(
        dynamic_reasons(&r).iter().any(|x| x.contains("$3")),
        "{:?}",
        r.unknown_reasons
    );
}
