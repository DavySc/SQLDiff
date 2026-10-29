//! Date and time literals assigned to variables: Db2 for i gets the ISO form
//! of the value T-SQL stores (it reads neither `yyyymmdd` nor
//! `yyyymmdd hh:mm:ss` as a date or timestamp, fact V33), and the engine
//! proves the assigned values equal. PostgreSQL keeps the literal.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

const PROC: &str = "CREATE PROCEDURE dbo.P
AS
BEGIN
    DECLARE @EndOfTime datetime2(7) = '99991231 23:59:59.9999999';
    DECLARE @d date = '20200101';
    DECLARE @phone nvarchar(20) = N'93748567';
    DECLARE @c datetime2(7);
    DECLARE @r datetime2(3);
    DECLARE @w datetime;
    SET @c = '20200101';
    SET @c = '2020-01-01 10:00:00.5';
    SET @r = '2020-01-01 10:00:00.1234';
    SET @w = '2020-01-02';
    SET @w = '20200102 10:00:00.120';
END
";

fn run(to: Dialect) -> Conversion {
    convert(PROC, Dialect::Tsql, to).unwrap()
}

fn rules(c: &Conversion) -> Vec<&str> {
    c.units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| n.rule.as_str()))
        .collect()
}

#[test]
fn db2_for_i_gets_iso_literals() {
    let c = run(Dialect::Db2i);
    let out = &c.output;
    for line in [
        "SET v_end_of_time = '9999-12-31 23:59:59.9999999';",
        "SET v_d = '2020-01-01';",
        "SET v_c = '2020-01-01 00:00:00';",
        "SET v_c = '2020-01-01 10:00:00.5';",
        "SET v_w = '2020-01-02 00:00:00';",
        "SET v_w = '2020-01-02 10:00:00.12';",
        // Not a date: stays as written.
        "SET v_phone = '93748567';",
        // T-SQL rounds a fourth digit into datetime2(3): no exact text, kept.
        "SET v_r = '2020-01-01 10:00:00.1234';",
    ] {
        assert!(out.contains(line), "missing {line}\n{out}");
    }
    let r = rules(&c);
    assert!(r.contains(&"ASM-FACT-V33"), "{r:?}");
    // The dash form read into datetime depends on DATEFORMAT.
    assert!(r.contains(&"ASM-TSQL-DATEFORMAT-YMD"), "{r:?}");
}

#[test]
fn the_engine_proves_the_db2_values() {
    let c = run(Dialect::Db2i);
    let unproven: Vec<&str> = c
        .report
        .findings
        .iter()
        .map(|f| f.witness.as_str())
        .filter(|w| w.contains("could not be shown equivalent"))
        .collect();
    for v in [
        "2020-01-01 00",
        "9999-12-31",
        "2020-01-01 10:00:00.5`",
        "2020-01-02",
    ] {
        assert!(!unproven.iter().any(|w| w.contains(v)), "{v}: {unproven:?}");
    }
    // Only the datetime2(3) literal T-SQL rounds (source line 12) stays
    // unproven.
    assert_eq!(unproven.len(), 1, "{unproven:?}");
    assert!(unproven[0].contains("A line 12,"), "{unproven:?}");
}

#[test]
fn postgres_keeps_a_literal_without_a_seventh_digit() {
    let c = run(Dialect::Postgres);
    assert!(c.output.contains("v_c := '20200101';"), "{}", c.output);
    assert!(!rules(&c).contains(&"ASM-FACT-V33"));
}

const SEVEN: &str = "CREATE PROCEDURE dbo.Q
AS
BEGIN
    DECLARE @EndOfTime datetime2(7) = '99991231 23:59:59.9999999';
    DECLARE @t datetime2 = '2020-01-01 10:00:00.1234567';
    DECLARE @r datetime2(3);
    SET @EndOfTime = '9999-12-31 23:59:59.9999999';
    SET @r = '9999-12-31 23:59:59.9999999';
END
";

/// PostgreSQL keeps 6 fraction digits and rounds a 7th, so T-SQL's end of
/// time into a datetime2(7) variable is written with 6 (Davy, 2026-10-29)
/// under ASM-END-OF-TIME, and the engine shows it equal under that
/// assumption and ASM-FRACTIONAL-SECONDS. Other 7-digit literals stay.
#[test]
fn postgres_writes_the_end_of_time_with_six_digits_under_an_assumption() {
    let c = convert(SEVEN, Dialect::Tsql, Dialect::Postgres).unwrap();
    for line in [
        "v_end_of_time := '9999-12-31 23:59:59.999999';",
        "v_t := '2020-01-01 10:00:00.1234567';",
        // datetime2(3): T-SQL rounds into 10000-01-01 too; kept.
        "v_r := '9999-12-31 23:59:59.9999999';",
    ] {
        assert!(c.output.contains(line), "missing {line}\n{}", c.output);
    }
    let unit = |line: usize| {
        c.units
            .iter()
            .filter(|u| u.src.line == line)
            .map(|u| {
                (
                    u.status,
                    u.notes.iter().map(|n| n.rule.as_str()).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    for line in [4, 7] {
        let us = unit(line);
        assert!(!us.is_empty(), "line {line}");
        assert!(
            us.iter().all(|(s, _)| *s == Status::Assumptions),
            "line {line}: {us:?}"
        );
        assert!(
            us.iter().any(|(_, r)| r.contains(&"ASM-END-OF-TIME")),
            "line {line}: {us:?}"
        );
    }
    // Not the sentinel: not rewritten, and not shown equal.
    for line in [5, 8] {
        let us = unit(line);
        assert!(
            us.iter().all(|(s, _)| *s > Status::Assumptions),
            "line {line}: {us:?}"
        );
    }
}
