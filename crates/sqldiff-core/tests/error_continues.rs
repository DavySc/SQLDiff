//! A T-SQL procedure without `SET XACT_ABORT ON` goes on after a statement
//! fails and keeps what the statements before it committed. The PostgreSQL
//! conversion gives each statement its own `BEGIN … EXCEPTION WHEN <classes>
//! THEN RAISE WARNING … END` and a COMMIT after it. Checked on SQL Server
//! 2022 and PostgreSQL 18 (`eval/xact-abort/`): a constraint violation, a
//! divide by zero, an arithmetic overflow, a string truncation and a subquery
//! that returns several rows (512) end only the statement; a conversion error (245) and THROW end the batch. The
//! behaviour model compares the error path exactly, per failing statement,
//! so a handler that catches more, fewer or other classes than T-SQL goes
//! on after, a missing COMMIT and a statement without its block all differ.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const TABLES_T: &str = "CREATE TABLE dbo.a (id int NOT NULL PRIMARY KEY);
CREATE TABLE dbo.b (id int NOT NULL PRIMARY KEY, n int NULL);
CREATE TABLE dbo.c (id int NOT NULL PRIMARY KEY);
";
const TABLES_P: &str = "CREATE TABLE a (id integer NOT NULL PRIMARY KEY);
CREATE TABLE b (id integer NOT NULL PRIMARY KEY, n integer NULL);
CREATE TABLE c (id integer NOT NULL PRIMARY KEY);
";

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options {
            catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES_T)],
            catalog_b: vec![Artifact::new("t.sql", Dialect::Postgres, TABLES_P)],
            ..Options::default()
        },
    )
}

fn equal(r: &Report) -> bool {
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

const TSQL: &str = "CREATE PROCEDURE dbo.p @x int AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO dbo.a (id) VALUES (@x);
  UPDATE dbo.b SET n = 1 WHERE id = @x;
  DELETE FROM dbo.c WHERE id = @x;
END;
";

const CLASSES: &str = "integrity_constraint_violation OR division_by_zero OR numeric_value_out_of_range OR string_data_right_truncation OR cardinality_violation";

fn block(stmt: &str, classes: &str, commit: bool) -> String {
    format!(
        "  BEGIN\n    {stmt}\n  EXCEPTION WHEN {classes} THEN\n    RAISE WARNING '%', SQLERRM;\n  END;\n{}",
        if commit { "  COMMIT;\n" } else { "" }
    )
}

fn pg(parts: &[String]) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n{}END;\n$$;\n",
        parts.concat()
    )
}

fn stmts() -> [&'static str; 3] {
    [
        "INSERT INTO a (id) VALUES (p_x);",
        "UPDATE b SET n = 1 WHERE id = p_x;",
        "DELETE FROM c WHERE id = p_x;",
    ]
}

fn wrapped(classes: &str, commit: bool) -> String {
    pg(&stmts().map(|s| block(s, classes, commit)))
}

#[test]
fn one_block_and_one_commit_per_statement_is_not_found_different() {
    // DML may raise a conversion error, which ends the T-SQL batch: whether
    // the COMMIT between two data statements is kept then rests on a profile
    // row that is not verified (`tsql.unhandled.batch-abort-conversion`), so
    // the verdict stays "not proven" rather than "equal". Nothing differs,
    // the handlers and the error path are compared, and the only open
    // questions are those two COMMITs.
    let r = run(TSQL, &wrapped(CLASSES, true));
    assert_eq!(r.verdict(), Verdict::Unknown, "{:#?}", r.findings);
    assert!(
        r.findings.iter().all(|f| f.verdict != Verdict::Different),
        "{:#?}",
        r.findings
    );
    assert_eq!(r.unknown_reasons.len(), 2, "{:#?}", r.unknown_reasons);
    assert!(
        r.unknown_reasons
            .iter()
            .all(|u| u.contains("COMMIT between data statements")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn without_the_blocks_the_procedure_stops_at_the_error() {
    let plain = pg(&stmts().map(|s| format!("  {s}\n  COMMIT;\n")));
    let r = run(TSQL, &plain);
    assert!(!equal(&r), "{:#?}", r.findings);
}

#[test]
fn without_the_commits_the_result_is_never_equal() {
    let r = run(TSQL, &wrapped(CLASSES, false));
    assert!(!equal(&r), "{:#?}", r.findings);
}

#[test]
fn a_statement_without_its_block_stops_the_procedure() {
    let s = stmts();
    let r = run(
        TSQL,
        &pg(&[
            block(s[0], CLASSES, true),
            format!("  {}\n  COMMIT;\n", s[1]),
            block(s[2], CLASSES, true),
        ]),
    );
    assert!(!equal(&r), "{:#?}", r.findings);
}

#[test]
fn a_handler_that_catches_everything_is_never_equal() {
    // T-SQL ends the batch at a conversion error.
    let r = run(TSQL, &wrapped("OTHERS", true));
    assert!(!equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_handler_that_also_catches_conversion_errors_is_never_equal() {
    let r = run(
        TSQL,
        &wrapped(&format!("{CLASSES} OR invalid_text_representation"), true),
    );
    assert!(!equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_handler_that_misses_a_class_t_sql_goes_on_after_differs() {
    // No arithmetic errors caught: T-SQL goes on after a divide by zero.
    let r = run(
        TSQL,
        &wrapped(
            "integrity_constraint_violation OR string_data_right_truncation",
            true,
        ),
    );
    assert!(!equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_handler_that_does_not_report_the_error_differs() {
    let silent = wrapped(CLASSES, true).replace("RAISE WARNING '%', SQLERRM;", "NULL;");
    let r = run(TSQL, &silent);
    assert!(!equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_handler_that_stops_with_the_error_is_a_different_routine() {
    let stops = wrapped(CLASSES, true).replace("RAISE WARNING '%', SQLERRM;", "RAISE;");
    let r = run(TSQL, &stops);
    assert!(!equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}

#[test]
fn a_changed_statement_inside_a_block_is_never_equal() {
    // The wrapper does not hide a changed statement.
    let s = stmts();
    let r = run(
        TSQL,
        &pg(&[
            block(s[0], CLASSES, true),
            block("UPDATE b SET n = 2 WHERE id = p_x;", CLASSES, true),
            block(s[2], CLASSES, true),
        ]),
    );
    assert!(!equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}
