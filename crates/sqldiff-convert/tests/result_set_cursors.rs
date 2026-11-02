//! A T-SQL procedure whose result sets a function RETURNS TABLE cannot take
//! (several result sets, writes before the SELECT, a UNION or a CTE, columns
//! without a catalog type, a status code) becomes a PostgreSQL procedure with
//! one INOUT refcursor parameter `result_set_N` per result set, each opened
//! by `OPEN result_set_N FOR query`, under ASM-RESULTSET-REFCURSOR (Davy's
//! decision D8 of 2026-11-02; the probes are in eval/refcursor). Shapes where
//! the cursor would not read what T-SQL sends stay refused with the reason.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.Item (ItemID int NOT NULL PRIMARY KEY, ItemName nvarchar(50) NOT NULL, Qty int NOT NULL);
CREATE TABLE dbo.Log (LogID int NOT NULL PRIMARY KEY, Note nvarchar(50) NOT NULL);
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
        "CREATE PROCEDURE dbo.p @ItemID int\nAS\nBEGIN\n    SET NOCOUNT ON;\n    SET XACT_ABORT ON;\n    {body}\nEND;\n"
    )
}

fn report(c: &Conversion) -> String {
    format!(
        "{}\n{:?}\n{:?}",
        c.output,
        c.units
            .iter()
            .map(|u| (
                u.status,
                u.notes
                    .iter()
                    .map(|n| n.text.to_string())
                    .collect::<Vec<_>>()
            ))
            .collect::<Vec<_>>(),
        c.report.unknown_reasons
    )
}

fn converted(c: &Conversion) {
    for u in &c.units {
        assert!(
            matches!(u.status, Status::Assumptions | Status::Converted),
            "{:?}\n{}",
            u.status,
            report(c)
        );
    }
    assert!(
        c.units
            .iter()
            .any(|u| u.notes.iter().any(|n| n.rule == "ASM-RESULTSET-REFCURSOR")),
        "{}",
        report(c)
    );
    assert!(c.unproven.is_empty(), "{:?}\n{}", c.unproven, report(c));
}

/// The procedure was refused, with `why` in the reason.
fn refused(c: &Conversion, why: &str) {
    assert!(
        !c.output.contains("OPEN result_set_") && !c.output.contains("INOUT result_set_"),
        "{}",
        report(c)
    );
    let all: String = c
        .units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| n.text.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains(why), "want `{why}` in\n{all}\n{}", report(c));
    assert!(c.units.iter().any(|u| u.status == Status::NotConverted));
}

#[test]
fn several_result_sets_and_a_write_become_cursors_in_order() {
    let c = convert(&proc(
        "UPDATE dbo.Item SET Qty = Qty + 1 WHERE ItemID = @ItemID;\n    SELECT ItemID, ItemName FROM dbo.Item WHERE ItemID = @ItemID ORDER BY ItemID;\n    SELECT Qty, ItemName FROM dbo.Item ORDER BY Qty, ItemID;",
    ));
    let out = c.output.as_str();
    for want in [
        "CREATE OR REPLACE PROCEDURE p(p_item_id integer, INOUT result_set_1 refcursor DEFAULT NULL, INOUT result_set_2 refcursor DEFAULT NULL)",
        "OPEN result_set_1 FOR SELECT ItemID, ItemName FROM Item WHERE ItemID = p_item_id ORDER BY ItemID;",
        "OPEN result_set_2 FOR SELECT Qty, ItemName FROM Item ORDER BY Qty, ItemID;",
    ] {
        assert!(out.contains(want), "{want}\n{out}");
    }
    converted(&c);
}

#[test]
fn a_cte_a_union_and_name_equals_items_convert() {
    let c = convert(&proc(
        "WITH big AS (SELECT ItemID, Qty FROM dbo.Item WHERE Qty > 10)\n    SELECT ItemID, Qty FROM big ORDER BY ItemID;\n    SELECT ItemID FROM dbo.Item UNION ALL SELECT LogID FROM dbo.Log;\n    SELECT Total = Qty + 1, Label = ItemName FROM dbo.Item ORDER BY ItemID;",
    ));
    let out = c.output.as_str();
    assert!(out.contains("OPEN result_set_1 FOR WITH big AS"), "{out}");
    assert!(
        out.contains(
            "OPEN result_set_2 FOR SELECT ItemID FROM Item UNION ALL SELECT LogID FROM Log;"
        ),
        "{out}"
    );
    assert!(
        out.contains("Qty + 1 AS Total") && out.contains("ItemName AS Label"),
        "{out}"
    );
    assert!(!out.contains("Total ="), "{out}");
    converted(&c);
}

#[test]
fn a_status_code_stays_the_last_parameter() {
    let c = convert(&proc(
        "SELECT ItemID FROM dbo.Item WHERE ItemID = @ItemID ORDER BY ItemID;\n    RETURN 3;",
    ));
    assert!(
        c.output.contains(
            "INOUT result_set_1 refcursor DEFAULT NULL, INOUT return_code integer DEFAULT NULL)"
        ),
        "{}",
        c.output
    );
}

#[test]
fn what_a_function_returns_table_can_take_stays_a_function() {
    let c = convert(&proc(
        "SELECT i.ItemID, i.ItemName FROM dbo.Item AS i WHERE i.ItemID = @ItemID ORDER BY i.ItemID;",
    ));
    assert!(
        c.output.contains("RETURNS TABLE") && !c.output.contains("refcursor"),
        "{}",
        c.output
    );
}

#[test]
fn shapes_where_a_cursor_would_read_differently_are_refused() {
    let q = "SELECT ItemID FROM dbo.Item ORDER BY ItemID;";
    // A write before the SELECT is fine; these after or inside it are not.
    for (body, why) in [
        (
            format!("{q}\n    DROP TABLE dbo.Log;"),
            "DROP follows the first result set",
        ),
        (
            format!("{q}\n    TRUNCATE TABLE dbo.Log;"),
            "TRUNCATE follows the first result set",
        ),
        (
            format!("{q}\n    EXEC dbo.other;"),
            "EXEC follows the first result set",
        ),
        (
            format!("BEGIN TRAN;\n    {q}\n    SELECT Qty FROM dbo.Item;\n    COMMIT;"),
            "controls its transaction",
        ),
        (
            format!("IF @ItemID > 0\n    BEGIN\n    {q}\n    END\n    SELECT Qty FROM dbo.Item;"),
            "inside an IF, a loop or TRY",
        ),
        (
            "SELECT ItemID, GETDATE() AS d FROM dbo.Item;\n    SELECT Qty FROM dbo.Item;"
                .to_string(),
            "GETDATE",
        ),
        (
            "SELECT ItemID, NEWID() AS g FROM dbo.Item;\n    SELECT Qty FROM dbo.Item;".to_string(),
            "NEWID",
        ),
        (
            "SELECT dbo.f(Qty) AS x FROM dbo.Item;\n    SELECT Qty FROM dbo.Item;".to_string(),
            "a user function",
        ),
        (
            "SELECT ItemID, @@ROWCOUNT AS n FROM dbo.Item;\n    SELECT Qty FROM dbo.Item;"
                .to_string(),
            "@@ROWCOUNT",
        ),
        (
            "SELECT ItemID FROM dbo.Item FOR JSON PATH;\n    SELECT Qty FROM dbo.Item;".to_string(),
            "FOR JSON",
        ),
        (
            "SELECT ItemID FROM dbo.Item OPTION (MAXDOP 1);\n    SELECT Qty FROM dbo.Item;"
                .to_string(),
            "OPTION clause",
        ),
    ] {
        refused(&convert(&proc(&body)), why);
    }
}
