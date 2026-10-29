//! T-SQL built-ins against the PostgreSQL forms the converter writes for
//! them. Equal only where SQL Server 2022 and PostgreSQL 18 give the same
//! value for every input of the types involved; the runs behind each case
//! are in eval/builtins/2026-10-29 (styles.py, dateparts.py, probe*.sql).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.M (Id int NOT NULL PRIMARY KEY, S varchar(20) NULL,
  N nvarchar(max) NULL, T datetime2(3) NULL, Dt datetime NULL, Day date NULL, Tm time(3) NULL,
  Q int NULL, Dto datetimeoffset NULL);";
const PG_DDL: &str = "CREATE TABLE M (Id integer NOT NULL PRIMARY KEY, S varchar(20) NULL,
  N text NULL, T timestamp(3) NULL, Dt timestamp(3) NULL, Day date NULL, Tm time(3) NULL,
  Q integer NULL, Dto timestamptz NULL);";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    /// Equal, under these assumptions (at least).
    Eq(&'static [&'static str]),
    /// Not shown equal.
    NotEq,
}
use Want::*;

fn run(a: &str, b: &str, catalog: bool) -> Report {
    let opts = if catalog {
        Options {
            catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, PG_DDL)],
            ..Options::default()
        }
    } else {
        Options::default()
    };
    diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            format!("CREATE VIEW dbo.V AS SELECT Id, {a} AS x FROM dbo.M;"),
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            format!("CREATE OR REPLACE VIEW V AS SELECT Id, {b} AS x FROM M;"),
        ),
        &opts,
    )
}

fn check(cases: &[(&str, &str, Want)]) {
    let mut bad = Vec::new();
    for (a, b, want) in cases {
        let r = run(a, b, true);
        let equal = matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions);
        let ids: Vec<_> = r.assumptions.iter().map(|x| x.id.as_str()).collect();
        let ok = match want {
            Eq(asm) => equal && asm.iter().all(|x| ids.contains(x)),
            NotEq => !equal,
        };
        if !ok {
            bad.push(format!(
                "{want:?}: got {:?} {ids:?}\n  A: {a}\n  B: {b}\n  {:?}",
                r.verdict(),
                r.findings.iter().map(|f| &f.witness).collect::<Vec<_>>()
            ));
        }
        // Without catalogs the types are unknown: what is not equal with
        // them is not equal without them either.
        if matches!(want, NotEq)
            && matches!(
                run(a, b, false).verdict(),
                Verdict::Equal | Verdict::EqualUnderAssumptions
            )
        {
            bad.push(format!("equal without catalogs\n  A: {a}\n  B: {b}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn string_functions() {
    check(&[
        ("LEN(S)", "length(rtrim(S))", Eq(&["ASM-TSQL-LEN-BMP"])),
        ("LEN(N)", "length(rtrim(N))", Eq(&["ASM-TSQL-LEN-BMP"])),
        // LEN ignores trailing spaces.
        ("LEN(S)", "length(S)", NotEq),
        ("LEN(Q)", "length(rtrim(Q))", NotEq),
        ("REPLICATE(S, 3)", "repeat(S, 3)", Eq(&[])),
        ("REPLICATE(S, 0)", "repeat(S, 0)", Eq(&[])),
        // A negative count is NULL in T-SQL, '' in PostgreSQL.
        ("REPLICATE(S, -1)", "repeat(S, -1)", NotEq),
        ("REPLICATE(S, Q)", "repeat(S, Q)", NotEq),
        // T-SQL cuts a non-MAX result at 8,000 bytes.
        ("REPLICATE(S, 300)", "repeat(S, 300)", NotEq),
        ("REPLICATE(N, 300)", "repeat(N, 300)", Eq(&[])),
        ("REPLICATE(S, 3)", "repeat(S, 2)", NotEq),
    ]);
}

#[test]
fn character_codes() {
    check(&[
        ("CHAR(13)", "chr(13)", Eq(&[])),
        ("NCHAR(10)", "chr(10)", Eq(&[])),
        ("CHAR(13) + CHAR(10)", "chr(13) || chr(10)", Eq(&[])),
        ("CHAR(13)", "chr(10)", NotEq),
        // Above 127 the code page decides; 0 is an error in chr().
        ("CHAR(200)", "chr(200)", NotEq),
        ("NCHAR(0)", "chr(0)", NotEq),
        ("CHAR(Q)", "chr(Q)", NotEq),
    ]);
}

#[test]
fn date_parts() {
    check(&[
        ("YEAR(T)", "EXTRACT(YEAR FROM T)::int", Eq(&[])),
        ("MONTH(Day)", "EXTRACT(MONTH FROM Day)::int", Eq(&[])),
        ("DAY(Dt)", "EXTRACT(DAY FROM Dt)::int", Eq(&[])),
        ("DATEPART(year, T)", "EXTRACT(YEAR FROM T)::int", Eq(&[])),
        ("DATEPART(hh, T)", "EXTRACT(HOUR FROM T)::int", Eq(&[])),
        (
            "DATEPART(minute, Tm)",
            "EXTRACT(MINUTE FROM Tm)::int",
            Eq(&[]),
        ),
        // DATEPART truncates the seconds; ::int rounds 59.9 to 60.
        (
            "DATEPART(second, T)",
            "floor(EXTRACT(SECOND FROM T))::int",
            Eq(&[]),
        ),
        (
            "DATEPART(second, T)",
            "trunc(EXTRACT(SECOND FROM T))::int",
            Eq(&[]),
        ),
        ("DATEPART(second, T)", "EXTRACT(SECOND FROM T)::int", NotEq),
        (
            "DATEPART(weekday, Day)",
            "(EXTRACT(DOW FROM Day)::int + 1)",
            Eq(&["ASM-TSQL-DATEFIRST-7"]),
        ),
        (
            "DATEPART(weekday, Day)",
            "EXTRACT(DOW FROM Day)::int",
            NotEq,
        ),
        ("YEAR(T)", "EXTRACT(MONTH FROM T)::int", NotEq),
        // A string or a time zone: T-SQL converts or keeps the offset's
        // local time, PostgreSQL does neither the same way.
        ("YEAR(S)", "EXTRACT(YEAR FROM S::timestamp)::int", NotEq),
        ("YEAR(Dto)", "EXTRACT(YEAR FROM Dto)::int", NotEq),
        // YEAR of a time is 1900 in T-SQL.
        ("DATEPART(hour, Day)", "EXTRACT(HOUR FROM Day)::int", NotEq),
    ]);
}

#[test]
fn convert_styles() {
    let ymdhms = "'YYYY-MM-DD HH24:MI:SS'";
    check(&[
        (
            "CONVERT(varchar(19), T, 120)",
            &format!("to_char(T, {ymdhms})"),
            Eq(&[]),
        ),
        (
            "CONVERT(nvarchar(max), Dt, 20)",
            &format!("to_char(Dt, {ymdhms})"),
            Eq(&[]),
        ),
        (
            "CONVERT(varchar, T, 120)",
            &format!("to_char(T, {ymdhms})"),
            Eq(&[]),
        ),
        // A date prints only its date part in style 120.
        (
            "CONVERT(varchar(30), Day, 120)",
            "to_char(Day, 'YYYY-MM-DD')",
            Eq(&[]),
        ),
        (
            "CONVERT(varchar(30), Day, 120)",
            &format!("to_char(Day, {ymdhms})"),
            NotEq,
        ),
        (
            "CONVERT(varchar(8), Day, 112)",
            "to_char(Day, 'YYYYMMDD')",
            Eq(&[]),
        ),
        (
            "CONVERT(varchar(10), T, 23)",
            "to_char(T, 'YYYY-MM-DD')",
            Eq(&[]),
        ),
        (
            "CONVERT(varchar(10), T, 102)",
            "to_char(T, 'YYYY.MM.DD')",
            Eq(&[]),
        ),
        (
            "CONVERT(varchar(10), T, 111)",
            "to_char(T, 'YYYY/MM/DD')",
            Eq(&[]),
        ),
        (
            "CONVERT(varchar(8), T, 108)",
            "to_char(T, 'HH24:MI:SS')",
            Eq(&[]),
        ),
        // A shorter target cuts the text on both sides.
        (
            "CONVERT(varchar(10), T, 120)",
            &format!("CAST(to_char(T, {ymdhms}) AS varchar(10))"),
            Eq(&[]),
        ),
        (
            "CONVERT(varchar(10), T, 120)",
            &format!("to_char(T, {ymdhms})"),
            NotEq,
        ),
        (
            "CONVERT(varchar(30), T, 121)",
            "to_char(T, 'YYYY-MM-DD HH24:MI:SS.MS')",
            NotEq,
        ),
        (
            "CONVERT(varchar(30), T, 112)",
            &format!("to_char(T, {ymdhms})"),
            NotEq,
        ),
        (
            "CONVERT(varchar(8), Day, 108)",
            "to_char(Day, 'HH24:MI:SS')",
            NotEq,
        ),
        (
            "CONVERT(varchar(30), S, 120)",
            &format!("to_char(S::timestamp, {ymdhms})"),
            NotEq,
        ),
        (
            "CONVERT(varchar(30), Dto, 120)",
            &format!("to_char(Dto, {ymdhms})"),
            NotEq,
        ),
    ]);
}

#[test]
fn database_name() {
    check(&[(
        "DB_NAME()",
        "current_database()",
        Eq(&["ASM-SAME-DATABASE-NAME"]),
    )]);
}
