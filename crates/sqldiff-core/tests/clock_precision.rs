//! Fraction digits of timestamps: the statement clock cast to a 6-digit
//! timestamp (ASM-CLOCK-FRACTION-6), the clock as a temporal table default,
//! and values with at most 6 fraction digits stored in datetime2(7). Each has
//! an equal pair and near misses.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    // A conversion check, which takes T-SQL's 7 digits against 6 under
    // ASM-FRACTIONAL-SECONDS.
    let _ci = sqldiff_core::equiv::compare::ci_collation_scope(true);
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|x| x.id.as_str()).collect()
}

fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert!(
        matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{a}\n{b}\n{:?}\n{:#?}\n{:#?}",
        r.verdict(),
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(a: &str, b: &str) {
    let r = run(a, b);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{a}\n{b}\n{:?}",
        ids(&r)
    );
}

/// A T-SQL procedure body against a PostgreSQL one; `@c` is a datetime2(7)
/// parameter, `@o` a datetime2(3) one, `@d` a datetime2(7) variable.
fn proc(body: &str, body_p: &str) -> (String, String) {
    (
        format!(
            "CREATE PROCEDURE dbo.p @c datetime2(7), @o datetime2(3)\nAS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  DECLARE @d datetime2(7);\n{body}\n  INSERT INTO dbo.t (id, w) VALUES (1, @d);\nEND\n"
        ),
        format!(
            "CREATE OR REPLACE PROCEDURE p(p_c timestamp, p_o timestamp(3))\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_d timestamp;\nBEGIN\n{body_p}\n  INSERT INTO t (id, w) VALUES (1, v_d);\nEND;\n$$;\n"
        ),
    )
}

fn table(col_t: &str, col_p: &str, def_t: &str, def_p: &str) -> (String, String) {
    (
        format!("CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, w {col_t} NOT NULL CONSTRAINT df_w DEFAULT ({def_t}));"),
        format!("CREATE TABLE t (id integer NOT NULL PRIMARY KEY, w {col_p} NOT NULL DEFAULT ({def_p}));"),
    )
}

/// A clock column default is not proven: inside a PostgreSQL routine
/// `statement_timestamp()` is the time of the CALL, also where it fills a
/// column default, while SQL Server fills the default at the INSERT.
#[test]
fn clock_default_of_a_datetime2_7_column_stays_unproven() {
    for (ct, cp, dt, dp) in [
        (
            "datetime2(7)",
            "timestamp",
            "sysdatetime()",
            "statement_timestamp()::timestamp",
        ),
        (
            "datetime2(7)",
            "timestamp",
            "sysdatetime()",
            "now()::timestamp",
        ),
        (
            "datetime2(7)",
            "timestamp",
            "sysdatetime()",
            "clock_timestamp()::timestamp",
        ),
    ] {
        let (a, b) = table(ct, cp, dt, dp);
        not_equal(&a, &b);
    }
}

#[test]
fn clock_cast_to_six_digits() {
    // Top-level statements: here the rule decides. The 7th digit of
    // SYSDATETIME() is dropped by the cast to datetime2(6).
    let ins = |v: &str| format!("INSERT INTO dbo.t (id, w) VALUES (1, {v});\n");
    let insp = |v: &str| format!("INSERT INTO t (id, w) VALUES (1, {v});\n");
    let r = equal(
        &ins("CAST(SYSDATETIME() AS datetime2(6))"),
        &insp("CAST(statement_timestamp() AS timestamp(6))"),
    );
    assert!(ids(&r).contains(&"ASM-CLOCK-FRACTION-6"), "{:?}", ids(&r));
    // Near misses: another precision, no cast on the T-SQL side or the
    // PostgreSQL side, another clock.
    for (a, b) in [
        (
            "CAST(SYSDATETIME() AS datetime2(6))",
            "CAST(statement_timestamp() AS timestamp(3))",
        ),
        (
            "CAST(SYSDATETIME() AS datetime2(7))",
            "CAST(statement_timestamp() AS timestamp(6))",
        ),
        (
            "CAST(SYSDATETIME() AS datetime2(6))",
            "CAST(now() AS timestamp(6))",
        ),
        (
            "CAST(SYSDATETIME() AS datetime2(6))",
            "CAST(clock_timestamp() AS timestamp(6))",
        ),
        (
            "CAST(GETDATE() AS datetime2(6))",
            "CAST(statement_timestamp() AS timestamp(6))",
        ),
    ] {
        not_equal(&ins(a), &insp(b));
    }
    // Inside a routine PostgreSQL's statement_timestamp() is the time of the
    // CALL, so an assignment from SYSDATETIME() is not shown equal to it.
    let (a, b) = proc(
        "  SET @d = SYSDATETIME();",
        "  v_d := statement_timestamp()::timestamp;",
    );
    not_equal(&a, &b);
    let (a, b) = proc(
        "  SET @d = CAST(SYSDATETIME() AS datetime2(6));",
        "  v_d := CAST(statement_timestamp() AS timestamp(6));",
    );
    not_equal(&a, &b);
}

#[test]
fn six_digit_values_stored_in_datetime2_7() {
    // A date and a datetime2(3) have no 7th digit: datetime2(7) and
    // timestamp(6) hold them alike.
    let (a, b) = proc(
        "  SET @d = CAST(@c AS date);",
        "  v_d := CAST(p_c AS date);",
    );
    equal(&a, &b);
    let (a, b) = proc("  SET @d = @o;", "  v_d := p_o;");
    equal(&a, &b);
    // Another day, and a different value.
    let (a, b) = proc(
        "  SET @d = CAST(@c AS date);",
        "  v_d := CAST(p_o AS date);",
    );
    not_equal(&a, &b);
    let (a, b) = proc("  SET @d = @o;", "  v_d := p_c;");
    not_equal(&a, &b);
}
