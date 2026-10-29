//! The conversion check pairs each input statement with the output written
//! from it (the units' trace), so a declaration moved to the top of its
//! block, a dropped statement or a statement the engine cannot read does not
//! shift the pairing of the statements around it.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

/// Ola Hallengren's layout: a setup batch that creates a stub, then the
/// real body as ALTER PROCEDURE, with `nvarchar(max)` declarations, a
/// declaration with a value, and a table variable.
const OLA: &str = "SET ANSI_NULLS ON
GO
IF NOT EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[p]') AND type in (N'P', N'PC'))
BEGIN
EXEC dbo.sp_executesql @statement = N'CREATE PROCEDURE [dbo].[p] AS'
END
GO
ALTER PROCEDURE [dbo].[p]
@Name nvarchar(max),
@Mode int
AS
BEGIN
  SET NOCOUNT ON
  DECLARE @Message nvarchar(max)
  DECLARE @Count int
  DECLARE @Errors TABLE (ID int IDENTITY PRIMARY KEY, [Message] nvarchar(max) NOT NULL)
  DECLARE @Prefix nvarchar(max) = N'p: ' + @Name
  SET @Count = @Mode + 1
  INSERT INTO @Errors ([Message]) VALUES ('a')
  INSERT INTO @Errors ([Message]) VALUES ('b')
  SET @Message = @Prefix + N'x'
  SET @Count = @Count * 2
END
GO
";

/// The smallest unit whose input contains `needle`, and is not the moved
/// declaration's comment (the last unit for that input).
fn unit<'a>(c: &'a Conversion, needle: &str) -> &'a Unit {
    let at = OLA.find(needle).unwrap();
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end)
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap()
}

/// What the engine left unpaired other than a statement whose output is a
/// not-converted placeholder (the PostgreSQL table variable).
fn unpaired(c: &Conversion) -> Vec<&String> {
    c.report
        .unknown_reasons
        .iter()
        .filter(|r| {
            r.contains("more than one way")
                || r.contains("in another order")
                || r.contains("no counterpart")
                || r.contains("several routines")
                || r.contains("expected RParen")
        })
        .collect()
}

#[test]
fn db2_lob_declarations_and_moved_declarations_keep_the_pairing() {
    let c = convert(OLA, Dialect::Tsql, Dialect::Db2i).unwrap();
    assert!(
        c.output.contains("DECLARE v_message NCLOB(1G);"),
        "{}",
        c.output
    );
    // Before: `NCLOB(1G)` did not read, every declaration became an
    // unreadable statement, and the statements around them lined up in more
    // than one way.
    assert_eq!(unpaired(&c), Vec::<&String>::new());
    // Compared with its own conversion. (Whether the `CAST(… AS NCLOB(1G))`
    // the conversion adds is a no-op needs Db2 for i type facts; until then
    // the pair stays unproven.)
    for s in ["SET @Count = @Mode + 1", "SET @Message = @Prefix + N'x'"] {
        let u = unit(&c, s);
        assert!(
            !u.notes.iter().any(|n| n.text.contains("did not compare")),
            "{s}: {:#?}",
            u.notes
        );
    }
    let u = unit(&c, "SET @Count = @Count * 2");
    assert_eq!(u.checked, Status::Converted, "{:#?}", u.notes);
}

#[test]
fn postgres_setup_block_beside_the_procedure_keeps_the_body_compared() {
    let c = convert(OLA, Dialect::Tsql, Dialect::Postgres).unwrap();
    assert!(c.output.contains("DO $$"), "{}", c.output);
    // Before: the DO block made "several routines in one artifact", and no
    // statement of the procedure was compared.
    assert_eq!(unpaired(&c), Vec::<&String>::new());
    for s in [
        "SET @Count = @Mode + 1",
        "SET @Message = @Prefix + N'x'",
        "SET @Count = @Count * 2",
    ] {
        let u = unit(&c, s);
        assert_eq!(u.checked, Status::Converted, "{s}: {:#?}", u.notes);
        assert!(
            matches!(u.status, Status::Converted | Status::Assumptions),
            "{s}: {:#?}",
            u.notes
        );
    }
}
