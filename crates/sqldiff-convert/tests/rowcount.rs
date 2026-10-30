//! `IF @@ROWCOUNT = 0` (`<> 0`, `> 0`; `= 1` after a SELECT TOP 1) right
//! after the INSERT, UPDATE, DELETE or variable-assigning SELECT it reads
//! becomes `IF NOT FOUND` / `IF FOUND` (ASM-ROWCOUNT-FOUND), and the engine
//! proves the IF; any other use of @@ROWCOUNT is refused with the reason.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

fn src(body: &str) -> String {
    format!(
        "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);
GO
CREATE PROCEDURE dbo.p @id int, @v int
AS
BEGIN
    SET NOCOUNT ON;
    SET XACT_ABORT ON;
{body}
END;
"
    )
}

fn pg(body: &str) -> Conversion {
    convert(&src(body), Dialect::Tsql, Dialect::Postgres).unwrap()
}

/// The unit of the IF that reads @@ROWCOUNT.
fn if_unit<'c>(c: &'c Conversion, body: &str) -> &'c sqldiff_convert::Unit {
    let line = src(body)
        .lines()
        .position(|l| l.contains("@@ROWCOUNT"))
        .unwrap()
        + 1;
    c.units
        .iter()
        .find(|u| u.src.line == line)
        .unwrap_or_else(|| panic!("no unit on line {line}\n{}", c.output))
}

#[track_caller]
fn found(body: &str, want: &str) {
    let c = pg(body);
    assert!(c.output.contains(want), "{want}\n{}", c.output);
    let u = if_unit(&c, body);
    assert_eq!(u.checked, Status::Converted, "{:?}\n{}", u.notes, c.output);
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    assert!(
        u.notes.iter().any(|n| n.rule == "ASM-ROWCOUNT-FOUND"),
        "{:?}",
        u.notes
    );
}

#[track_caller]
fn refused(body: &str, why: &str) {
    let c = pg(body);
    let u = if_unit(&c, body);
    assert_eq!(u.status, Status::NotConverted, "{}", c.output);
    assert!(
        u.notes.iter().any(|n| n.text.contains(why)),
        "{:?}",
        u.notes
    );
}

#[test]
fn rowcount_right_after_the_statement_becomes_found() {
    found(
        "    UPDATE dbo.t SET v = @v WHERE id = @id;\n    IF @@ROWCOUNT = 0\n        INSERT INTO dbo.t (id, v) VALUES (@id, @v);",
        "IF NOT FOUND THEN",
    );
    found(
        "    DELETE FROM dbo.t WHERE id = @id;\n    IF @@ROWCOUNT > 0 RETURN;",
        "IF FOUND THEN",
    );
    found(
        "    SELECT TOP 1 @v = v FROM dbo.t WHERE id > @id ORDER BY id;\n    IF @@ROWCOUNT = 0 RETURN;",
        "IF NOT FOUND THEN",
    );
    // At most one row: a count of 1 is a row found.
    found(
        "    SELECT TOP (1) @v = v FROM dbo.t WHERE id > @id ORDER BY id;\n    IF @@ROWCOUNT = 1 RETURN;",
        "IF FOUND THEN",
    );
}

#[test]
fn other_rowcount_reads_are_refused() {
    // SET resets @@ROWCOUNT to 0 in T-SQL; PL/pgSQL leaves FOUND alone.
    refused(
        "    UPDATE dbo.t SET v = @v WHERE id = @id;\n    SET @v = 1;\n    IF @@ROWCOUNT = 0 RETURN;",
        "other than INSERT, UPDATE, DELETE",
    );
    refused(
        "    SELECT @v = v FROM dbo.t WHERE id > @id;\n    IF @@ROWCOUNT = 1 RETURN;",
        "more than one row",
    );
    refused(
        "    SELECT TOP 1 WITH TIES @v = v FROM dbo.t WHERE id > @id ORDER BY v;\n    IF @@ROWCOUNT = 1 RETURN;",
        "more than one row",
    );
    refused(
        "    SELECT TOP 1 PERCENT @v = v FROM dbo.t WHERE id > @id ORDER BY v;\n    IF @@ROWCOUNT = 1 RETURN;",
        "more than one row",
    );
    // A variable in WHERE is a comparison, not an assignment.
    refused(
        "    SELECT v FROM dbo.t WHERE @id = id;\n    IF @@ROWCOUNT = 0 RETURN;",
        "other than INSERT, UPDATE, DELETE",
    );
    refused(
        "    UPDATE dbo.t SET v = @v WHERE id = @id;\n    IF @@ROWCOUNT = 2 RETURN;",
        "GET DIAGNOSTICS",
    );
    // SET XACT_ABORT ON comes right before.
    refused(
        "    IF @@ROWCOUNT = 0 RETURN;",
        "other than INSERT, UPDATE, DELETE",
    );
    refused(
        "    UPDATE dbo.t SET v = @v WHERE id = @id;\n    IF @id > 0\n    BEGIN\n        IF @@ROWCOUNT = 0 RETURN;\n    END;",
        "does not follow",
    );
    // A SELECT with no table is an assignment: FOUND is not set.
    refused(
        "    SELECT @v = 1;\n    IF @@ROWCOUNT = 0 RETURN;",
        "other than INSERT, UPDATE, DELETE",
    );
}

/// OPTION (MAXRECURSION n) closing an INSERT … SELECT over a recursive CTE
/// is dropped, as in a SELECT: PostgreSQL has no recursion limit, which
/// ASM-CTE-MAXRECURSION states.
#[test]
fn maxrecursion_after_insert_select_is_dropped() {
    let c = pg(
        "    WITH n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3)
    INSERT INTO dbo.t (id, v) SELECT i, i FROM n
    OPTION (MAXRECURSION 0);",
    );
    assert!(!c.output.contains("OPTION (MAXRECURSION"), "{}", c.output);
    assert!(c.output.contains("WITH RECURSIVE"), "{}", c.output);
}
