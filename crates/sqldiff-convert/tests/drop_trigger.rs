//! T-SQL `DROP TRIGGER` → PostgreSQL `DROP TRIGGER … ON table` plus the
//! trigger function: the table comes from the tree's CREATE TRIGGER.

use std::sync::Arc;

use sqldiff_convert::{rewrite, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Dialect, Options};

const TABLE: &str =
    "CREATE TABLE Sales.Orders (OrderID int NOT NULL PRIMARY KEY, Total int NULL);\nGO\n";
const TRIG: &str = "CREATE TRIGGER Sales.TR_Orders ON Sales.Orders AFTER INSERT AS\nBEGIN\n    SET NOCOUNT ON;\nEND;\nGO\n";
const PROC: &str = "CREATE PROCEDURE Sales.DropIt\nAS BEGIN\n    SET XACT_ABORT ON;\n    DROP TRIGGER IF EXISTS [Sales].[TR_Orders];\nEND;\n";

fn catalog(texts: &[&str], d: Dialect) -> Catalog {
    let mut cat = Catalog::default();
    for t in texts {
        let root = sqldiff_syntax::parse(t, d).syntax();
        ddl::load_tree(&mut cat, &root, d);
    }
    cat
}

/// `text` converted with the tree `tree` (T-SQL) as the source catalog, and
/// checked against both trees' catalogs.
fn pg_in(text: &str, tree: &[&str]) -> Conversion {
    let src = catalog(tree, Dialect::Tsql);
    let outs: Vec<String> = tree
        .iter()
        .map(|t| {
            rewrite_in(t, Dialect::Tsql, Dialect::Postgres, Some(&src))
                .output()
                .to_string()
        })
        .collect();
    let target = catalog(
        &outs.iter().map(String::as_str).collect::<Vec<_>>(),
        Dialect::Postgres,
    );
    let d = rewrite_in(text, Dialect::Tsql, Dialect::Postgres, Some(&src));
    let opts = Options {
        facts_a: vec![Arc::new(src)],
        facts_b: vec![Arc::new(target)],
        ..Options::default()
    };
    sqldiff_convert::check(text, d, &opts)
}

fn unit<'a>(c: &'a Conversion, input: &str, needle: &str) -> &'a Unit {
    let at = input.find(needle).unwrap();
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end.max(u.src.start + 1))
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap()
}

fn refused(c: &Conversion, needle: &str) {
    let u = unit(c, PROC, "DROP TRIGGER");
    assert_eq!(u.status, Status::NotConverted, "{}", c.output);
    assert!(
        u.notes.iter().any(|n| n.text.contains(needle)),
        "{needle}: {:?}",
        u.notes
    );
}

#[test]
fn the_table_comes_from_the_trees_create_trigger() {
    let c = pg_in(PROC, &[TABLE, TRIG, PROC]);
    assert!(
        c.output.contains(
            "DROP TRIGGER IF EXISTS TR_Orders ON Sales.Orders;\n  DROP FUNCTION IF EXISTS Sales.TR_Orders();"
        ),
        "{}",
        c.output
    );
    let u = unit(&c, PROC, "DROP TRIGGER");
    assert_eq!(u.status, Status::Assumptions, "{:?}\n{}", u.notes, c.output);
    assert!(u.notes.iter().any(|n| n.rule == "ASM-TRIGGER-DROP"));
}

#[test]
fn dbo_and_unqualified_names() {
    let table = "CREATE TABLE dbo.Orders (OrderID int NOT NULL PRIMARY KEY);\nGO\n";
    let trig = "CREATE TRIGGER TR_Orders ON dbo.Orders AFTER DELETE AS\nBEGIN\n    SET NOCOUNT ON;\nEND;\nGO\n";
    let drop = "DROP TRIGGER TR_Orders;\n";
    let d = rewrite_in(
        drop,
        Dialect::Tsql,
        Dialect::Postgres,
        Some(&catalog(&[table, trig], Dialect::Tsql)),
    );
    assert!(
        d.output()
            .contains("DROP TRIGGER TR_Orders ON Orders;\nDROP FUNCTION IF EXISTS TR_Orders();"),
        "{}",
        d.output()
    );
}

#[test]
fn a_trigger_not_known_uniquely_is_refused() {
    // Not in the tree.
    refused(&pg_in(PROC, &[TABLE, PROC]), "no CREATE TRIGGER");
    // Defined twice, on different tables.
    let other = TRIG.replace("ON Sales.Orders", "ON Sales.Lines");
    refused(&pg_in(PROC, &[TABLE, TRIG, &other, PROC]), "more than once");
    // Several events: one PostgreSQL trigger and function per event.
    let multi = TRIG.replace("AFTER INSERT", "AFTER INSERT, UPDATE");
    refused(&pg_in(PROC, &[TABLE, &multi, PROC]), "several events");
    // INSTEAD OF is not converted.
    let instead = TRIG.replace("AFTER INSERT", "INSTEAD OF INSERT");
    refused(&pg_in(PROC, &[TABLE, &instead, PROC]), "INSTEAD OF");
    // A name PostgreSQL needs quoted.
    let spaced = "CREATE TABLE Sales.[Order Lines] (a int);\nGO\nCREATE TRIGGER Sales.TR_Orders ON Sales.[Order Lines] AFTER INSERT AS\nBEGIN\n    SET NOCOUNT ON;\nEND;\nGO\n";
    refused(&pg_in(PROC, &[spaced, PROC]), "needs quotes");
}

#[test]
fn ddl_triggers_and_several_names_are_refused() {
    let cat = catalog(&[TABLE, TRIG], Dialect::Tsql);
    for (src, needle) in [
        ("DROP TRIGGER TR_Orders ON DATABASE;\n", "DDL trigger"),
        (
            "DROP TRIGGER Sales.TR_Orders, Sales.TR_Orders;\n",
            "several triggers",
        ),
    ] {
        let d = rewrite_in(src, Dialect::Tsql, Dialect::Postgres, Some(&cat));
        let c = sqldiff_convert::check(src, d, &Options::default());
        assert!(
            c.units.iter().any(|u| u.status == Status::NotConverted
                && u.notes.iter().any(|n| n.text.contains(needle))),
            "{needle}: {}",
            c.output
        );
    }
}

#[test]
fn db2_stays_refused() {
    let d = rewrite(PROC, Dialect::Tsql, Dialect::Db2i);
    assert!(!d.output().contains("\n    DROP TRIGGER"), "{}", d.output());
}
