//! DATEADD to PostgreSQL and Db2 for i: the result keeps the type of the
//! date argument, the number is truncated to an integer as T-SQL does, and
//! every form without an exact equivalent is refused with its reason.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

const TABLES: &str = "CREATE TABLE t (d date, tm time, t0 time(0), dt datetime, sd smalldatetime, \
d2 datetime2(3), d7 datetime2, dto datetimeoffset, n int, b bigint, f decimal(5,2), s varchar(10));
GO
CREATE TABLE u (d datetime2(3), k int);
GO
";

fn run(expr: &str, to: Dialect) -> (Conversion, String) {
    let src = format!("{TABLES}SELECT {expr} AS r FROM t;\n");
    let c = convert(&src, Dialect::Tsql, to).unwrap();
    let line = c
        .output
        .lines()
        .rev()
        .find(|l| l.contains(" AS r"))
        .unwrap_or_default()
        .to_string();
    (c, line)
}

/// The last unit is the SELECT.
fn last_status(c: &Conversion) -> Status {
    c.units.last().unwrap().status
}

/// The status the rules alone gave the SELECT: the engine does not model
/// DATEADD or the targets' interval arithmetic yet, so it cannot confirm
/// these conversions and the unit itself is not proven.
fn rule_status(c: &Conversion) -> Status {
    let u = c.units.last().unwrap();
    assert_eq!(u.checked, Status::Unproven, "{:?}", u.notes);
    u.notes
        .iter()
        .filter(|n| n.status != Status::Unproven)
        .map(|n| n.status)
        .max()
        .unwrap_or(Status::Converted)
}

fn notes(c: &Conversion) -> Vec<String> {
    let u = c.units.last().unwrap();
    u.notes
        .iter()
        .map(|n| format!("{} {}", n.rule, n.text))
        .collect()
}

/// Converts on both targets to exactly these expressions.
#[track_caller]
fn both(expr: &str, pg: &str, db2: &str) {
    for (to, want) in [(Dialect::Postgres, pg), (Dialect::Db2i, db2)] {
        let (c, line) = run(expr, to);
        assert_eq!(
            line,
            format!("SELECT {want} AS r FROM t;"),
            "{expr} → {to:?}:\n{}",
            c.output
        );
        assert_ne!(last_status(&c), Status::NotConverted, "{}", c.output);
    }
}

/// Refused on the target with a reason containing `why`.
#[track_caller]
fn refused(expr: &str, to: Dialect, why: &str) {
    let (c, _) = run(expr, to);
    assert_eq!(
        last_status(&c),
        Status::NotConverted,
        "{expr} → {to:?}:\n{}",
        c.output
    );
    let n = notes(&c);
    assert!(
        n.iter().any(|t| t.contains("DATEADD: ") && t.contains(why)),
        "{expr} → {to:?}: no reason with {why:?} in {n:?}"
    );
}

#[track_caller]
fn refused_both(expr: &str, why: &str) {
    refused(expr, Dialect::Postgres, why);
    refused(expr, Dialect::Db2i, why);
}

#[test]
fn days_on_a_date_stay_a_date() {
    both("DATEADD(day, 5, d)", "(d + 5)", "(d + 5 DAYS)");
    both("DATEADD(dd, -5, d)", "(d - 5)", "(d - 5 DAYS)");
    both("DATEADD(d, n, d)", "(d + n)", "(d + n DAYS)");
    // dayofyear and weekday add days too.
    for part in ["dayofyear", "dy", "y", "weekday", "dw", "w"] {
        both(&format!("DATEADD({part}, 3, d)"), "(d + 3)", "(d + 3 DAYS)");
    }
    both("DATEADD(week, 2, d)", "(d + 14)", "(d + 14 DAYS)");
    both("DATEADD(wk, -1, d)", "(d - 7)", "(d - 7 DAYS)");
    both("DATEADD(ww, n, d)", "(d + 7 * n)", "(d + (7 * n) DAYS)");
}

#[test]
fn months_on_a_date_are_cast_back_to_date_in_postgres() {
    both(
        "DATEADD(month, n, d)",
        "(d + make_interval(months => n))::date",
        "(d + n MONTHS)",
    );
    both(
        "DATEADD(mm, 1, d)",
        "(d + make_interval(months => 1))::date",
        "(d + 1 MONTHS)",
    );
    both(
        "DATEADD(qq, 1, d)",
        "(d + make_interval(months => 3))::date",
        "(d + 3 MONTHS)",
    );
    both(
        "DATEADD(q, n, d)",
        "(d + make_interval(months => 3 * n))::date",
        "(d + (3 * n) MONTHS)",
    );
    both(
        "DATEADD(yyyy, -2, d)",
        "(d + make_interval(years => -2))::date",
        "(d - 2 YEARS)",
    );
}

#[test]
fn timestamps_and_times() {
    both(
        "DATEADD(hour, 1, dt)",
        "(dt + make_interval(hours => 1))",
        "(dt + 1 HOURS)",
    );
    both(
        "DATEADD(mi, n, sd)",
        "(sd + make_interval(mins => n))",
        "(sd + n MINUTES)",
    );
    both(
        "DATEADD(ss, 30, d2)",
        "(d2 + make_interval(secs => 30))",
        "(d2 + 30 SECONDS)",
    );
    both(
        "DATEADD(day, 1, d2)",
        "(d2 + make_interval(days => 1))",
        "(d2 + 1 DAYS)",
    );
    both(
        "DATEADD(month, 1, dt)",
        "(dt + make_interval(months => 1))",
        "(dt + 1 MONTHS)",
    );
    both(
        "DATEADD(ms, 5, d2)",
        "(d2 + 5 * interval '1 millisecond')",
        "(d2 + 5000 MICROSECONDS)",
    );
    both(
        "DATEADD(ms, n, d2)",
        "(d2 + n * interval '1 millisecond')",
        "(d2 + (CAST(n AS BIGINT) * 1000) MICROSECONDS)",
    );
    both(
        "DATEADD(mcs, 5, d7)",
        "(d7 + 5 * interval '1 microsecond')",
        "(d7 + 5 MICROSECONDS)",
    );
    both(
        "DATEADD(hour, 2, tm)",
        "(tm + make_interval(hours => 2))",
        "(tm + 2 HOURS)",
    );
    let (c, _) = run("DATEADD(hour, 2, tm)", Dialect::Db2i);
    assert!(notes(&c).iter().any(|n| n.starts_with("ASM-DB2-TIME-WRAP")));
}

#[test]
fn the_number_is_truncated_to_an_integer() {
    both(
        "DATEADD(day, f, d)",
        "(d + trunc(f)::integer)",
        "(d + (CAST(f AS INTEGER)) DAYS)",
    );
    both(
        "DATEADD(day, 1.9, d)",
        "(d + trunc(1.9)::integer)",
        "(d + (CAST(1.9 AS INTEGER)) DAYS)",
    );
    both(
        "DATEADD(day, b, d)",
        "(d + (b)::integer)",
        "(d + (CAST(b AS INTEGER)) DAYS)",
    );
    both(
        "DATEADD(month, f, dt)",
        "(dt + make_interval(months => trunc(f)::integer))",
        "(dt + (CAST(f AS INTEGER)) MONTHS)",
    );
    let (c, _) = run("DATEADD(day, f, d)", Dialect::Db2i);
    assert!(notes(&c)
        .iter()
        .any(|n| n.starts_with("ASM-DB2-DATEADD-NUMBER")));
    refused_both("DATEADD(day, s, d)", "the number is a string");
}

#[test]
fn the_date_type_comes_from_casts_nested_calls_and_aliases() {
    both(
        "DATEADD(day, 1, CAST(s AS date))",
        "(CAST(s AS date) + 1)",
        "(CAST(s AS DATE) + 1 DAYS)",
    );
    both(
        "DATEADD(month, 1, DATEADD(day, 1, d))",
        "((d + 1) + make_interval(months => 1))::date",
        "((d + 1 DAYS) + 1 MONTHS)",
    );
    let src = format!(
        "{TABLES}SELECT DATEADD(day, 1, x.d) AS a, DATEADD(day, 1, y.d) AS b FROM t x JOIN u y ON y.k = x.n;\n"
    );
    let c = convert(&src, Dialect::Tsql, Dialect::Postgres).unwrap();
    assert!(
        c.output
            .contains("(x.d + 1) AS a, (y.d + make_interval(days => 1)) AS b"),
        "{}",
        c.output
    );
    // A bare `d` could be t.d (date) or u.d (datetime2): not guessed.
    let src = format!("{TABLES}SELECT DATEADD(day, 1, d) AS r FROM t x JOIN u y ON y.k = x.n;\n");
    let c = convert(&src, Dialect::Tsql, Dialect::Postgres).unwrap();
    assert_eq!(
        c.units.last().unwrap().status,
        Status::NotConverted,
        "{}",
        c.output
    );
}

#[test]
fn variables_carry_their_declared_type() {
    let src = "CREATE PROCEDURE dbo.p @n int, @d date, @w datetime2(3) AS\nBEGIN\n  DECLARE @x date = DATEADD(day, @n, @d);\n  SET @w = DATEADD(month, -@n, @w);\n  SET @w = DATEADD(qq, 1, @w);\nEND\n";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    let o = &c.output;
    assert!(o.contains("v_x := (p_d + p_n);"), "{o}");
    assert!(
        o.contains("p_w := (p_w + make_interval(months => -p_n));"),
        "{o}"
    );
    assert!(
        o.contains("p_w := (p_w + make_interval(months => 3));"),
        "{o}"
    );
    let c = convert(src, Dialect::Tsql, Dialect::Db2i).unwrap();
    let o = &c.output;
    assert!(o.contains("SET v_x = (p_d + p_n DAYS);"), "{o}");
    assert!(o.contains("SET p_w = (p_w + (-p_n) MONTHS);"), "{o}");
    assert_eq!(c.counts.not_converted, 0, "{o}");
}

#[test]
fn every_conversion_states_the_range_assumption() {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let (c, _) = run("DATEADD(day, 5, d)", to);
        assert_eq!(rule_status(&c), Status::Assumptions);
        assert!(notes(&c).iter().any(|n| n.starts_with("ASM-DATEADD-RANGE")));
        // A time wraps around midnight instead: no range to leave.
        let (c, _) = run("DATEADD(hour, 5, tm)", to);
        assert!(!notes(&c).iter().any(|n| n.starts_with("ASM-DATEADD-RANGE")));
    }
    let (c, _) = run("DATEADD(month, 1, d)", Dialect::Db2i);
    assert!(notes(&c).iter().any(|n| n.starts_with("ASM-DB2-MONTH-END")));
    // What Microsoft does not document is stated, not assumed silently.
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let (c, _) = run("DATEADD(year, 1, d)", to);
        assert!(notes(&c)
            .iter()
            .any(|n| n.starts_with("ASM-DATEADD-YEAR-END")));
        let (c, _) = run("DATEADD(month, 1, d)", to);
        assert!(!notes(&c)
            .iter()
            .any(|n| n.starts_with("ASM-DATEADD-YEAR-END")));
        let (c, _) = run("DATEADD(hour, 1, tm)", to);
        assert_eq!(rule_status(&c), Status::Assumptions);
        assert!(notes(&c)
            .iter()
            .any(|n| n.starts_with("ASM-DATEADD-TIME-WRAP")));
    }
}

#[test]
fn what_t_sql_rejects_is_refused() {
    refused_both("DATEADD(hour, 1, d)", "T-SQL rejects");
    refused_both("DATEADD(day, 1, tm)", "T-SQL rejects");
    refused_both("DATEADD(mcs, 1, dt)", "T-SQL rejects");
    refused_both("DATEADD(fortnight, 1, d)", "not a datepart");
}

#[test]
fn roundings_without_an_equivalent_are_refused() {
    refused_both("DATEADD(ns, 100, d7)", "nanoseconds");
    refused_both("DATEADD(second, 30, sd)", "smalldatetime");
    refused_both("DATEADD(ms, 1, sd)", "smalldatetime");
    refused_both("DATEADD(ms, 1, dt)", "1/300 s");
    refused("DATEADD(ms, 1, t0)", Dialect::Postgres, "rounds the result");
    refused_both("DATEADD(mcs, 1, d2)", "rounds the result");
    refused("DATEADD(ms, 1, tm)", Dialect::Db2i, "no fractional seconds");
    let (c, line) = run("DATEADD(ms, 1, tm)", Dialect::Postgres);
    assert_eq!(
        line, "SELECT (tm + 1 * interval '1 millisecond') AS r FROM t;",
        "{}",
        c.output
    );
}

#[test]
fn datetimeoffset_days_and_unknown_types_are_refused() {
    refused(
        "DATEADD(day, 1, dto)",
        Dialect::Postgres,
        "session time zone",
    );
    refused("DATEADD(hour, 1, dto)", Dialect::Db2i, "no Db2 for i type");
    refused_both("DATEADD(day, 1, '2024-01-01')", "the date is a string");
    refused_both("DATEADD(day, 1, n)", "not a date or time value");
    let src = "SELECT DATEADD(day, 1, q) AS r FROM elsewhere;\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(src, Dialect::Tsql, to).unwrap();
        assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
        assert!(c.units[0]
            .notes
            .iter()
            .any(|n| n.text.contains("not known")));
    }
}
