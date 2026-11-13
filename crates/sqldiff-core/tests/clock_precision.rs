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

#[test]
fn clock_default_of_a_datetime2_7_column() {
    let (a, b) = table(
        "datetime2(7)",
        "timestamp",
        "sysdatetime()",
        "statement_timestamp()::timestamp",
    );
    let r = equal(&a, &b);
    assert!(ids(&r).contains(&"ASM-CLOCK-FRACTION-6"), "{:?}", ids(&r));
    // Another clock, a clock cast to another precision, another column type.
    for (ct, cp, dt, dp) in [
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
        (
            "datetime2(7)",
            "timestamp",
            "getdate()",
            "statement_timestamp()::timestamp",
        ),
        (
            "datetime2(7)",
            "timestamp(3)",
            "sysdatetime()",
            "statement_timestamp()::timestamp",
        ),
        (
            "datetime2(3)",
            "timestamp(3)",
            "sysdatetime()",
            "statement_timestamp()::timestamp",
        ),
        (
            "datetime2(7)",
            "timestamp",
            "sysdatetime()",
            "statement_timestamp()::timestamp(3)",
        ),
    ] {
        let (a, b) = table(ct, cp, dt, dp);
        not_equal(&a, &b);
    }
}

#[test]
fn clock_cast_to_six_digits() {
    let (a, b) = proc(
        "  SET @d = SYSDATETIME();",
        "  v_d := statement_timestamp()::timestamp;",
    );
    // Inside a routine PostgreSQL's statement_timestamp() is the time of the
    // CALL, not of the statement, so main no longer shows an assignment from
    // SYSDATETIME() equal to it: the clock cast alone (ASM-CLOCK-FRACTION-6)
    // does not make the pair equal there.
    not_equal(&a, &b);
    let (a, b) = proc(
        "  SET @d = CAST(SYSDATETIME() AS datetime2(6));",
        "  v_d := CAST(statement_timestamp() AS timestamp(6));",
    );
    not_equal(&a, &b);
    // The transaction clock, the real clock and datetime's GETDATE are other values.
    for p in [
        "  v_d := now()::timestamp;",
        "  v_d := clock_timestamp()::timestamp;",
        "  v_d := statement_timestamp()::timestamp(3);",
    ] {
        let (a, b) = proc("  SET @d = SYSDATETIME();", p);
        not_equal(&a, &b);
    }
    let (a, b) = proc(
        "  SET @d = GETDATE();",
        "  v_d := statement_timestamp()::timestamp;",
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
