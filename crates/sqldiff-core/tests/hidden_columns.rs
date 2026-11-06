//! T-SQL `GENERATED ALWAYS AS ROW START|END HIDDEN` columns: `SELECT *`,
//! `t.*` and an INSERT without a column list leave them out; PostgreSQL has
//! no hidden columns, so its `*` returns them. The catalog records the
//! flag and a star is compared as the columns each side returns.

use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};
use std::sync::Arc;

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

/// k, hidden f and e, v.
const TSQL_TABLE: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));";
/// Only f is hidden.
const TSQL_ONE: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));";
/// The conversion with the period columns kept (all four columns).
const PG_FULL: &str = "CREATE TABLE t (k integer NOT NULL PRIMARY KEY, f timestamp NOT NULL DEFAULT '-infinity', e timestamp NOT NULL DEFAULT '-infinity', v integer NULL);";
/// The conversion without the period columns (--temporal-historyless).
const PG_PLAIN: &str = "CREATE TABLE t (k integer NOT NULL PRIMARY KEY, v integer NULL);";
const OTHER_T: &str = "CREATE TABLE dbo.u (k INT NOT NULL PRIMARY KEY, w INT NULL);";
const OTHER_P: &str = "CREATE TABLE u (k integer NOT NULL PRIMARY KEY, w integer NULL);";

fn catalog(dialect: Dialect, files: &[&str]) -> Arc<Catalog> {
    let mut cat = Catalog::default();
    for f in files {
        let root = sqldiff_syntax::parse(f, dialect).syntax();
        ddl::load_tree(&mut cat, &root, dialect);
    }
    Arc::new(cat)
}

fn run(tsql_tables: &[&str], pg_tables: &[&str], a: &str, b: &str) -> Report {
    let opts = Options {
        facts_a: vec![catalog(T, tsql_tables)],
        facts_b: vec![catalog(P, pg_tables)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &opts,
    )
}

fn proc_t(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n  {body}\nEND")
}

fn proc_p(body: &str) -> String {
    format!(
        "CREATE PROCEDURE p(INOUT result_set_1 refcursor DEFAULT NULL)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  {body}\nEND;\n$$;"
    )
}

/// A PostgreSQL procedure without a result set.
fn proc_q(body: &str) -> String {
    format!("CREATE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  {body}\nEND;\n$$;")
}

fn proven(r: &Report) -> bool {
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
        && r.unknown_reasons.is_empty()
}

#[test]
fn the_catalog_records_hidden_columns() {
    let cat = catalog(T, &[TSQL_ONE]);
    let t = &cat.tables[0];
    let hidden: Vec<(&str, bool)> = t
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.hidden))
        .collect();
    assert_eq!(
        hidden,
        [("k", false), ("f", true), ("e", false), ("v", false)]
    );
    // PostgreSQL and a column that merely mentions the word are not hidden.
    let cat = catalog(T, &["CREATE TABLE dbo.h (hidden INT DEFAULT 1, x INT)"]);
    assert!(cat.tables[0].columns.iter().all(|c| !c.hidden));
}

#[test]
fn a_star_over_hidden_columns_is_not_the_stars_of_a_table_that_keeps_them() {
    let a = proc_t("SELECT * FROM dbo.t;");
    let b = proc_p("OPEN result_set_1 FOR SELECT * FROM t;");
    let r = run(&[TSQL_TABLE], &[PG_FULL], &a, &b);
    assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_star_equals_the_visible_columns_of_a_table_without_the_hidden_ones() {
    let a = proc_t("SELECT * FROM dbo.t;");
    let b = proc_p("OPEN result_set_1 FOR SELECT * FROM t;");
    let r = run(&[TSQL_TABLE], &[PG_PLAIN], &a, &b);
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_star_equals_the_explicit_visible_columns() {
    let a = proc_t("SELECT * FROM dbo.t;");
    let b = proc_p("OPEN result_set_1 FOR SELECT k, v FROM t;");
    let r = run(&[TSQL_TABLE], &[PG_FULL], &a, &b);
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
    // The hidden columns named are not what T-SQL's star returns.
    let b = proc_p("OPEN result_set_1 FOR SELECT k, f, e, v FROM t;");
    let r = run(&[TSQL_TABLE], &[PG_FULL], &a, &b);
    assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_qualified_star_leaves_the_hidden_columns_out_too() {
    let a = proc_t("SELECT t.* FROM dbo.t;");
    let r = run(
        &[TSQL_TABLE],
        &[PG_FULL],
        &a,
        &proc_p("OPEN result_set_1 FOR SELECT t.* FROM t;"),
    );
    assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
    let r = run(
        &[TSQL_TABLE],
        &[PG_FULL],
        &a,
        &proc_p("OPEN result_set_1 FOR SELECT t.k, t.v FROM t;"),
    );
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
    // An alias, and a star beside another table's columns.
    let a = proc_t("SELECT x.*, u.w FROM dbo.t x JOIN dbo.u ON u.k = x.k;");
    let b = proc_p("OPEN result_set_1 FOR SELECT x.k, x.v, u.w FROM t x JOIN u ON u.k = x.k;");
    let r = run(&[TSQL_TABLE, OTHER_T], &[PG_FULL, OTHER_P], &a, &b);
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_star_over_two_tables_lists_each_tables_columns() {
    let a = proc_t("SELECT * FROM dbo.t JOIN dbo.u ON u.k = t.k;");
    let b = proc_p("OPEN result_set_1 FOR SELECT t.k, t.v, u.k, u.w FROM t JOIN u ON u.k = t.k;");
    let r = run(&[TSQL_TABLE, OTHER_T], &[PG_FULL, OTHER_P], &a, &b);
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
    let b = proc_p("OPEN result_set_1 FOR SELECT * FROM t JOIN u ON u.k = t.k;");
    let r = run(&[TSQL_TABLE, OTHER_T], &[PG_FULL, OTHER_P], &a, &b);
    assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn only_one_hidden_column_is_left_out() {
    let a = proc_t("SELECT * FROM dbo.t;");
    let r = run(
        &[TSQL_ONE],
        &[PG_FULL],
        &a,
        &proc_p("OPEN result_set_1 FOR SELECT * FROM t;"),
    );
    assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
    let r = run(
        &[TSQL_ONE],
        &[PG_FULL],
        &a,
        &proc_p("OPEN result_set_1 FOR SELECT k, e, v FROM t;"),
    );
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn insert_select_star_reads_the_visible_columns() {
    let a = proc_t("INSERT INTO dbo.u (k, w) SELECT * FROM dbo.t;");
    let b = proc_q("INSERT INTO u (k, w) SELECT * FROM t;");
    let r = run(&[TSQL_TABLE, OTHER_T], &[PG_FULL, OTHER_P], &a, &b);
    assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
    let r = run(&[TSQL_TABLE, OTHER_T], &[PG_PLAIN, OTHER_P], &a, &b);
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}

#[test]
fn an_insert_without_a_column_list_into_hidden_columns_is_not_compared() {
    let a = proc_t("INSERT INTO dbo.t VALUES (1, 2);");
    for pg in [PG_FULL, PG_PLAIN] {
        let b = proc_q("INSERT INTO t VALUES (1, 2);");
        let r = run(&[TSQL_TABLE], &[pg], &a, &b);
        assert!(!proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
        let why = format!("{:?}{:?}", r.findings, r.unknown_reasons);
        assert!(why.contains("INSERT without a column list"), "{why}");
    }
}

#[test]
fn a_comparison_without_hidden_columns_keeps_its_stars() {
    let tsql = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, v INT NULL);";
    let a = proc_t("SELECT * FROM dbo.t;");
    let b = proc_p("OPEN result_set_1 FOR SELECT * FROM t;");
    let r = run(&[tsql], &[PG_PLAIN], &a, &b);
    assert!(proven(&r), "{:#?}\n{:?}", r.findings, r.unknown_reasons);
}
