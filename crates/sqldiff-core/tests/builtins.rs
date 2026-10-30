//! T-SQL built-ins against the PostgreSQL forms the converter writes for
//! them. Equal only where SQL Server 2022 and PostgreSQL 18 give the same
//! value for every input of the types involved; the runs behind each case
//! are in eval/builtins/2026-10-29 (styles.py, dateparts.py, probe*.sql).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.M (Id int NOT NULL PRIMARY KEY, S varchar(20) NULL, C char(5) NULL, NC nchar(5) NULL,
  N nvarchar(max) NULL, T datetime2(3) NULL, Dt datetime NULL, Day date NULL, Tm time(3) NULL,
  Q int NULL, Dto datetimeoffset NULL, D decimal(10,2) NULL, F float NULL, S2 varchar(10) NULL,
  B bigint NULL, W nvarchar(4000) NULL, V varchar(8000) NULL);";
const PG_DDL: &str = "CREATE TABLE M (Id integer NOT NULL PRIMARY KEY, S varchar(20) NULL, C char(5) NULL, NC char(5) NULL,
  N text NULL, T timestamp(3) NULL, Dt timestamp(3) NULL, Day date NULL, Tm time(3) NULL,
  Q integer NULL, Dto timestamptz NULL, D decimal(10,2) NULL, F double precision NULL,
  S2 varchar(10) NULL, B bigint NULL, W varchar(4000) NULL, V varchar(8000) NULL);";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    /// Equal, under these assumptions (at least).
    Eq(&'static [&'static str]),
    /// Not shown equal.
    NotEq,
}
use Want::*;

fn run(a: &str, b: &str, catalog: bool) -> Report {
    run_as(a, b, catalog, false)
}

/// `conversion`: as a conversion check runs it, where T-SQL's
/// case-insensitive string matching is flagged with ASM-CI-COLLATION.
fn run_as(a: &str, b: &str, catalog: bool, conversion: bool) -> Report {
    let _ci = sqldiff_core::equiv::compare::ci_collation_scope(conversion);
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
    check_as(cases, false)
}

fn check_as(cases: &[(&str, &str, Want)], conversion: bool) {
    let mut bad = Vec::new();
    for (a, b, want) in cases {
        let r = run_as(a, b, true, conversion);
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
        // A MAX value is not told apart from one of unknown length, which
        // T-SQL may cut (DB_NAME() is nvarchar(128)): not shown equal.
        ("REPLICATE(N, 300)", "repeat(N, 300)", NotEq),
        (
            "REPLICATE(DB_NAME(), 3000)",
            "repeat(current_database(), 3000)",
            NotEq,
        ),
        // PostgreSQL strips char padding before repeat; T-SQL keeps it.
        ("REPLICATE(C, 3)", "repeat(C, 3)", NotEq),
        ("REPLICATE(NC, 3)", "repeat(NC, 3)", NotEq),
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

/// REPLACE and CHARINDEX match under the collation: T-SQL finds 'b' in
/// 'ABC', PostgreSQL does not. Equal only in a conversion check, under
/// ASM-CI-COLLATION; a plain diff keeps them apart.
#[test]
fn substring_matches_follow_the_collation() {
    const CI: Want = Eq(&["ASM-CI-COLLATION"]);
    check_as(
        &[
            ("REPLACE(S, 'a', 'b')", "replace(S, 'a', 'b')", CI),
            ("REPLACE(S, 'a', 'b')", "replace(S, 'a', 'c')", NotEq),
            // Shrinks: never cut, whatever the length.
            ("REPLACE(N, 'ab', 'x')", "replace(N, 'ab', 'x')", CI),
            // Grows within 4,000 characters of a known length.
            ("REPLACE(S, 'a', 'bb')", "replace(S, 'a', 'bb')", CI),
            // Grows from an unknown length: T-SQL may cut at 8,000 bytes.
            ("REPLACE(N, , '')", "replace(N, , '')", NotEq),
            ("REPLACE(S, S2, N)", "replace(S, S2, N)", NotEq),
            // One character for one, but 😀 is two UTF-16 units (four UTF-8
            // bytes): 4,000 'a's become 8,000 units, which T-SQL cuts.
            ("REPLACE(W, N'a', N'😀')", "replace(W, 'a', '😀')", NotEq),
            ("REPLACE(V, 'a', 'é')", "replace(V, 'a', 'é')", NotEq),
            ("REPLACE(W, N'é', N'a')", "replace(W, 'é', 'a')", CI),
            // PostgreSQL strips char padding before replacing.
            ("REPLACE(C, 'a', 'b')", "replace(C, 'a', 'b')", NotEq),
            // Positions count UTF-16 units in T-SQL: ASM-TSQL-LEN-BMP too.
            (
                "CHARINDEX('a', S)",
                "strpos(S, 'a')",
                Eq(&["ASM-CI-COLLATION", "ASM-TSQL-LEN-BMP"]),
            ),
            ("CHARINDEX(',' + S2, S)", "strpos(S, ',' || S2)", CI),
            // An empty needle: CHARINDEX 0, strpos 1.
            ("CHARINDEX(S2, S)", "strpos(S, S2)", NotEq),
            ("CHARINDEX('', S)", "strpos(S, '')", NotEq),
            (
                "CHARINDEX(S2, S)",
                "CASE WHEN length(S2) = 0 THEN 0 ELSE strpos(S, S2) END",
                CI,
            ),
            (
                "CHARINDEX(S2, S)",
                "CASE WHEN length(S) = 0 THEN 0 ELSE strpos(S, S2) END",
                NotEq,
            ),
            ("CHARINDEX('a', C)", "strpos(C, 'a')", NotEq),
            ("CHARINDEX('a', S)", "strpos(S, 'b')", NotEq),
        ],
        true,
    );
    check(&[
        ("REPLACE(S, 'a', 'b')", "replace(S, 'a', 'b')", NotEq),
        ("CHARINDEX('a', S)", "strpos(S, 'a')", NotEq),
    ]);
}

#[test]
fn trims_quotes_and_numbers() {
    check(&[
        // Spaces only on both sides (not tabs or NBSP).
        ("LTRIM(S)", "ltrim(S)", Eq(&[])),
        ("RTRIM(LTRIM(S))", "rtrim(ltrim(S))", Eq(&[])),
        ("LTRIM(S)", "rtrim(S)", NotEq),
        ("LTRIM(S)", "btrim(S)", NotEq),
        // PostgreSQL strips char padding first: LTRIM(' ab') of a char(6)
        // keeps 'ab   ' in T-SQL.
        ("LTRIM(C)", "ltrim(C)", NotEq),
        (
            "QUOTENAME(S)",
            "quote_ident(S)",
            Eq(&["ASM-QUOTENAME-IDENT"]),
        ),
        ("QUOTENAME(S)", "S", NotEq),
        ("CEILING(D)", "ceil(D)", Eq(&[])),
        ("CEILING(D)", "ceiling(D)", Eq(&[])),
        ("FLOOR(F)", "floor(F)", Eq(&[])),
        ("CEILING(D)", "floor(D)", NotEq),
        ("CEILING(Q)", "ceil(Q)", Eq(&[])),
        // ceil(bigint) is ceil(double precision): 9007199254740993 becomes
        // 9007199254740992.
        ("CEILING(B)", "ceil(B)", NotEq),
        ("FLOOR(B)", "floor(B)", NotEq),
        ("CEILING(B) + 1", "ceil(B) + 1", NotEq),
        ("SQRT(B)", "sqrt(B)", Eq(&[])),
        // CEILING of an int stays int in T-SQL, is double precision in
        // PostgreSQL: the division that follows differs.
        ("CEILING(Q) / 2", "ceil(Q) / 2", NotEq),
        ("SQRT(F)", "sqrt(F)", Eq(&[])),
        ("SQRT(Q)", "sqrt(Q)", Eq(&[])),
        // sqrt(numeric) is a numeric with other digits than T-SQL's float.
        ("SQRT(D)", "sqrt(D)", NotEq),
    ]);
}

/// A construct lowered per dialect is named in the unproven message.
#[test]
fn unproven_built_ins_are_named() {
    let r = run("NEWID()", "gen_random_uuid()", true);
    assert_eq!(r.verdict(), Verdict::Unknown);
    let text = format!("{:?}", r.findings);
    assert!(
        text.contains("A has `newid()` (T-SQL) where B has `gen_random_uuid()` (PostgreSQL)"),
        "{text}"
    );
}

/// A procedure's `OBJECT_NAME(@@PROCID)` / `OBJECT_SCHEMA_NAME(@@PROCID)`
/// against the literal the converter writes: its own name and schema, letter
/// for letter, under ASM-PROCID-NAME.
#[test]
fn own_name_from_procid() {
    let run = |header: &str, a: &str, b: &str| {
        diff(
            &Artifact::new(
                "a.sql",
                Dialect::Tsql,
                format!("CREATE PROCEDURE {header}\nAS\nBEGIN\n    DECLARE @r nvarchar(max);\n    SET @r = {a};\n    INSERT INTO dbo.L (s) VALUES (@r);\nEND\n"),
            ),
            &Artifact::new(
                "b.sql",
                Dialect::Postgres,
                format!("CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nDECLARE\n    v_r text;\nBEGIN\n    v_r := {b};\n    INSERT INTO L (s) VALUES (v_r);\nEND;\n$$;\n"),
            ),
            &Options {
                catalog_a: vec![Artifact::new("c.sql", Dialect::Tsql, "CREATE TABLE dbo.L (s nvarchar(max) NULL);")],
                catalog_b: vec![Artifact::new("c.sql", Dialect::Postgres, "CREATE TABLE L (s text NULL);")],
                ..Options::default()
            },
        )
    };
    let equal = |r: &Report| matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions);
    let r = run("[dbo].[Proc_A]", "OBJECT_NAME(@@PROCID)", "'Proc_A'");
    assert!(equal(&r), "{:?} {:?}", r.verdict(), r.findings);
    assert!(r.assumptions.iter().any(|x| x.id == "ASM-PROCID-NAME"));
    let r = run("[dbo].[Proc_A]", "OBJECT_SCHEMA_NAME(@@PROCID)", "'dbo'");
    assert!(equal(&r), "{:?} {:?}", r.verdict(), r.findings);
    // Another name, another letter case, another schema.
    for (header, a, b) in [
        ("[dbo].[Proc_A]", "OBJECT_NAME(@@PROCID)", "'Proc_B'"),
        ("[dbo].[Proc_A]", "OBJECT_NAME(@@PROCID)", "'proc_a'"),
        ("[dbo].[Proc_A]", "OBJECT_SCHEMA_NAME(@@PROCID)", "'Proc_A'"),
        ("Sales.Proc_A", "OBJECT_SCHEMA_NAME(@@PROCID)", "'dbo'"),
        // No schema in the header: the creator's default schema, unknown.
        ("Proc_A", "OBJECT_SCHEMA_NAME(@@PROCID)", "'dbo'"),
        // Another object's id.
        ("[dbo].[Proc_A]", "OBJECT_NAME(@@SPID)", "'Proc_A'"),
    ] {
        let r = run(header, a, b);
        assert!(!equal(&r), "{header} {a} = {b}: {:?}", r.verdict());
    }
}

/// A function's `@@PROCID` may be its caller's: SQL Server runs an inline
/// table-valued function, and a scalar function it inlines, inside the
/// calling query. Never folded there.
#[test]
fn own_name_from_procid_not_in_functions() {
    let r = diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            "CREATE FUNCTION dbo.F()\nRETURNS nvarchar(128)\nAS\nBEGIN\n    RETURN OBJECT_NAME(@@PROCID);\nEND\n",
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            "CREATE OR REPLACE FUNCTION F() RETURNS text\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    RETURN 'F';\nEND;\n$$;\n",
        ),
        &Options::default(),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}
