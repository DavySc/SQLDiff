//! A T-SQL AFTER trigger against the PostgreSQL statement triggers it became
//! (trigger rule T5): each event's function is compared with the T-SQL body
//! for that event, the transition tables as the images, and PostgreSQL's
//! re-entry for the trigger's own UPDATE is proven harmless or reported.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const DDL: &str = "CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, qty int NOT NULL, price decimal(10,2) NOT NULL, total decimal(12,2) NULL);
CREATE TABLE order_items (item_id int NOT NULL PRIMARY KEY, order_id int NOT NULL, amount decimal(10,2) NOT NULL);
CREATE TABLE audit_log (order_id int NULL, action varchar(10) NULL, qty int NULL);
";

fn run(tsql: &str, pg: &str) -> Report {
    run_with(tsql, pg, DDL, DDL)
}

fn run_with(tsql: &str, pg: &str, ddl_a: &str, ddl_b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, ddl_a)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, ddl_b)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, tsql),
        &Artifact::new("b.sql", Dialect::Postgres, pg),
        &opts,
    )
}

fn equal(r: &Report) -> bool {
    matches!(r.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions)
}

fn assumes(r: &Report, id: &str) -> bool {
    r.assumptions.iter().any(|a| a.id == id)
}

const ORDERS_TU: &str = "CREATE TRIGGER dbo.orders_tu ON dbo.orders FOR UPDATE AS
BEGIN
  IF @@ROWCOUNT = 0 RETURN;
  SET NOCOUNT ON;
  UPDATE o SET total = i.qty * i.price
  FROM dbo.orders o JOIN inserted i ON o.order_id = i.order_id
  LEFT JOIN deleted d ON d.order_id = i.order_id
  WHERE i.qty <> d.qty OR i.price <> d.price;
END";

/// The PostgreSQL conversion of `ORDERS_TU`, with `body` as the function's
/// statements and `frame` as the trigger's clauses after the table.
fn orders_tu_pg(body: &str, frame: &str) -> String {
    format!(
        "CREATE OR REPLACE FUNCTION orders_tu() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
{body}
END;
$$;

CREATE OR REPLACE TRIGGER orders_tu
AFTER UPDATE ON orders
{frame}
EXECUTE FUNCTION orders_tu();
"
    )
}

const GUARD: &str = "  IF NOT EXISTS (SELECT 1 FROM inserted) THEN
    RETURN NULL;
  END IF;";
const TU_UPDATE: &str = "  UPDATE orders o SET total = i.qty * i.price FROM inserted i LEFT JOIN deleted d ON d.order_id = i.order_id WHERE (o.order_id = i.order_id) AND (i.qty <> d.qty or i.price <> d.price);";
const FRAME: &str = "REFERENCING NEW TABLE AS inserted OLD TABLE AS deleted\nFOR EACH STATEMENT";

fn tu(guard: &str, update: &str) -> String {
    orders_tu_pg(&format!("{guard}\n{update}\n  RETURN NULL;"), FRAME)
}

#[test]
fn the_proposal_example_is_equal_and_its_reentry_harmless() {
    let r = run(ORDERS_TU, &tu(GUARD, TU_UPDATE));
    assert!(equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
    for id in [
        "ASM-TRIGGER-ERROR",
        "ASM-TRIGGER-ORDER",
        "ASM-TRIGGER-ROWCOUNT",
        "ASM-TRIGGER-REENTRY-OTHERS",
    ] {
        assert!(assumes(&r, id), "{id}: {:?}", r.assumptions);
    }
}

/// `ORDERS_TU` with its guard (or none), `deleted`'s join condition and
/// the WHERE condition replaced.
fn orders_tu(guard: bool, on: &str, cond: &str) -> String {
    let guard = if guard {
        "  IF @@ROWCOUNT = 0 RETURN;\n"
    } else {
        ""
    };
    format!(
        "CREATE TRIGGER dbo.orders_tu ON dbo.orders FOR UPDATE AS
BEGIN
{guard}  UPDATE o SET total = i.qty * i.price
  FROM dbo.orders o JOIN inserted i ON o.order_id = i.order_id
  LEFT JOIN deleted d ON {on}
  WHERE {cond};
END"
    )
}

#[test]
fn reentry_without_a_proof_is_not_equal() {
    let key = "d.order_id = i.order_id";
    let changed = "i.qty <> d.qty OR i.price <> d.price";
    for (why, guard, on, cond) in [
        ("no guard", false, key, changed),
        // No test that the row changed: the second firing updates again.
        ("no change test", true, key, "i.qty > 0"),
        // The change test reads the column the trigger sets.
        ("assigned column", true, key, "i.total <> d.total"),
        // The images are not matched on a key.
        ("no key match", true, "d.qty = i.qty", changed),
        // One branch of the OR is no change test.
        ("or", true, key, "i.qty <> d.qty OR i.price > 0"),
    ] {
        let pg = tu(
            if guard { GUARD } else { "" },
            &format!(
                "  UPDATE orders o SET total = i.qty * i.price FROM inserted i LEFT JOIN deleted d ON {on} WHERE (o.order_id = i.order_id) AND ({cond});"
            ),
        );
        let r = run(&orders_tu(guard, on, cond), &pg);
        assert!(!equal(&r), "{why}: {:?}", r.assumptions);
        assert!(
            r.unknown_reasons
                .iter()
                .any(|x| x.contains("updates its own table")),
            "{why}: {:#?}",
            r.unknown_reasons
        );
    }
}

#[test]
fn trigger_frames_that_differ_are_not_equal() {
    let ok = tu(GUARD, TU_UPDATE);
    for (why, pg) in [
        (
            "row trigger",
            ok.replace("FOR EACH STATEMENT", "FOR EACH ROW"),
        ),
        (
            "other event",
            ok.replace("AFTER UPDATE ON", "AFTER INSERT ON"),
        ),
        (
            "other table",
            ok.replace("AFTER UPDATE ON orders", "AFTER UPDATE ON order_items"),
        ),
        ("before", ok.replace("AFTER UPDATE", "BEFORE UPDATE")),
        (
            "images swapped",
            ok.replace(
                "NEW TABLE AS inserted OLD TABLE AS deleted",
                "NEW TABLE AS deleted OLD TABLE AS inserted",
            ),
        ),
        ("an image missing", ok.replace(" OLD TABLE AS deleted", "")),
        (
            "no final RETURN",
            ok.replace("\n  RETURN NULL;\nEND;", "\nEND;"),
        ),
        ("guard on one side", tu("", TU_UPDATE)),
        (
            "a statement more",
            tu(GUARD, &format!("{TU_UPDATE}\n  DELETE FROM audit_log;")),
        ),
        (
            "another function",
            format!("{ok}\nCREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;"),
        ),
        (
            "security definer",
            ok.replace("LANGUAGE plpgsql\n", "LANGUAGE plpgsql SECURITY DEFINER\n"),
        ),
    ] {
        let r = run(ORDERS_TU, &pg);
        assert!(!equal(&r), "{why}: {:?}\n{pg}", r.assumptions);
    }
}

const AUDIT: &str = "CREATE TRIGGER dbo.orders_audit ON dbo.orders AFTER INSERT, DELETE AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO audit_log (order_id, action, qty)
  SELECT i.order_id, 'ins', i.qty FROM inserted i
  WHERE NOT EXISTS (SELECT 1 FROM deleted d WHERE d.order_id = i.order_id);
  INSERT INTO audit_log (order_id, action, qty) SELECT order_id, 'del', qty FROM deleted;
END";

fn audit_pg(ins_deleted: &str, del_inserted: &str) -> String {
    format!(
        "CREATE OR REPLACE FUNCTION orders_audit_insert() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO audit_log (order_id, action, qty)
  SELECT i.order_id, 'ins', i.qty FROM inserted i
  WHERE NOT EXISTS (SELECT 1 FROM {ins_deleted} d WHERE d.order_id = i.order_id);
  INSERT INTO audit_log (order_id, action, qty) SELECT order_id, 'del', qty FROM {ins_deleted} AS deleted;
  RETURN NULL;
END;
$$;

CREATE OR REPLACE TRIGGER orders_audit_insert
AFTER INSERT ON orders
REFERENCING NEW TABLE AS inserted
FOR EACH STATEMENT
EXECUTE FUNCTION orders_audit_insert();

CREATE OR REPLACE FUNCTION orders_audit_delete() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO audit_log (order_id, action, qty)
  SELECT i.order_id, 'ins', i.qty FROM {del_inserted} i
  WHERE NOT EXISTS (SELECT 1 FROM deleted d WHERE d.order_id = i.order_id);
  INSERT INTO audit_log (order_id, action, qty) SELECT order_id, 'del', qty FROM deleted;
  RETURN NULL;
END;
$$;

CREATE OR REPLACE TRIGGER orders_audit_delete
AFTER DELETE ON orders
REFERENCING OLD TABLE AS deleted
FOR EACH STATEMENT
EXECUTE FUNCTION orders_audit_delete();
"
    )
}

const EMPTY: &str = "(SELECT * FROM orders WHERE false)";

#[test]
fn each_event_is_compared_with_its_own_function() {
    let r = run(AUDIT, &audit_pg(EMPTY, EMPTY));
    assert!(equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
    assert!(!assumes(&r, "ASM-TRIGGER-ROWCOUNT"));
    // The missing image must be empty, of the trigger's table.
    for (why, x, y) in [
        (
            "a non-empty image",
            "(SELECT * FROM orders WHERE true)",
            EMPTY,
        ),
        ("the table itself", "orders", EMPTY),
        (
            "another table",
            "(SELECT * FROM audit_log WHERE false)",
            EMPTY,
        ),
        ("the other side", EMPTY, "(SELECT * FROM orders)"),
    ] {
        let r = run(AUDIT, &audit_pg(x, y));
        assert!(!equal(&r), "{why}: {:?}", r.assumptions);
    }
    // A trigger for one event only is not the conversion of both.
    let pg = audit_pg(EMPTY, EMPTY);
    let insert_only = &pg[..pg
        .find("CREATE OR REPLACE FUNCTION orders_audit_delete")
        .unwrap()];
    let r = run(AUDIT, insert_only);
    assert!(!equal(&r));
    // An image is not its table: reading `orders` is not reading `inserted`.
    let r = run(
        AUDIT,
        &audit_pg(EMPTY, EMPTY).replacen("FROM inserted i", "FROM orders i", 1),
    );
    assert!(!equal(&r), "{:?}", r.assumptions);
    // A body that reads the wrong image.
    let r = run(
        AUDIT,
        &audit_pg(EMPTY, EMPTY).replacen(
            "FROM inserted i",
            "FROM (SELECT * FROM orders WHERE false) i",
            1,
        ),
    );
    assert!(!equal(&r), "{:?}", r.assumptions);
}

/// The trigger's writes are paired per function: each event's function
/// repeats them.
#[test]
fn writes_are_paired_per_function() {
    let r = run(AUDIT, &audit_pg(EMPTY, EMPTY));
    assert!(
        !r.findings.iter().any(|f| f.rule == "write.unpaired"),
        "{:#?}",
        r.findings
    );
}

/// `@@ROWCOUNT` and an existence test on the rows' image are the same
/// guard; anything else first is an ordinary IF, not compared.
#[test]
fn guards() {
    for (why, tsql_guard, ok) in [
        ("rowcount", "IF @@ROWCOUNT = 0 RETURN;", true),
        (
            "rowcount < 1",
            "IF (@@ROWCOUNT < 1) BEGIN RETURN; END",
            true,
        ),
        (
            "exists",
            "IF NOT EXISTS (SELECT 1 FROM inserted) RETURN;",
            true,
        ),
        (
            "other image",
            "IF NOT EXISTS (SELECT 1 FROM deleted) RETURN;",
            false,
        ),
        ("rowcount > 0", "IF @@ROWCOUNT > 0 RETURN;", false),
    ] {
        let tsql = ORDERS_TU.replace("IF @@ROWCOUNT = 0 RETURN;", tsql_guard);
        let r = run(&tsql, &tu(GUARD, TU_UPDATE));
        assert_eq!(equal(&r), ok, "{why}: {:#?}", r.unknown_reasons);
    }
    // After SET NOCOUNT ON, @@ROWCOUNT is 0: not a guard.
    let tsql = ORDERS_TU.replace(
        "  IF @@ROWCOUNT = 0 RETURN;\n  SET NOCOUNT ON;",
        "  SET NOCOUNT ON;\n  IF @@ROWCOUNT = 0 RETURN;",
    );
    assert!(!equal(&run(&tsql, &tu(GUARD, TU_UPDATE))));
}

/// A product of exact numbers within 38 digits is exact on both sides.
#[test]
fn exact_products_compare_equal() {
    let t = |e: &str| format!("CREATE PROCEDURE p AS UPDATE orders SET total = {e};");
    let p = |e: &str| {
        format!("CREATE PROCEDURE p() LANGUAGE sql AS $$ UPDATE orders SET total = {e}; $$;")
    };
    let r = run(&t("qty * price"), &p("qty * price"));
    assert!(equal(&r), "{:#?}", r.findings);
    let r = run(&t("price * qty"), &p("qty * price"));
    assert!(equal(&r), "{:#?}", r.findings);
    // A float operand, or a quotient, is not.
    let r = run(&t("qty * price / 3"), &p("qty * price / 3"));
    assert!(!equal(&r));
    let r = run(
        &t("qty * CAST(price AS float)"),
        &p("qty * CAST(price AS float)"),
    );
    assert!(!equal(&r));
}

/// From the review of the re-entry proof: a change test on a computed column
/// changes with the column the trigger assigns, so PostgreSQL re-fires the
/// trigger until the stack depth limit.
#[test]
fn a_change_test_on_a_computed_column_is_no_proof() {
    let ddl_a = "CREATE TABLE dbo.orders (order_id int NOT NULL PRIMARY KEY, qty int NOT NULL, price decimal(10,2) NOT NULL, total AS (qty * price) PERSISTED);";
    let ddl_b = "CREATE TABLE orders (order_id integer NOT NULL PRIMARY KEY, qty integer NOT NULL, price numeric(10,2) NOT NULL, total numeric(21,2) GENERATED ALWAYS AS (qty * price) STORED);";
    let tsql = "CREATE TRIGGER dbo.tr ON dbo.orders FOR UPDATE AS BEGIN IF @@ROWCOUNT = 0 RETURN;
  UPDATE o SET qty = i.qty + 1 FROM dbo.orders o JOIN inserted i ON o.order_id = i.order_id
  JOIN deleted d ON d.order_id = i.order_id WHERE i.total <> d.total; END";
    let pg = |cond: &str| {
        format!(
            "CREATE OR REPLACE FUNCTION tr() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
{GUARD}
  UPDATE orders o SET qty = i.qty + 1 FROM inserted i JOIN deleted d ON d.order_id = i.order_id WHERE o.order_id = i.order_id AND {cond};
  RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER tr AFTER UPDATE ON orders REFERENCING NEW TABLE AS inserted OLD TABLE AS deleted FOR EACH STATEMENT EXECUTE FUNCTION tr();"
        )
    };
    let r = run_with(tsql, &pg("i.total <> d.total"), ddl_a, ddl_b);
    assert!(!equal(&r), "{:?}", r.assumptions);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("updates its own table")),
        "{:#?}",
        r.unknown_reasons
    );
    // The same test on a stored column the trigger does not assign is one.
    let r = run_with(
        &tsql.replace("i.total <> d.total", "i.price <> d.price"),
        &pg("i.price <> d.price"),
        ddl_a,
        ddl_b,
    );
    assert!(equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
}

/// From the review of the re-entry proof: a routine the trigger calls may
/// update the trigger's table, which the proof does not see.
#[test]
fn a_call_in_the_trigger_is_no_proof() {
    let ddl_a = format!("{DDL}CREATE PROCEDURE dbo.touch AS UPDATE dbo.orders SET total = 0;");
    let ddl_b = format!(
        "{DDL}CREATE PROCEDURE touch() LANGUAGE sql AS $$ UPDATE orders SET total = 0; $$;"
    );
    let tsql = "CREATE TRIGGER dbo.tr ON dbo.orders AFTER INSERT, UPDATE AS EXEC dbo.touch;";
    let f = |ev: &str| {
        format!(
            "CREATE OR REPLACE FUNCTION tr_{ev}() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  CALL touch();
  RETURN NULL;
END;
$$;
"
        )
    };
    let pg = format!(
        "{}{}CREATE OR REPLACE TRIGGER tr_insert AFTER INSERT ON orders REFERENCING NEW TABLE AS inserted FOR EACH STATEMENT EXECUTE FUNCTION tr_insert();
CREATE OR REPLACE TRIGGER tr_update AFTER UPDATE ON orders REFERENCING NEW TABLE AS inserted OLD TABLE AS deleted FOR EACH STATEMENT EXECUTE FUNCTION tr_update();",
        f("insert"),
        f("update")
    );
    let r = run_with(tsql, &pg, &ddl_a, &ddl_b);
    assert!(!equal(&r), "{:?}", r.assumptions);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("calls a routine")),
        "{:#?}",
        r.unknown_reasons
    );
}
