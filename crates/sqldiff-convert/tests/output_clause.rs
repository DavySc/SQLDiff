//! T-SQL `OUTPUT … INTO` becomes a PostgreSQL data-modifying CTE whose
//! RETURNING rows an INSERT writes (`inserted` → `new`, `deleted` → `old`,
//! PostgreSQL 18), which the engine compares with the source; OUTPUT without
//! INTO and the forms the engine does not compare are refused.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.item (item_id int NOT NULL PRIMARY KEY, name nvarchar(100) NOT NULL, qty int NULL);
CREATE TABLE dbo.item_log (item_id int NULL, old_qty int NULL, new_qty int NULL);
";

fn convert_to(stmt: &str, to: Dialect) -> Conversion {
    let src = format!("{stmt}\n");
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let target = rewrite_in(TABLES, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(&src, rewrite_in(&src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn last(c: &Conversion) -> (&str, &sqldiff_convert::Unit) {
    let u = c.units.last().unwrap();
    (&c.output[u.out.start..u.out.end], u)
}

#[test]
fn output_into_becomes_returning_into_an_insert() {
    let cases = [
        (
            "UPDATE dbo.item SET qty = 0 OUTPUT inserted.item_id, deleted.qty AS was, inserted.qty INTO dbo.item_log (item_id, old_qty, new_qty) WHERE item_id = 1;",
            "WITH sqldiff_output AS (\n  UPDATE item SET qty = 0 WHERE item_id = 1\n  RETURNING new.item_id AS item_id, old.qty AS old_qty, new.qty AS new_qty\n)\nINSERT INTO item_log (item_id, old_qty, new_qty)\nSELECT item_id, old_qty, new_qty FROM sqldiff_output;",
        ),
        (
            "DELETE FROM dbo.item OUTPUT deleted.item_id, deleted.qty INTO dbo.item_log (item_id, old_qty) WHERE qty = 0;",
            "WITH sqldiff_output AS (\n  DELETE FROM item WHERE qty = 0\n  RETURNING old.item_id AS item_id, old.qty AS old_qty\n)\nINSERT INTO item_log (item_id, old_qty)\nSELECT item_id, old_qty FROM sqldiff_output;",
        ),
        (
            "INSERT INTO dbo.item (item_id, name) OUTPUT inserted.item_id, 5 INTO dbo.item_log (item_id, new_qty) VALUES (7, N'x');",
            "WITH sqldiff_output AS (\n  INSERT INTO item (item_id, name) VALUES (7, 'x')\n  RETURNING new.item_id AS item_id, 5 AS new_qty\n)\nINSERT INTO item_log (item_id, new_qty)\nSELECT item_id, new_qty FROM sqldiff_output;",
        ),
    ];
    for (stmt, want) in cases {
        let c = convert_to(stmt, Dialect::Postgres);
        let (out, u) = last(&c);
        assert!(out.contains(want), "{stmt}\n{out}");
        assert_eq!(
            u.status,
            Status::Assumptions,
            "{stmt}: {:?}\n{:?}\n{:#?}",
            u.notes,
            c.report.unknown_reasons,
            c.report.findings
        );
        assert!(
            u.notes.iter().any(|n| n.rule == "ASM-OUTPUT-INTO-TARGET"),
            "{:?}",
            u.notes
        );
    }
}

#[test]
fn output_into_a_table_variable_writes_its_temp_table() {
    let c = convert_to(
        "DECLARE @log TABLE (item_id int, qty int);\nDELETE FROM dbo.item OUTPUT deleted.item_id, deleted.qty INTO @log (item_id, qty) WHERE qty = 0;",
        Dialect::Postgres,
    );
    let (out, u) = last(&c);
    assert!(
        out.contains("INSERT INTO pg_temp.tv__log (item_id, qty)")
            && out.contains("SELECT item_id, qty FROM sqldiff_output;"),
        "{out}"
    );
    assert!(
        u.status < Status::Unproven,
        "{:?}\n{:?}",
        u.notes,
        c.report.unknown_reasons
    );
}

#[test]
fn other_output_clauses_are_refused() {
    for (stmt, why) in [
        (
            "INSERT INTO dbo.item (item_id, name) OUTPUT inserted.item_id VALUES (7, N'x');",
            "cannot return a result set",
        ),
        (
            "DELETE FROM dbo.item OUTPUT deleted.item_id INTO dbo.item_log WHERE qty = 0;",
            "without a column list",
        ),
        (
            "DELETE FROM dbo.item OUTPUT inserted.item_id INTO dbo.item_log (item_id) WHERE qty = 0;",
            "inserted in a DELETE",
        ),
        (
            "UPDATE dbo.item SET qty = 0 OUTPUT inserted.qty + 1 INTO dbo.item_log (new_qty);",
            "an item other than",
        ),
        (
            "DELETE dbo.item WITH (TABLOCK) OUTPUT deleted.item_id INTO dbo.item_log (item_id) WHERE qty = 0;",
            "table hints",
        ),
        (
            "UPDATE i SET qty = 0 OUTPUT inserted.qty INTO dbo.item_log (new_qty) FROM dbo.item i JOIN dbo.item_log l ON l.item_id = i.item_id;",
            "FROM list",
        ),
    ] {
        let c = convert_to(stmt, Dialect::Postgres);
        let (_, u) = last(&c);
        assert_eq!(u.status, Status::NotConverted, "{stmt}");
        assert!(
            u.notes.iter().any(|n| n.text.contains(why)),
            "{stmt}: {:?}",
            u.notes
        );
    }
    // Db2 for i keeps its own reason.
    let c = convert_to(
        "DELETE FROM dbo.item OUTPUT deleted.item_id INTO dbo.item_log (item_id) WHERE qty = 0;",
        Dialect::Db2i,
    );
    assert_eq!(last(&c).1.status, Status::NotConverted);
}
