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
