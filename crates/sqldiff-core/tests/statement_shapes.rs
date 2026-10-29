//! Statement shapes the engine reads since burndown S: PostgreSQL
//! `make_interval` named arguments (DATEADD), a T-SQL SELECT assigning
//! several variables, a message that is an expression, and INSERT without a
//! column list. Each has an equal pair and near misses.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const CATALOG_T: &str =
    "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL, w nvarchar(20) NULL);";
const CATALOG_P: &str =
    "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL, w varchar(20) NULL);";

/// A T-SQL procedure with `body`, against a PostgreSQL one with `body_p`.
/// Both declare a timestamp `@d` (datetime2(6)), a date `@dt`,
/// an int `@n`, a bigint `@b` and string `@e` (nvarchar(100)).
fn run(body: &str, body_p: &str) -> Report {
    let a = format!(
        "CREATE PROCEDURE dbo.p @id int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  DECLARE @d datetime2(6);\n  DECLARE @dt date;\n  DECLARE @n int = 3;\n  DECLARE @b bigint = 4;\n  DECLARE @e nvarchar(100) = N'e';\n{body}\nEND\n"
    );
    let b = format!(
        "CREATE OR REPLACE PROCEDURE p(p_id integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_d timestamp(6);\n  v_dt date;\n  v_n integer;\n  v_b bigint;\n  v_e varchar(100);\nBEGIN\n  v_n := 3;\n  v_b := 4;\n  v_e := 'e';\n{body_p}\nEND;\n$$;\n"
    );
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", Dialect::Tsql, CATALOG_T)],
        catalog_b: vec![Artifact::new("cb.sql", Dialect::Postgres, CATALOG_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, &a),
        &Artifact::new("b.sql", Dialect::Postgres, &b),
        &opts,
    )
}

/// The verdict and reasons of the statement under test only: the
/// declarations around it are the same in every case.
fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|x| x.id.as_str()).collect()
}

fn equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert!(
        matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{body}\n{body_p}\n{:?}\n{:#?}\n{:#?}",
        r.verdict(),
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{body}\n{body_p}\n{:?}",
        ids(&r)
    );
    r
}

#[test]
fn baseline_is_equal() {
    equal("", "");
}

#[test]
fn dateadd_against_make_interval() {
    let r = equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(hours => 7));",
    );
    assert!(ids(&r).contains(&"ASM-DATEADD-RANGE"), "{:?}", ids(&r));
    equal(
        "  SET @d = DATEADD(second, @n, @d);",
        "  v_d := (v_d + make_interval(secs => v_n));",
    );
    equal(
        "  SET @d = DATEADD(mi, -5, @d);",
        "  v_d := (v_d + make_interval(mins => -5));",
    );
    // Positional and other zero amounts are the same call.
    equal(
        "  SET @d = DATEADD(day, @n, @d);",
        "  v_d := (v_d + make_interval(0, 0, 0, v_n));",
    );
    equal(
        "  SET @d = DATEADD(day, @n, @d);",
        "  v_d := (v_d + make_interval(days => v_n, secs => 0.0));",
    );
    let r = equal(
        "  SET @d = DATEADD(year, 1, @d);",
        "  v_d := (v_d + make_interval(years => 1));",
    );
    assert!(ids(&r).contains(&"ASM-DATEADD-YEAR-END"), "{:?}", ids(&r));
    // Another amount, field or operand.
    not_equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(hours => 8));",
    );
    not_equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(mins => 7));",
    );
    not_equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(hours => 7, mins => 1));",
    );
    not_equal(
        "  SET @d = DATEADD(day, @n, @d);",
        "  v_d := (v_d + make_interval(0, 0, 0, 0, v_n));",
    );
    // A name make_interval does not have, a repeated one, and a positional
    // argument after a named one fail in PostgreSQL.
    for bad in [
        "make_interval(hour => 7)",
        "make_interval(hours => 7, hours => 7)",
        "make_interval(hours => 7, 0)",
    ] {
        not_equal(
            "  SET @d = DATEADD(hour, 7, @d);",
            &format!("  v_d := (v_d + {bad});"),
        );
    }
    // T-SQL truncates a bigint or decimal amount to int (or fails), PostgreSQL
    // keeps a fraction of seconds.
    not_equal(
        "  SET @d = DATEADD(second, @b, @d);",
        "  v_d := (v_d + make_interval(secs => v_b));",
    );
}

#[test]
fn dateadd_on_a_date() {
    equal("  SET @dt = DATEADD(day, 1, @dt);", "  v_dt := (v_dt + 1);");
    equal(
        "  SET @dt = DATEADD(dd, @n, @dt);",
        "  v_dt := (v_dt + v_n);",
    );
    equal(
        "  SET @dt = DATEADD(week, 2, @dt);",
        "  v_dt := (v_dt + 14);",
    );
    equal(
        "  SET @dt = DATEADD(month, @n, @dt);",
        "  v_dt := (v_dt + make_interval(months => v_n))::date;",
    );
    not_equal("  SET @dt = DATEADD(day, 1, @dt);", "  v_dt := (v_dt + 2);");
    not_equal(
        "  SET @dt = DATEADD(week, 2, @dt);",
        "  v_dt := (v_dt + 2);",
    );
    not_equal(
        "  SET @dt = DATEADD(dd, @b, @dt);",
        "  v_dt := (v_dt + v_b);",
    );
    not_equal(
        "  SET @dt = DATEADD(month, @n, @dt);",
        "  v_dt := (v_dt + make_interval(days => v_n))::date;",
    );
}
