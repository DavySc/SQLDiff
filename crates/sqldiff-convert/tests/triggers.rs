//! T-SQL AFTER triggers become PostgreSQL statement triggers (rules T1-T3):
//! one function and one `FOR EACH STATEMENT` trigger per event, transition
//! tables named `inserted` and `deleted`, an image the event lacks as an
//! empty query of the table, and the row-count guard as a test for rows.
//! The engine compares each event's function with the T-SQL body for that
//! event (rule T5), so a converted trigger counts as converted under
//! assumptions only when the engine showed it equal.

use sqldiff_convert::{check, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.orders (order_id int NOT NULL PRIMARY KEY, qty int NOT NULL, price decimal(10,2) NOT NULL, total decimal(12,2) NULL);
CREATE TABLE dbo.order_items (item_id int NOT NULL PRIMARY KEY, order_id int NOT NULL, amount decimal(10,2) NOT NULL);
CREATE TABLE dbo.audit_log (order_id int NULL, action varchar(10) NULL, qty int NULL);
";

/// Converts `stmt` with the tables as the catalogs of both sides.
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

fn tsql(stmt: &str) -> Conversion {
    convert_to(stmt, Dialect::Postgres)
}

/// The unit that holds the trigger.
fn unit(c: &Conversion) -> &Unit {
    c.units.last().unwrap()
}

fn has_note(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

/// A converted trigger the engine showed equal: under assumptions (the
/// trigger's own ones at least).
fn assert_proven(stmt: &str, c: &Conversion) {
    let u = unit(c);
    assert!(
        c.report.unknown_reasons.is_empty(),
        "{stmt}\n{:#?}",
        c.report.unknown_reasons
    );
    assert_eq!(
        u.status,
        Status::Assumptions,
        "{stmt}\n{:?}\n{:#?}\n{}",
        u.notes,
        c.report.findings,
        c.output
    );
}

#[test]
fn an_update_trigger_becomes_a_statement_trigger() {
    let stmt = "CREATE TRIGGER dbo.orders_tu ON dbo.orders FOR UPDATE AS
BEGIN
  IF @@ROWCOUNT = 0 RETURN;
  SET NOCOUNT ON;
  UPDATE o SET total = i.qty * i.price
  FROM dbo.orders o JOIN inserted i ON o.order_id = i.order_id
  LEFT JOIN deleted d ON d.order_id = i.order_id
  WHERE i.qty <> d.qty OR i.price <> d.price;
END";
    let c = tsql(stmt);
    let want = "CREATE OR REPLACE FUNCTION orders_tu() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM inserted) THEN
    RETURN NULL;
  END IF;
  -- SET NOCOUNT ON: no PostgreSQL equivalent needed
  UPDATE orders o SET total = i.qty * i.price FROM inserted i LEFT JOIN deleted d ON d.order_id = i.order_id WHERE (o.order_id = i.order_id) AND (i.qty <> d.qty OR i.price <> d.price);
  RETURN NULL;
END;
$$;

CREATE OR REPLACE TRIGGER orders_tu
AFTER UPDATE ON orders
REFERENCING NEW TABLE AS inserted OLD TABLE AS deleted
FOR EACH STATEMENT
EXECUTE FUNCTION orders_tu();
";
    assert!(c.output.contains(want), "{}", c.output);
    assert_proven(stmt, &c);
    let u = unit(&c);
    for rule in [
        "ASM-TRIGGER-ROWCOUNT",
        "ASM-TRIGGER-ERROR",
        "ASM-TRIGGER-ORDER",
    ] {
        assert!(has_note(u, rule), "{rule}: {:?}", u.notes);
    }
    // The body changes its own table: PostgreSQL fires the trigger again,
    // which the engine shows does nothing.
    assert!(
        c.report
            .assumptions
            .iter()
            .any(|a| a.id == "ASM-TRIGGER-REENTRY-OTHERS"),
        "{:?}",
        c.report.assumptions
    );
}

#[test]
fn several_events_become_one_trigger_each() {
    let stmt = "CREATE TRIGGER orders_audit ON orders AFTER INSERT, DELETE AS
SET NOCOUNT ON;
INSERT INTO audit_log (order_id, action, qty)
SELECT i.order_id, 'ins', i.qty FROM inserted i
WHERE NOT EXISTS (SELECT 1 FROM deleted d WHERE d.order_id = i.order_id);
INSERT INTO audit_log (order_id, action, qty) SELECT order_id, 'del', qty FROM deleted;";
    let c = tsql(stmt);
    for want in [
        // INSERT: no OLD TABLE, `deleted` is an empty query of the table.
        "CREATE OR REPLACE FUNCTION orders_audit_insert() RETURNS trigger",
        "FROM inserted i\n  WHERE NOT EXISTS (SELECT 1 FROM (SELECT * FROM orders WHERE false) d WHERE",
        "SELECT order_id, 'del', qty FROM (SELECT * FROM orders WHERE false) AS deleted;",
        "AFTER INSERT ON orders\nREFERENCING NEW TABLE AS inserted\nFOR EACH STATEMENT\nEXECUTE FUNCTION orders_audit_insert();",
        // DELETE: no NEW TABLE, `inserted` is the empty one.
        "CREATE OR REPLACE FUNCTION orders_audit_delete() RETURNS trigger",
        "FROM (SELECT * FROM orders WHERE false) i\n  WHERE NOT EXISTS (SELECT 1 FROM deleted d WHERE",
        "AFTER DELETE ON orders\nREFERENCING OLD TABLE AS deleted\nFOR EACH STATEMENT\nEXECUTE FUNCTION orders_audit_delete();",
    ] {
        assert!(c.output.contains(want), "{want}\n---\n{}", c.output);
    }
    assert!(!c.output.contains("orders_audit_update"), "{}", c.output);
    assert_proven(stmt, &c);
    let u = unit(&c);
    assert!(has_note(u, "pg.trigger-per-event"), "{:?}", u.notes);
}

#[test]
fn a_bare_return_returns_null() {
    let stmt = "CREATE TRIGGER t ON orders AFTER DELETE AS
BEGIN
  IF NOT EXISTS (SELECT 1 FROM deleted) RETURN;
  INSERT INTO audit_log (order_id, action, qty) SELECT order_id, 'del', qty FROM deleted;
END";
    let c = tsql(stmt);
    assert!(
        c.output
            .contains("IF NOT EXISTS (SELECT 1 FROM deleted) THEN\n    RETURN NULL;\n  END IF;"),
        "{}",
        c.output
    );
    assert_proven(stmt, &c);
}

#[test]
fn the_row_count_guard_tests_the_events_rows() {
    for (stmt, image) in [
        ("CREATE TRIGGER t ON orders AFTER INSERT AS IF @@ROWCOUNT = 0 RETURN; INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "inserted"),
        ("CREATE TRIGGER t ON orders AFTER DELETE AS IF (@@ROWCOUNT < 1) BEGIN RETURN; END INSERT INTO audit_log (order_id) SELECT order_id FROM deleted;", "deleted"),
    ] {
        let c = tsql(stmt);
        let want = format!("IF NOT EXISTS (SELECT 1 FROM {image}) THEN\n    RETURN NULL;\n  END IF;");
        assert!(c.output.contains(&want), "{stmt}\n{}", c.output);
        assert_proven(stmt, &c);
    }
}

#[test]
fn triggers_without_a_rule_are_refused() {
    for (stmt, why) in [
        // SET NOCOUNT ON resets @@ROWCOUNT: only a first guard is a guard.
        ("CREATE TRIGGER t ON orders AFTER INSERT AS SET NOCOUNT ON; IF @@ROWCOUNT = 0 RETURN; INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "@@ROWCOUNT"),
        ("CREATE TRIGGER t ON orders INSTEAD OF INSERT AS INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "INSTEAD OF"),
        ("CREATE TRIGGER t ON orders AFTER UPDATE AS IF UPDATE(qty) INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "UPDATE(column)"),
        ("CREATE TRIGGER t ON orders AFTER UPDATE AS IF COLUMNS_UPDATED() > 0 INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "COLUMNS_UPDATED"),
        ("CREATE TRIGGER t ON orders AFTER INSERT AS IF EXISTS (SELECT 1 FROM inserted WHERE qty < 0) BEGIN ROLLBACK; THROW 50000, 'neg', 1; END", "transaction control"),
        ("CREATE TRIGGER t ON orders AFTER INSERT AS IF TRIGGER_NESTLEVEL() > 1 RETURN; INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "TRIGGER_NESTLEVEL"),
        ("CREATE TRIGGER t ON DATABASE FOR CREATE_TABLE AS PRINT 'x';", "DDL triggers"),
        ("CREATE TRIGGER t ON orders WITH EXECUTE AS OWNER AFTER INSERT AS INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;", "EXECUTE AS"),
        // An order meets several items: no unique match.
        ("CREATE TRIGGER t ON order_items AFTER UPDATE AS UPDATE o SET total = i.amount FROM orders o JOIN inserted i ON i.order_id = o.order_id;", "at most one row"),
        // A statement that is refused refuses the whole trigger.
        ("CREATE TRIGGER t ON orders AFTER INSERT AS INSERT INTO audit_log (order_id) SELECT order_id FROM inserted; RETURN 1;", ""),
    ] {
        let c = tsql(stmt);
        let u = unit(&c);
        assert_eq!(u.status, Status::NotConverted, "{stmt}\n{}", c.output);
        assert!(
            u.notes.iter().any(|n| n.text.contains(why)),
            "{stmt}: {:?}",
            u.notes
        );
        assert!(!c.output.contains("CREATE OR REPLACE TRIGGER"), "{stmt}\n{}", c.output);
    }
}

#[test]
fn alter_trigger_replaces_the_triggers() {
    let stmt = "ALTER TRIGGER t ON orders AFTER INSERT, UPDATE AS INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;";
    let c = tsql(stmt);
    assert!(
        c.output
            .contains("CREATE OR REPLACE TRIGGER t_insert\nAFTER INSERT ON orders"),
        "{}",
        c.output
    );
    assert!(c.output.contains("CREATE OR REPLACE TRIGGER t_update\nAFTER UPDATE ON orders\nREFERENCING NEW TABLE AS inserted OLD TABLE AS deleted"), "{}", c.output);
    assert_proven(stmt, &c);
    assert!(
        has_note(unit(&c), "ASM-TRIGGER-REPLACE"),
        "{:?}",
        unit(&c).notes
    );
}

#[test]
fn db2_refuses_triggers() {
    let c = convert_to(
        "CREATE TRIGGER t ON orders AFTER INSERT AS INSERT INTO audit_log (order_id) SELECT order_id FROM inserted;",
        Dialect::Db2i,
    );
    assert_eq!(unit(&c).status, Status::NotConverted);
}

/// An UPDATE trigger that updates its own table without a test that the row
/// changed: PostgreSQL fires it again and updates again, T-SQL does not.
#[test]
fn unproven_reentry_is_not_proven() {
    let stmt = "CREATE TRIGGER t ON orders AFTER UPDATE AS
IF @@ROWCOUNT = 0 RETURN;
UPDATE orders SET total = qty * price WHERE order_id IN (SELECT order_id FROM inserted);";
    let c = tsql(stmt);
    assert_eq!(
        unit(&c).status,
        Status::Unproven,
        "{}\n{:?}\n{:#?}",
        c.output,
        unit(&c).notes,
        c.report.unknown_reasons
    );
    assert!(
        c.report
            .unknown_reasons
            .iter()
            .any(|r| r.contains("updates its own table")),
        "{:#?}",
        c.report.unknown_reasons
    );
}
