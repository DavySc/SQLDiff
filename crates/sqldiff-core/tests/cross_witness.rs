//! Cross-dialect witnesses on canonical terms (equiv::xwit): a changed
//! operator, condition, value or dropped WHERE is "different" with a
//! concrete witness, and rewrites that keep the meaning never are.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const DDL: &str = "
CREATE TABLE item (item_id integer PRIMARY KEY, name varchar(100) NOT NULL, qty integer, code char(5), flag integer);
CREATE TABLE item_price (item_id integer PRIMARY KEY, unit_price decimal(12,2) NOT NULL, updated_at timestamp);
";

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const D: Dialect = Dialect::Db2i;

fn run(a: &str, da: Dialect, b: &str, db: Dialect, catalog: bool) -> Report {
    let opts = if catalog {
        Options {
            catalog_a: vec![Artifact::new("catalog.sql", da, DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", db, DDL)],
            ..Options::default()
        }
    } else {
        Options::default()
    };
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &opts,
    )
}

fn ts(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @Id int, @Qty int AS BEGIN {body} END")
}

fn pg(body: &str) -> String {
    format!(
        "CREATE PROCEDURE p(p_id integer, p_qty integer) LANGUAGE plpgsql AS $$
         BEGIN {body} END; $$;"
    )
}

fn db2(body: &str) -> String {
    format!(
        "CREATE PROCEDURE p (IN p_id INTEGER, IN p_qty INTEGER) LANGUAGE SQL MODIFIES SQL DATA
         SET OPTION USRPRF = *USER
         BEGIN {body} END;"
    )
}

fn witness(r: &Report) -> Option<&sqldiff_core::model::Finding> {
    r.findings.iter().find(|f| f.rule == "equiv.witness")
}

fn ids(f: &sqldiff_core::model::Finding) -> Vec<&str> {
    f.assumptions.iter().map(String::as_str).collect()
}

/// The pair is "different" through a witness; returns its text.
fn different(a: &str, da: Dialect, b: &str, db: Dialect) -> String {
    let mut text = String::new();
    for catalog in [false, true] {
        let r = run(a, da, b, db, catalog);
        assert_eq!(
            r.verdict,
            Verdict::Different,
            "catalog {catalog}: {:#?} {:?}",
            r.findings,
            r.unknown_reasons
        );
        let f = witness(&r).unwrap_or_else(|| panic!("no witness: {:#?}", r.findings));
        assert!(ids(f).contains(&"ASM-WITNESS-DATA") || f.witness.contains("returns"));
        text = f.witness.clone();
    }
    text
}

/// The pair is never "different" through a witness (nor equal when it is not).
fn no_witness(a: &str, da: Dialect, b: &str, db: Dialect) {
    for catalog in [false, true] {
        let r = run(a, da, b, db, catalog);
        assert!(
            witness(&r).is_none(),
            "catalog {catalog}: {:#?}",
            r.findings
        );
    }
}

const BASE: (&str, &str) = (
    "DELETE FROM dbo.item WHERE item_id = @Id;",
    "DELETE FROM item WHERE item_id = p_id;",
);

#[test]
fn the_unchanged_pair_is_equal() {
    for (b, d) in [(pg(BASE.1), P), (db2(BASE.1), D)] {
        let r = run(&ts(BASE.0), T, &b, d, true);
        assert_eq!(
            r.verdict,
            Verdict::EqualUnderAssumptions,
            "{:?} {:?}",
            r.findings,
            r.unknown_reasons
        );
    }
}

#[test]
fn changed_comparison_operator() {
    let w = different(
        &ts(BASE.0),
        T,
        &pg("DELETE FROM item WHERE item_id <> p_id;"),
        P,
    );
    assert!(
        w.contains("deletes 1 row") && w.contains("deletes no row"),
        "{w}"
    );
    different(
        &ts(BASE.0),
        T,
        &db2("DELETE FROM item WHERE item_id <> p_id;"),
        D,
    );
    different(
        &ts("UPDATE dbo.item SET qty = @Qty WHERE item_id > @Id;"),
        T,
        &pg("UPDATE item SET qty = p_qty WHERE item_id >= p_id;"),
        P,
    );
    different(
        &ts("UPDATE dbo.item SET qty = @Qty WHERE item_id > @Id;"),
        T,
        &db2("UPDATE item SET qty = p_qty WHERE item_id < p_id;"),
        D,
    );
}

#[test]
fn dropped_or_weakened_where() {
    let w = different(&ts(BASE.0), T, &pg("DELETE FROM item;"), P);
    assert!(
        w.contains("deletes no row") || w.contains("deletes 1 row"),
        "{w}"
    );
    different(&ts(BASE.0), T, &db2("DELETE FROM item;"), D);
    different(
        &ts("DELETE FROM dbo.item WHERE item_id = @Id AND qty > @Qty;"),
        T,
        &pg("DELETE FROM item WHERE item_id = p_id OR qty > p_qty;"),
        P,
    );
    different(
        &ts("DELETE FROM dbo.item WHERE item_id = @Id AND qty IS NULL;"),
        T,
        &pg("DELETE FROM item WHERE item_id = p_id;"),
        P,
    );
}

#[test]
fn changed_values_parameters_and_columns() {
    different(
        &ts(BASE.0),
        T,
        &pg("DELETE FROM item WHERE item_id = p_id + 1;"),
        P,
    );
    different(
        &ts(BASE.0),
        T,
        &pg("DELETE FROM item WHERE item_id = 1;"),
        P,
    );
    different(
        &ts(BASE.0),
        T,
        &pg("DELETE FROM item WHERE item_id = p_qty;"),
        P,
    );
    different(&ts(BASE.0), T, &pg("DELETE FROM item WHERE qty = p_id;"), P);
    let w = different(
        &ts("UPDATE dbo.item SET qty = @Qty WHERE item_id = @Id;"),
        T,
        &db2("UPDATE item SET qty = p_qty + 1 WHERE item_id = p_id;"),
        D,
    );
    assert!(w.contains("afterwards"), "{w}");
    let w = different(
        &ts("INSERT INTO dbo.item (item_id, name, qty) VALUES (@Id, 'x', @Qty);"),
        T,
        &pg("INSERT INTO item (item_id, name, qty) VALUES (p_id, 'x', 0);"),
        P,
    );
    assert!(w.contains("inserts"), "{w}");
}

#[test]
fn string_values_respect_the_comparison() {
    // Across dialects a string comparison differs by collation unless both
    // sides compare the same way: PostgreSQL CHAR against Db2 for i
    // (case-sensitive, blank-padded).
    let r = run(
        &pg("DELETE FROM item WHERE code = 'abc';"),
        P,
        &db2("DELETE FROM item WHERE code <> 'abc';"),
        D,
        true,
    );
    let f = witness(&r).unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert!(f.witness.contains("deletes 1 row"), "{}", f.witness);
    // A literal with blanks is not used.
    let r = run(
        &pg("DELETE FROM item WHERE code = 'a b';"),
        P,
        &db2("DELETE FROM item WHERE code <> 'a b';"),
        D,
        true,
    );
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
}

#[test]
fn rewrites_with_the_same_meaning_have_no_witness() {
    // Equal for integers; the search only uses integers and finds nothing.
    no_witness(
        &ts("DELETE FROM dbo.item WHERE qty > @Qty;"),
        T,
        &pg("DELETE FROM item WHERE qty >= p_qty + 1;"),
        P,
    );
    no_witness(
        &ts("DELETE FROM dbo.item WHERE qty = @Qty OR qty <> @Qty;"),
        T,
        &pg("DELETE FROM item WHERE qty IS NOT NULL;"),
        P,
    );
    no_witness(
        &ts("UPDATE dbo.item SET qty = @Qty WHERE item_id = @Id;"),
        T,
        &pg("UPDATE item SET qty = p_qty + 0 WHERE item_id = p_id;"),
        P,
    );
    no_witness(
        &ts("DELETE FROM dbo.item WHERE qty BETWEEN @Id AND @Qty;"),
        T,
        &pg("DELETE FROM item WHERE qty >= p_id AND qty <= p_qty;"),
        P,
    );
}

#[test]
fn decimals_and_unknown_shapes_stay_unknown() {
    // Not equivalent for decimals, but the integer search cannot show it.
    let r = run(
        &ts("DELETE FROM dbo.item_price WHERE unit_price > @Qty;"),
        T,
        &pg("DELETE FROM item_price WHERE unit_price >= p_qty + 1;"),
        P,
        true,
    );
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
    assert_ne!(r.verdict, Verdict::EqualUnderAssumptions);
    // Subqueries are not evaluated.
    no_witness(
        &ts("DELETE FROM dbo.item WHERE item_id IN (SELECT item_id FROM dbo.item_price);"),
        T,
        &pg("DELETE FROM item WHERE item_id NOT IN (SELECT item_id FROM item_price);"),
        P,
    );
}

#[test]
fn reaching_the_statement_is_assumed_when_others_surround_it() {
    let a = ts(
        "DELETE FROM dbo.item_price WHERE item_id = @Id; DELETE FROM dbo.item WHERE item_id = @Id;",
    );
    let b =
        pg("DELETE FROM item_price WHERE item_id = p_id; DELETE FROM item WHERE item_id <> p_id;");
    let r = run(&a, T, &b, P, true);
    assert_eq!(r.verdict, Verdict::Different);
    let f = witness(&r).expect("witness");
    assert!(ids(f).contains(&"ASM-WITNESS-PATH"), "{:?}", f.assumptions);
    let r = run(
        &ts(BASE.0),
        T,
        &pg("DELETE FROM item WHERE item_id <> p_id;"),
        P,
        true,
    );
    assert!(!ids(witness(&r).unwrap()).contains(&"ASM-WITNESS-PATH"));
}

#[test]
fn parameters_must_line_up() {
    // Different parameter lists: positions do not mean the same input.
    let r = run(
        &ts(BASE.0),
        T,
        "CREATE PROCEDURE p(p_id integer) LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM item WHERE item_id <> p_id; END; $$;",
        P,
        true,
    );
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
}

#[test]
fn tsql_parameters_without_parentheses_and_with_as() {
    let a = "CREATE PROCEDURE dbo.p
        @Id AS int,
        @Qty AS int OUTPUT
    AS
    BEGIN
        DELETE FROM dbo.item WHERE item_id = @Id;
    END";
    let b = "CREATE PROCEDURE p(p_id integer, INOUT p_qty integer) LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM item WHERE item_id = p_id; END; $$;";
    let r = run(a, T, b, P, true);
    assert!(
        !r.unknown_reasons
            .iter()
            .any(|x| x.contains("unrecognised parameter")),
        "{:?}",
        r.unknown_reasons
    );
    assert_eq!(
        r.verdict,
        Verdict::EqualUnderAssumptions,
        "{:#?} {:?}",
        r.findings,
        r.unknown_reasons
    );
    let b2 = b.replace("item_id = p_id", "item_id <> p_id");
    let r = run(a, T, &b2, P, true);
    assert_eq!(r.verdict, Verdict::Different);
    assert!(witness(&r).is_some(), "{:#?}", r.findings);
}

#[test]
fn looked_up_values_and_out_parameters() {
    // A function returning a looked-up value (assignment and RETURN are
    // lowered to one statement).
    let ts_fn = "CREATE FUNCTION dbo.qty_of(@Id int) RETURNS int AS
    BEGIN
      DECLARE @q int;
      SELECT @q = qty FROM dbo.item WHERE flag = @Id;
      RETURN @q;
    END";
    let pg_fn = |cond: &str| {
        format!(
            "CREATE FUNCTION qty_of(p_id integer) RETURNS integer LANGUAGE plpgsql AS $$
             DECLARE v_q integer;
             BEGIN SELECT qty INTO v_q FROM item WHERE {cond}; RETURN v_q; END; $$;"
        )
    };
    let r = run(ts_fn, T, &pg_fn("flag = p_id"), P, true);
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
    let w = different(ts_fn, T, &pg_fn("flag <> p_id"), P);
    assert!(w.contains("returns") && w.contains("item holds"), "{w}");
    // TOP 1 / LIMIT 1 over at most one qualifying row.
    let w = different(
        &ts_fn.replace("SELECT @q", "SELECT TOP 1 @q"),
        T,
        &pg_fn("flag < p_id LIMIT 1"),
        P,
    );
    assert!(w.contains("returns"), "{w}");

    // An OUT parameter set by the only statement; the zero-row behaviour
    // differs anyway (T-SQL keeps the value, PostgreSQL sets NULL).
    let ts_out = "CREATE PROCEDURE dbo.qty_of @Id int, @q int OUTPUT AS
      BEGIN SET @q = @Id + 1; END";
    let pg_out = |e: &str| {
        format!(
            "CREATE PROCEDURE qty_of(p_id integer, INOUT p_q integer) LANGUAGE plpgsql AS $$
             BEGIN p_q := {e}; END; $$;"
        )
    };
    let r = run(ts_out, T, &pg_out("p_id + 1"), P, true);
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
    let w = different(ts_out, T, &pg_out("p_id + 2"), P);
    assert!(w.contains("returns 1") && w.contains("returns 2"), "{w}");
    // A variable read later is not decided here.
    let r = run(
        "CREATE PROCEDURE dbo.qty_of @Id int, @q int OUTPUT AS
          BEGIN SET @q = @Id + 1; DELETE FROM dbo.item WHERE qty = @q; END",
        T,
        "CREATE PROCEDURE qty_of(p_id integer, INOUT p_q integer) LANGUAGE plpgsql AS $$
         BEGIN p_q := p_id + 2; DELETE FROM item WHERE qty = p_q; END; $$;",
        P,
        true,
    );
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
}

#[test]
fn changes_that_compensate_each_other_have_no_witness() {
    // Review of #64: each pair differs, the routines do not.
    let cases = [
        (
            "CREATE PROCEDURE dbo.p @p int, @x int OUTPUT AS BEGIN SET @x = @p + 1; SET @x = @x * 2; END",
            "CREATE PROCEDURE p(p_p integer, INOUT p_x integer) LANGUAGE plpgsql AS $$
             BEGIN p_x := p_p + 2; p_x := (p_x - 1) * 2; END; $$;",
        ),
        (
            &*ts("UPDATE dbo.item SET qty = @Qty WHERE item_id = @Id; UPDATE dbo.item SET qty = qty + 1 WHERE item_id = @Id;"),
            &*pg("UPDATE item SET qty = p_qty + 1 WHERE item_id = p_id; UPDATE item SET qty = qty WHERE item_id = p_id;"),
        ),
        (
            &*ts("DELETE FROM dbo.item WHERE item_id = @Id; DELETE FROM dbo.item WHERE item_id <> @Id;"),
            &*pg("DELETE FROM item WHERE item_id <> p_id; DELETE FROM item WHERE item_id = p_id;"),
        ),
        (
            &*ts("INSERT INTO dbo.item (item_id, qty) VALUES (@Id, @Qty); UPDATE dbo.item SET qty = @Qty WHERE item_id = @Id;"),
            &*pg("INSERT INTO item (item_id, qty) VALUES (p_id, 0); UPDATE item SET qty = p_qty WHERE item_id = p_id;"),
        ),
    ];
    for (a, b) in cases {
        for catalog in [false, true] {
            let r = run(a, T, b, P, catalog);
            assert!(witness(&r).is_none(), "{a}\n{b}\n{:#?}", r.findings);
            assert!(!r.verdict.label().starts_with("equal"), "{a}");
        }
    }
    // One changed statement, but another statement writes its table first.
    let r = run(
        &ts("DELETE FROM dbo.item WHERE qty > @Qty; DELETE FROM dbo.item WHERE item_id = @Id;"),
        T,
        &pg("DELETE FROM item WHERE qty > p_qty; DELETE FROM item WHERE item_id <> p_id;"),
        P,
        true,
    );
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
    // Or assigns a parameter it reads.
    let r = run(
        "CREATE PROCEDURE dbo.p @Id int, @Qty int AS BEGIN SET @Id = 5; DELETE FROM dbo.item WHERE item_id = @Id; END",
        T,
        "CREATE PROCEDURE p(p_id integer, p_qty integer) LANGUAGE plpgsql AS $$
         BEGIN p_id := 5; DELETE FROM item WHERE item_id <> p_id; END; $$;",
        P,
        true,
    );
    assert!(witness(&r).is_none(), "{:#?}", r.findings);
}
