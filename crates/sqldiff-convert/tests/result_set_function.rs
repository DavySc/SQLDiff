//! A T-SQL procedure that returns one result set becomes a PostgreSQL
//! function RETURNS TABLE (Davy's decision of 2026-10-30): the SELECT, or
//! the data change with OUTPUT but no INTO, becomes its one RETURN QUERY,
//! each column cast to the type the function declares, which the engine
//! compares with the source. Any other procedure with a result set stays a
//! procedure, its result set refused with the reason.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.Item (ItemID int IDENTITY(1,1) NOT NULL PRIMARY KEY, ItemName nvarchar(50) NOT NULL, Qty int NULL, Price decimal(18,2) NULL);
CREATE TABLE dbo.[ETL Cutoff] ([Table Name] sysname NOT NULL PRIMARY KEY, [Cutoff Time] datetime2(7) NOT NULL);
";

fn convert(src: &str) -> Conversion {
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let to = Dialect::Postgres;
    let target = rewrite_in(TABLES, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(src, rewrite_in(src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn proc(body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p @ItemID int\nAS\nBEGIN\n    SET NOCOUNT ON;\n    {body}\nEND;\n"
    )
}

/// The output, and every unit's status with its notes and the engine's reasons.
fn statuses(c: &Conversion) -> Vec<(Status, String)> {
    c.units
        .iter()
        .map(|u| {
            (
                u.status,
                format!(
                    "{:?} {:?} {:?}",
                    u.notes.iter().map(|n| &n.text).collect::<Vec<_>>(),
                    c.report.unknown_reasons,
                    c.report
                        .findings
                        .iter()
                        .map(|f| &f.witness)
                        .collect::<Vec<_>>()
                ),
            )
        })
        .collect()
}

fn under_assumptions(c: &Conversion) {
    for (s, why) in statuses(c) {
        assert!(
            matches!(s, Status::Assumptions | Status::Converted),
            "{s:?}: {why}\n{}",
            c.output
        );
    }
    assert!(
        c.units
            .iter()
            .any(|u| u.notes.iter().any(|n| n.rule == "ASM-RESULT-SET-FUNCTION")),
        "{}",
        c.output
    );
}

#[test]
fn a_select_becomes_return_query_with_declared_casts() {
    let c = convert(&proc(
        "SELECT i.ItemID, i.ItemName AS Name, i.Price\n    FROM dbo.Item AS i\n    WHERE i.ItemID = @ItemID\n    ORDER BY i.ItemID;",
    ));
    let out = c.output.as_str();
    for want in [
        "CREATE OR REPLACE FUNCTION p(p_item_id integer)\nRETURNS TABLE (ItemID integer, Name varchar(50), Price numeric(18, 2))\nLANGUAGE plpgsql\nAS $$\n#variable_conflict use_column\n",
        "RETURN QUERY SELECT CAST(i.ItemID AS integer) AS ItemID, CAST(i.ItemName AS varchar(50)) AS Name, CAST(i.Price AS numeric(18, 2)) AS Price FROM Item AS i",
    ] {
        assert!(out.contains(want), "{out}");
    }
    under_assumptions(&c);
}

#[test]
fn quoted_names_and_name_equals_forms() {
    let c = convert(
        "CREATE PROCEDURE dbo.Cutoff @TableName sysname\nAS\nBEGIN\n    SELECT CutoffTime = [Cutoff Time] FROM dbo.[ETL Cutoff] WHERE [Table Name] = @TableName;\nEND;\n",
    );
    assert!(
        c.output.contains("RETURNS TABLE (CutoffTime timestamp)")
            && c.output.contains("RETURN QUERY SELECT CAST(\"Cutoff Time\" AS timestamp) AS CutoffTime FROM \"ETL Cutoff\""),
        "{}",
        c.output
    );
}

#[test]
fn output_without_into_becomes_return_query_returning() {
    for (body, header, ret) in [
        (
            "SET XACT_ABORT ON;\n    INSERT dbo.Item (ItemName, Qty)\n    OUTPUT inserted.ItemID AS NewID\n    VALUES (N'x', @ItemID);\n    RETURN 0;",
            "RETURNS TABLE (NewID integer)",
            "RETURN QUERY INSERT INTO Item (ItemName, Qty)\n  VALUES ('x', p_item_id)\n  RETURNING CAST(new.ItemID AS integer);\n  RETURN;",
        ),
        (
            "UPDATE dbo.Item SET Qty = Qty + 1 OUTPUT deleted.Qty AS OldQty, inserted.Qty WHERE ItemID = @ItemID;",
            "RETURNS TABLE (OldQty integer, Qty integer)",
            "RETURN QUERY UPDATE Item SET Qty = Qty + 1 WHERE ItemID = p_item_id\n  RETURNING CAST(old.Qty AS integer), CAST(new.Qty AS integer);",
        ),
        (
            "DELETE FROM dbo.Item OUTPUT deleted.ItemID, deleted.ItemName WHERE ItemID = @ItemID;",
            "RETURNS TABLE (ItemID integer, ItemName varchar(50))",
            "RETURN QUERY DELETE FROM Item WHERE ItemID = p_item_id\n  RETURNING CAST(old.ItemID AS integer), CAST(old.ItemName AS varchar(50));",
        ),
    ] {
        let c = convert(&proc(body));
        assert!(
            c.output.contains(header) && c.output.contains(ret),
            "{body}\n{}",
            c.output
        );
        under_assumptions(&c);
    }
}

#[test]
fn other_result_sets_stay_refused_with_the_reason() {
    for (body, why) in [
        (
            "SELECT ItemID FROM dbo.Item;\n    SELECT ItemName FROM dbo.Item;",
            "it returns 2 result sets",
        ),
        (
            "IF @ItemID > 0\n        SELECT ItemID FROM dbo.Item;",
            "inside an IF, a loop or TRY … CATCH",
        ),
        (
            "SELECT ItemID FROM dbo.Item;\n    RETURN 2;",
            "a status other than 0",
        ),
        (
            "SELECT ISNULL(ItemName, N'') AS Label FROM dbo.Item;",
            "result column 1 (Label) is not a column",
        ),
        ("SELECT ItemID + 1 FROM dbo.Item;", "result column 1 has no name"),
        (
            "UPDATE dbo.Item SET Qty = 0 WHERE ItemID = @ItemID;\n    DELETE FROM dbo.Item WHERE Qty IS NULL;\n    SELECT ItemID FROM dbo.Item;",
            "writes before the statement that returns the rows",
        ),
        (
            "SET XACT_ABORT ON;\n    UPDATE dbo.Item SET Qty = 0 WHERE ItemID = @ItemID;\n    SELECT ItemID FROM dbo.Item WHERE ItemID = 1 / @ItemID;",
            "writes before the statement that returns the rows",
        ),
        (
            "SET XACT_ABORT ON;\n    BEGIN TRAN;\n    UPDATE dbo.Item SET Qty = 0 WHERE ItemID = @ItemID;\n    COMMIT;\n    SELECT ItemID FROM dbo.Item;",
            "writes before the statement that returns the rows",
        ),
        (
            "SELECT ItemID FROM dbo.Item;\n    UPDATE dbo.Item SET Qty = 0 WHERE ItemID = @ItemID;",
            "runs statements after the one that returns the rows",
        ),
        (
            "SELECT ItemID FROM dbo.Item UNION ALL SELECT ItemID FROM dbo.Item;",
            "built with UNION",
        ),
        (
            "SELECT ItemID FROM dbo.Item FOR JSON AUTO, ROOT(N'Items');",
            "built with FOR JSON",
        ),
        (
            "INSERT dbo.Item (ItemName) OUTPUT inserted.ItemID + 1 AS n VALUES (N'x');",
            "only a column of inserted or deleted",
        ),
    ] {
        let c = convert(&proc(body));
        assert!(
            c.output.contains("CREATE OR REPLACE PROCEDURE")
                && c.output.contains("not made a function RETURNS TABLE because")
                && c.output.contains(why),
            "{body}\n{}",
            c.output
        );
    }
    // OUTPUT parameters.
    let c = convert(
        "CREATE PROCEDURE dbo.p @ItemID int, @n int OUTPUT\nAS\nBEGIN\n    SELECT ItemID FROM dbo.Item;\nEND;\n",
    );
    assert!(
        c.output.contains("it has OUTPUT parameters"),
        "{}",
        c.output
    );
}

/// One transaction around the writes and the result set, under
/// SET XACT_ABORT ON, fails as a whole, as the function does.
#[test]
fn a_transaction_spanning_the_result_set_converts() {
    let c = convert(&proc(
        "SET XACT_ABORT ON;\n    BEGIN TRAN;\n    UPDATE dbo.Item SET Qty = 0 WHERE ItemID = @ItemID;\n    SELECT ItemID FROM dbo.Item WHERE ItemID = @ItemID;\n    COMMIT;",
    ));
    assert!(
        c.output.contains("RETURNS TABLE (ItemID integer)")
            && c.output.contains("RETURN QUERY SELECT"),
        "{}",
        c.output
    );
    under_assumptions(&c);
}
