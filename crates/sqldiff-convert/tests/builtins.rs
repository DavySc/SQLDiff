//! T-SQL built-ins to PostgreSQL: the forms checked on SQL Server 2022 and
//! PostgreSQL 18 (eval/builtins/2026-10-29) convert and are proven by the
//! engine; the rest are refused with a reason that says why.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

/// One assignment in a procedure whose parameters and variables give every
/// value a type.
fn run(stmt: &str) -> (Conversion, String) {
    let src = format!(
        "CREATE PROCEDURE dbo.p @s nvarchar(100), @d datetime2, @dt datetime, @day date, @sd smalldatetime
AS
BEGIN
    DECLARE @r nvarchar(max);
    DECLARE @i int;
    {stmt}
END
"
    );
    let c = convert(&src, Dialect::Tsql, Dialect::Postgres).unwrap();
    let line = c
        .output
        .lines()
        .find(|l| l.trim_start().starts_with("v_r :=") || l.trim_start().starts_with("v_i :="))
        .unwrap_or_default()
        .trim()
        .to_string();
    (c, line)
}

/// The unit of the statement on line 6.
fn unit(c: &Conversion) -> &sqldiff_convert::Unit {
    c.units.iter().find(|u| u.src.line == 6).unwrap()
}

/// Converted to exactly `want`, and the engine proved it (possibly under
/// assumptions).
#[track_caller]
fn proven(stmt: &str, want: &str) {
    let (c, line) = run(stmt);
    assert_eq!(line, want, "{}", c.output);
    let u = unit(&c);
    assert_eq!(
        u.checked,
        Status::Converted,
        "{stmt}\n{:?}\n{}",
        u.notes,
        c.output
    );
    assert!(
        matches!(u.status, Status::Converted | Status::Assumptions),
        "{stmt}: {:?} {:?}",
        u.status,
        u.notes
    );
}

/// Refused, with a reason containing `why`.
#[track_caller]
fn refused(stmt: &str, why: &str) {
    let (c, _) = run(stmt);
    let u = unit(&c);
    assert_eq!(u.status, Status::NotConverted, "{stmt}\n{}", c.output);
    assert!(
        u.notes.iter().any(|n| n.text.contains(why)),
        "{stmt}: {:?}",
        u.notes
    );
}

#[test]
fn date_parts() {
    proven("SET @i = YEAR(@d);", "v_i := EXTRACT(YEAR FROM p_d)::int;");
    proven(
        "SET @i = DATEPART(month, @day);",
        "v_i := EXTRACT(month FROM p_day)::int;",
    );
    // DATEPART truncates the seconds (59.9 is 59); ::int alone would round to 60.
    proven(
        "SET @i = DATEPART(second, @d);",
        "v_i := floor(EXTRACT(SECOND FROM p_d))::int;",
    );
    proven(
        "SET @i = DATEPART(weekday, @day);",
        "v_i := (EXTRACT(DOW FROM p_day)::int + 1);",
    );
    let (c, _) = run("SET @i = DATEPART(dw, @day);");
    assert!(unit(&c)
        .notes
        .iter()
        .any(|n| n.rule == "ASM-TSQL-DATEFIRST-7"));
}

#[test]
fn convert_styles() {
    proven(
        "SET @r = N'at ' + CONVERT(nvarchar(max), @d, 120);",
        "v_r := 'at ' || to_char(p_d, 'YYYY-MM-DD HH24:MI:SS');",
    );
    proven(
        "SET @r = CONVERT(varchar(20), @day, 112);",
        "v_r := to_char(p_day, 'YYYYMMDD');",
    );
    // A date prints only its date part, also in style 120.
    proven(
        "SET @r = CONVERT(varchar(30), @day, 120);",
        "v_r := to_char(p_day, 'YYYY-MM-DD');",
    );
    proven(
        "SET @r = CONVERT(nvarchar(10), @dt, 120);",
        "v_r := CAST(to_char(p_dt, 'YYYY-MM-DD HH24:MI:SS') AS varchar(10));",
    );
    refused(
        "SET @r = CONVERT(varchar(30), @d, 121);",
        "style 121 of datetime2(7)",
    );
    refused(
        "SET @r = CONVERT(varchar(30), @sd, 120);",
        "style 120 of smalldatetime",
    );
    refused("SET @r = CONVERT(varchar(30), @s, 120);", "not a date");
    refused(
        "SET @r = CONVERT(char(30), @d, 120);",
        "VARCHAR or NVARCHAR target",
    );
}

#[test]
fn character_functions() {
    proven(
        "SET @r = NCHAR(13) + NCHAR(10);",
        "v_r := chr(13) || chr(10);",
    );
    proven("SET @i = LEN(@s);", "v_i := length(rtrim(p_s));");
    proven("SET @r = REPLICATE(N'ab', 3);", "v_r := repeat('ab', 3);");
    refused("SET @r = NCHAR(200);", "UTF-16 code unit");
}

#[test]
fn charindex_of_an_empty_needle_is_zero() {
    // strpos finds '' at 1, CHARINDEX nowhere (0): only a needle that cannot
    // be empty is passed straight through.
    let (_, line) = run("SET @i = CHARINDEX(N'a', @s);");
    assert_eq!(line, "v_i := strpos(p_s, 'a');");
    let (_, line) = run("SET @i = CHARINDEX(@s + N',', N'a,b');");
    assert_eq!(line, "v_i := strpos('a,b', p_s || ',');");
    let (_, line) = run("SET @i = CHARINDEX(@s, N'abc');");
    assert_eq!(
        line,
        "v_i := (CASE WHEN length(p_s) = 0 THEN 0 ELSE strpos('abc', p_s) END);"
    );
    refused("SET @i = CHARINDEX(LTRIM(@s), N'abc');", "computed needle");
}

#[test]
fn database_name_is_an_assumption() {
    proven("SET @r = DB_NAME();", "v_r := current_database();");
    let (c, _) = run("SET @r = DB_NAME();");
    assert!(unit(&c)
        .notes
        .iter()
        .any(|n| n.rule == "ASM-SAME-DATABASE-NAME"));
}

#[test]
fn instance_metadata_is_refused_with_its_reason() {
    refused(
        "SET @r = CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(max));",
        "describes the SQL Server instance",
    );
    refused("SET @r = OBJECT_NAME(@@PROCID);", "@@PROCID");
    refused("SET @i = OBJECT_ID(N'dbo.t');", "SQL Server object id");
    refused(
        "SET @r = JSON_MODIFY(@s, '$.a', 1);",
        "JSON_MODIFY deletes a key",
    );
    refused(
        "SET @i = ABS(CHECKSUM(NEWID())) % 10;",
        "SQL Server's own hash",
    );
}

/// Proven under the case-insensitive collation assumption, which the unit
/// carries for the reviewer.
#[track_caller]
fn proven_ci(stmt: &str, want: &str) {
    proven(stmt, want);
    let (c, _) = run(stmt);
    let u = unit(&c);
    assert!(
        u.notes.iter().any(|n| n.rule == "ASM-CI-COLLATION"),
        "{stmt}: {:?}",
        u.notes
    );
}

#[test]
fn substring_matches_are_proven_under_the_collation_assumption() {
    proven_ci(
        "SET @r = REPLACE(@s, N'a', N'b');",
        "v_r := REPLACE(p_s, 'a', 'b');",
    );
    proven_ci("SET @i = CHARINDEX(N',', @s);", "v_i := strpos(p_s, ',');");
    proven_ci(
        "SET @i = CHARINDEX(@s, N'abc');",
        "v_i := (CASE WHEN length(p_s) = 0 THEN 0 ELSE strpos('abc', p_s) END);",
    );
}

#[test]
fn trims_and_quotes_are_proven() {
    proven("SET @r = LTRIM(RTRIM(@s));", "v_r := LTRIM(RTRIM(p_s));");
    proven("SET @r = QUOTENAME(@s);", "v_r := quote_ident(p_s);");
    let (c, _) = run("SET @r = QUOTENAME(@s);");
    assert!(unit(&c)
        .notes
        .iter()
        .any(|n| n.rule == "ASM-QUOTENAME-IDENT"));
}
