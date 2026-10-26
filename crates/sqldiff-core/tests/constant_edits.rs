//! Changed constants and arithmetic: a literal moved by one, `+` turned
//! into `-`, `>` turned into `>=`, in places where the plain row search had
//! nothing to tell the versions apart (intervals, RAND(), table and view
//! definitions, string arithmetic). Each different verdict needs a concrete
//! input; edits that keep the meaning must not be called different.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const TSQL_DDL: &str = "
CREATE TABLE dbo.item (item_id int NOT NULL PRIMARY KEY, name nvarchar(50) NOT NULL, price decimal(12,2) NOT NULL);
";
const PG_DDL: &str = "
CREATE TABLE item (item_id integer PRIMARY KEY, name varchar(50) NOT NULL, created_at timestamp NOT NULL);
";

fn run(a: &str, b: &str, d: Dialect) -> Report {
    let ddl = if d == T { TSQL_DDL } else { PG_DDL };
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", d, ddl)],
        catalog_b: vec![Artifact::new("catalog.sql", d, ddl)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", d, a),
        &Artifact::new("b.sql", d, b),
        &opts,
    )
}

fn different(a: &str, b: &str, d: Dialect) -> String {
    let r = run(a, b, d);
    assert_eq!(r.verdict, Verdict::Different, "{:#?}", r.findings);
    r.findings
        .iter()
        .find(|f| f.verdict == Verdict::Different)
        .map(|f| f.witness.clone())
        .unwrap()
}

fn not_different(a: &str, b: &str, d: Dialect) {
    let r = run(a, b, d);
    assert_ne!(r.verdict, Verdict::Different, "{:#?}", r.findings);
}

fn pg_since(span: &str) -> String {
    format!(
        "CREATE FUNCTION recent(p_at timestamp) RETURNS SETOF integer LANGUAGE sql AS $$
           SELECT item_id FROM item WHERE created_at > p_at - interval '{span}' ORDER BY item_id;
         $$;"
    )
}

#[test]
fn changed_interval_has_a_witness() {
    different(&pg_since("1 day"), &pg_since("2 days"), P);
}

#[test]
fn interval_spelled_differently_is_not_different() {
    not_different(&pg_since("1 day"), &pg_since("1 days"), P);
    not_different(&pg_since("1 day"), &pg_since("-1 day ago"), P);
    not_different(&pg_since("2 days"), &pg_since("@ 2 days"), P);
}

fn tsql_rand(e: &str) -> String {
    format!("CREATE PROCEDURE dbo.pick AS BEGIN SELECT {e} AS r; END")
}

#[test]
fn widened_rand_range_is_different() {
    different(
        &tsql_rand("FLOOR(RAND() * 10)"),
        &tsql_rand("FLOOR(RAND() * 20)"),
        T,
    );
}

#[test]
fn rand_draws_are_never_compared_value_by_value() {
    // The same range drawn twice may come out differently: not a witness.
    not_different(&tsql_rand("RAND() * 10"), &tsql_rand("10 * RAND()"), T);
    not_different(
        &tsql_rand("FLOOR(RAND() * 10)"),
        &tsql_rand("FLOOR(10 * RAND())"),
        T,
    );
}

#[test]
fn shifted_rand_range_is_different() {
    // A returns 0 one time in ten; B never does.
    let w = different(
        &tsql_rand("FLOOR(RAND() * 10)"),
        &tsql_rand("FLOOR(RAND() * 10) + 1"),
        T,
    );
    assert!(w.contains("random value between 0 and 10"), "{w}");
}

#[test]
fn changed_check_constraint_has_a_row() {
    let t = |c: &str| {
        format!("CREATE TABLE dbo.stock (qty int NOT NULL, CONSTRAINT ck_qty CHECK (qty > {c}));")
    };
    let w = different(&t("0"), &t("1"), T);
    assert!(w.contains("qty"), "{w}");
}

#[test]
fn shorter_string_column_rejects_a_value() {
    let t =
        |n: u32| format!("CREATE TABLE dbo.tag (tag_id int NOT NULL, label nvarchar({n}) NULL);");
    let w = different(&t(20), &t(19), T);
    assert!(w.contains("20 characters"), "{w}");
}

#[test]
fn changed_constant_default_is_different() {
    let t = |v: &str| {
        format!("CREATE TABLE dbo.job (job_id int NOT NULL, tries int NOT NULL DEFAULT (({v})));")
    };
    different(&t("3"), &t("4"), T);
    not_different(&t("3"), &t("(3)"), T);
}

#[test]
fn changed_view_filter_is_different() {
    let v = |n: &str| {
        format!("CREATE VIEW dbo.dear AS SELECT item_id FROM dbo.item WHERE price > {n};")
    };
    different(&v("10"), &v("11"), T);
    different(&v("10"), &v("10 OR price = 10"), T);
}

#[test]
fn top_assignment_is_a_variable_write() {
    // `SELECT TOP 1 @v = ...` assigns @v; the change after it is observable.
    let p = |k: u32| {
        format!(
            "CREATE PROCEDURE dbo.first_price AS
             BEGIN
               DECLARE @p decimal(12,2);
               SELECT TOP (1) @p = price FROM dbo.item ORDER BY item_id;
               SELECT @p + {k} AS p;
             END"
        )
    };
    let r = run(&p(1), &p(2), T);
    assert_ne!(r.verdict, Verdict::Equal, "{:#?}", r.findings);
}

#[test]
fn arithmetic_on_a_text_literal_fails_when_compiled() {
    let p = |op: &str| {
        format!(
            "CREATE PROCEDURE dbo.label AS
             BEGIN
               SELECT N'[' {op} name AS l FROM dbo.item;
             END"
        )
    };
    let w = different(&p("+"), &p("-"), T);
    assert!(w.contains("fails"), "{w}");
}

const TSQL_TYPES: &str = "
CREATE TABLE dbo.P (Code VARCHAR(10) NOT NULL PRIMARY KEY);
CREATE TABLE dbo.T (Id INT NOT NULL PRIMARY KEY, Q INT NULL, SD SMALLDATETIME NULL, DT DATETIME NULL);
";

fn run_types(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", T, TSQL_TYPES)],
        catalog_b: vec![Artifact::new("catalog.sql", T, TSQL_TYPES)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", T, b),
        &opts,
    )
}

#[test]
fn times_a_column_rounds_alike_are_not_different() {
    // smalldatetime stores whole minutes, datetime 1/300 s: both literals
    // convert to the same stored value.
    for (col, x, y) in [
        ("SD", "2020-01-01 10:00:20", "2020-01-01 10:00:00"),
        ("DT", "2020-01-01 10:00:00.001", "2020-01-01 10:00:00"),
        ("DT", "2020-01-01 23:59:59.999", "2020-01-02"),
    ] {
        for op in ["=", ">"] {
            let u = |v: &str| format!("UPDATE dbo.T SET Q = 1 WHERE {col} {op} '{v}';");
            let r = run_types(&u(x), &u(y));
            assert_ne!(
                r.verdict,
                Verdict::Different,
                "{col} {op}: {:#?}",
                r.findings
            );
        }
    }
    let u = |op: &str| format!("UPDATE dbo.T SET Q = 1 WHERE DT {op} '2020-01-01 10:00';");
    assert_eq!(run_types(&u(">"), &u(">=")).verdict, Verdict::Different);
}

#[test]
fn check_witness_fits_the_column_type() {
    // No tinyint holds 300: both versions reject that row before the CHECK.
    let t =
        |n: u32| format!("CREATE TABLE dbo.X (Id int NOT NULL, Q tinyint NULL, CHECK (Q < {n}));");
    not_different(&t(300), &t(400), T);
}

#[test]
fn foreign_key_bounds_a_column_length() {
    let inline = |n: u32| {
        format!("CREATE TABLE dbo.C (Id int NOT NULL, Code varchar({n}) NOT NULL REFERENCES dbo.P (Code));")
    };
    let r = run_types(&inline(10), &inline(20));
    assert_ne!(r.verdict, Verdict::Different, "{:#?}", r.findings);
    let named = |n: u32| {
        format!("CREATE TABLE dbo.C (Id int NOT NULL, Code varchar({n}) NOT NULL, CONSTRAINT FK_C FOREIGN KEY (Code) REFERENCES dbo.P (Code));")
    };
    let r = run_types(&named(10), &named(20));
    assert_ne!(r.verdict, Verdict::Different, "{:#?}", r.findings);
}

#[test]
fn a_variable_holding_a_random_draw_is_not_an_input() {
    // RAND() is never NULL, and @r - @r is 0 whatever it drew.
    for set in [
        "DECLARE @r float = RAND();",
        "DECLARE @r float; SET @r = RAND();",
    ] {
        let p = |e: &str| format!("CREATE PROCEDURE dbo.r AS BEGIN {set} SELECT {e} AS x; END");
        not_different(&p("@r - @r"), &p("0e0"), T);
    }
}

#[test]
fn foreign_key_on_another_column_keeps_a_default_witness() {
    let t = |v: u32| {
        format!("CREATE TABLE dbo.C (Code varchar(10) NOT NULL REFERENCES dbo.P (Code), tries int NOT NULL DEFAULT (({v})));")
    };
    let r = run_types(&t(3), &t(4));
    assert_eq!(r.verdict, Verdict::Different, "{:#?}", r.findings);
}
