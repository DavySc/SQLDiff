//! Statements the conversion check left unpaired although the trace links
//! them (burndown U): a declaration whose value was refused, `RETURN n` in a
//! procedure, a batch the conversion wrote no statement for, and a body
//! holding `$$`.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

/// The smallest unit whose output contains `needle`.
fn out_unit<'a>(c: &'a Conversion, needle: &str) -> &'a Unit {
    let at = c
        .output
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in\n{}", c.output));
    c.units
        .iter()
        .filter(|u| u.out.start <= at && at < u.out.end.max(u.out.start + 1))
        .min_by_key(|u| u.out.end - u.out.start)
        .unwrap()
}

fn clean(u: &Unit) -> bool {
    matches!(u.status, Status::Converted | Status::Assumptions)
}

fn reasons(u: &Unit) -> String {
    u.notes
        .iter()
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_refused_initial_value_leaves_its_declaration_compared() {
    let src = "CREATE PROCEDURE dbo.P
AS
BEGIN
  DECLARE @A nvarchar(max)
  DECLARE @SchemaName nvarchar(max) = OBJECT_SCHEMA_NAME(@@PROCID)
  DECLARE @B int = 1
  SET @A = N'x'
END
";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    // Before: the declaration took the reason of its refused value ("its
    // counterpart may be in an error raise") and stayed not proven.
    let decl = out_unit(&c, "v_schema_name text;");
    assert!(clean(decl), "{:?}: {}", decl.status, reasons(decl));
    // The refused value stays not converted, and the engine still says it
    // compared nothing there.
    let raise = out_unit(&c, "RAISE EXCEPTION");
    assert_eq!(raise.status, Status::NotConverted);
    assert!(
        c.report
            .unknown_reasons
            .iter()
            .any(|r| r.starts_with("side B (PostgreSQL)")
                && r.contains("the statement at side A (T-SQL) line 5 is not paired")),
        "{:#?}",
        c.report.unknown_reasons
    );
}

#[test]
fn return_n_in_a_procedure_is_compared_with_its_return_code() {
    let src = "CREATE PROCEDURE dbo.P
@Mode int
AS
BEGIN
  SET XACT_ABORT ON
  DECLARE @ReturnCode int = 0
  IF @Mode = 1 RETURN 5
  SET @ReturnCode = @Mode * 2
  RETURN @ReturnCode
END
";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    let five = out_unit(&c, "COALESCE(5, 0)");
    assert!(clean(five), "{:?}: {}", five.status, reasons(five));
    let rc = out_unit(&c, "COALESCE(v_return_code, 0)");
    assert!(clean(rc), "{:?}: {}", rc.status, reasons(rc));
    // A NULL status is 0 on both sides, but T-SQL also sends a warning.
    assert!(
        rc.notes
            .iter()
            .any(|n| n.rule == "ASM-TSQL-RETURN-NULL-WARNING"),
        "{}",
        reasons(rc)
    );
    // Only the `return_code := 0;` the conversion puts first (T-SQL's
    // status starts at 0) is still unpaired; it is in no unit.
    let left: Vec<&String> = c
        .report
        .unknown_reasons
        .iter()
        .filter(|r| r.contains("no counterpart"))
        .collect();
    assert_eq!(left.len(), 1, "{left:#?}");
    assert!(
        left[0].starts_with("side B (PostgreSQL) line 7:"),
        "{left:#?}\n{}",
        c.output
    );
}

#[test]
fn return_of_a_decimal_is_not_taken_for_its_return_code() {
    // T-SQL truncates 2.5 to the int status 2; PostgreSQL rounds it into
    // the integer return_code (3). The pair is left unpaired.
    let src = "CREATE PROCEDURE dbo.P
AS
BEGIN
  DECLARE @D decimal(5,1) = 2.5
  RETURN @D
END
";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    let rc = out_unit(&c, "COALESCE(v_d, 0)");
    assert!(!clean(rc), "{:?}: {}\n{}", rc.status, reasons(rc), c.output);
}

#[test]
fn a_batch_the_conversion_wrote_nothing_for_keeps_the_others_paired() {
    let src = "DECLARE @StartOfTime datetime2(7) = '20130101';
DECLARE @N int = 5;
PRINT 'done';
GO
ALTER DATABASE CURRENT SET MEMORY_OPTIMIZED_ELEVATE_TO_SNAPSHOT=ON
GO
";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    // Before: two segments on side A and one on side B, so the script was
    // compared whole, the DO block was "several routines" and every
    // statement had no counterpart.
    for s in ["v_n integer;", "v_n := 5;", "RAISE NOTICE"] {
        let u = out_unit(&c, s);
        assert!(clean(u), "{s}: {:?}: {}", u.status, reasons(u));
    }
    let alter = c
        .units
        .iter()
        .find(|u| src[u.src.start..u.src.end].starts_with("ALTER DATABASE"))
        .unwrap();
    assert!(!clean(alter));
}

#[test]
fn a_body_holding_dollar_dollar_gets_another_quote() {
    let src = "DECLARE @Pattern nvarchar(100) = N'rollingtest-$${sd:type}.log';
PRINT @Pattern;
GO
CREATE PROCEDURE dbo.P
AS
BEGIN
  DECLARE @X nvarchar(100) = N'a$$b'
  PRINT @X
END
";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    assert!(c.output.contains("DO $sqldiff$\n"), "{}", c.output);
    assert!(c.output.contains("AS $sqldiff$\n"), "{}", c.output);
    // Before: the `$$` in the string ended the body and the rest did not
    // parse.
    for u in &c.units {
        assert!(
            !u.notes.iter().any(|n| n.rule == "parse"),
            "{}\n{}",
            reasons(u),
            c.output
        );
    }
    let x = out_unit(&c, "v_x := 'a$$b'");
    assert!(clean(x), "{:?}: {}", x.status, reasons(x));
}
