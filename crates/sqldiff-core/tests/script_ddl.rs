//! Script-level DDL compared across dialects (T-SQL against PostgreSQL):
//! DROP of a table, view, routine, sequence or schema, a column default set
//! after the table, and CREATE TABLE statements paired by name when one side
//! creates fewer tables.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn differs(a: &str, b: &str, rule: &str) {
    let r = run(a, b);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.unknown_reasons);
    assert!(rules(&r).contains(&rule), "{rule}: {:?}", rules(&r));
}

fn unknown(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert_ne!(r.verdict(), Verdict::Equal);
}

#[test]
fn drops_are_compared() {
    for (t, p) in [
        ("DROP TABLE dbo.Orders;", "DROP TABLE Orders;"),
        (
            "DROP TABLE IF EXISTS dbo.Orders;",
            "DROP TABLE IF EXISTS Orders;",
        ),
        ("DROP VIEW Sales.V;", "DROP VIEW Sales.V;"),
        ("DROP PROC dbo.p;", "DROP PROCEDURE p;"),
        ("DROP FUNCTION dbo.f;", "DROP FUNCTION f;"),
        ("DROP SEQUENCE dbo.s;", "DROP SEQUENCE s RESTRICT;"),
    ] {
        let r = equal(t, p);
        assert!(asms(&r).contains(&"ASM-DROP-NO-DEPENDENTS"), "{t}");
    }
    // A schema is only dropped when empty, on both.
    let r = equal("DROP SCHEMA Sales;", "DROP SCHEMA Sales;");
    assert!(!asms(&r).contains(&"ASM-DROP-NO-DEPENDENTS"));
}

#[test]
fn drops_that_differ_or_are_not_read() {
    differs("DROP TABLE dbo.A;", "DROP TABLE B;", "equiv.drop-object");
    unknown("DROP TABLE dbo.A;", "DROP VIEW A;", "no counterpart");
    differs(
        "DROP TABLE IF EXISTS dbo.A;",
        "DROP TABLE A;",
        "equiv.drop-if-exists",
    );
    // T-SQL drops the tables one by one and stops at the first failure.
    unknown(
        "DROP TABLE dbo.A, dbo.B;",
        "DROP TABLE A, B;",
        "several objects",
    );
    // CASCADE drops what depends on the object too.
    unknown("DROP TABLE dbo.A;", "DROP TABLE A CASCADE;", "CASCADE");
    unknown(
        "DROP FUNCTION dbo.f;",
        "DROP FUNCTION f(int);",
        "argument list",
    );
}

#[test]
fn a_default_added_after_the_table_is_compared() {
    let t = "CREATE TABLE dbo.T (id int NOT NULL PRIMARY KEY, n int NOT NULL);\nGO\nALTER TABLE dbo.T ADD CONSTRAINT DF_T_n DEFAULT (0) FOR n;\n";
    let p = "CREATE TABLE T (id integer NOT NULL PRIMARY KEY, n integer NOT NULL);\nALTER TABLE T ALTER COLUMN n SET DEFAULT 0;\n";
    let r = equal(t, p);
    assert!(asms(&r).contains(&"ASM-DEFAULT-CONSTRAINT-NAME"));
    unknown(
        t,
        &p.replace("SET DEFAULT 0", "SET DEFAULT 1"),
        "A has `0` where B has `1`",
    );
    differs(
        t,
        &p.replace("ALTER COLUMN n", "ALTER COLUMN id"),
        "equiv.column-default",
    );
    // The table must come before the default on both sides.
    unknown(
        t,
        "ALTER TABLE T ALTER COLUMN n SET DEFAULT 0;\nCREATE TABLE T (id integer NOT NULL PRIMARY KEY, n integer NOT NULL);\n",
        "another order",
    );
}

#[test]
fn tables_pair_by_name_when_one_side_creates_fewer() {
    // The conversion left B out (a placeholder stands in its place): A and
    // C still meet their own counterparts.
    let t = "CREATE TABLE dbo.A (id int NOT NULL PRIMARY KEY);\nGO\nCREATE TABLE dbo.B (id int NOT NULL PRIMARY KEY);\nGO\nCREATE TABLE dbo.C (id int NOT NULL PRIMARY KEY);\n";
    let p = "CREATE TABLE A (id integer NOT NULL PRIMARY KEY);\nCREATE TABLE C (id integer NOT NULL PRIMARY KEY);\n";
    let r = run(t, p);
    assert_eq!(
        r.unknown_reasons,
        vec!["side A (T-SQL) line 3: a CREATE TABLE is not compared: the other side creates no table of that name".to_string()]
    );
    assert!(r.findings.is_empty(), "{:?}", rules(&r));
    // A real difference in a paired table is still found.
    let r = run(
        t,
        &p.replace(
            "C (id integer NOT NULL PRIMARY KEY)",
            "C (id integer NOT NULL)",
        ),
    );
    assert!(rules(&r).contains(&"equiv.table-key"), "{:?}", rules(&r));
    // Tables in another order are not paired by name.
    let r = run(
        t,
        "CREATE TABLE C (id integer NOT NULL PRIMARY KEY);\nCREATE TABLE A (id integer NOT NULL PRIMARY KEY);\n",
    );
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("the sides create 3 and 2 tables")),
        "{:?}",
        r.unknown_reasons
    );
}

const TWO: &str = "CREATE TABLE dbo.T (id int NOT NULL PRIMARY KEY, n int NOT NULL);
GO
CREATE PROCEDURE dbo.p1 AS
BEGIN
    UPDATE dbo.T SET n = 1;
END;
GO
DELETE FROM dbo.T;
GO
CREATE PROCEDURE dbo.p2 AS
BEGIN
    UPDATE dbo.T SET n = 2;
END;
GO
";

fn two_pg(n1: &str, n2: &str, v2: &str) -> String {
    format!(
        "CREATE TABLE T (id integer NOT NULL PRIMARY KEY, n integer NOT NULL);
CREATE OR REPLACE PROCEDURE {n1}()
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE T SET n = 1;
END;
$$;
DELETE FROM T;
CREATE OR REPLACE PROCEDURE {n2}()
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE T SET n = {v2};
END;
$$;
"
    )
}

#[test]
fn several_routines_meet_their_conversions_by_name() {
    let r = equal(TWO, &two_pg("p1", "p2", "2"));
    assert!(asms(&r).contains(&"ASM-DEFAULT-SCHEMA"), "{:?}", asms(&r));
    // The second routine is compared too.
    let r = run(TWO, &two_pg("p1", "p2", "3"));
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.unknown_reasons);
    // Routines in another order (or under other names) are not paired: the
    // scripts are compared whole and every routine's statements are named.
    let r = run(TWO, &two_pg("p2", "p1", "2"));
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    for line in [3, 5, 10, 12] {
        assert!(
            r.unknown_reasons
                .iter()
                .any(|x| x.starts_with(&format!("side A (T-SQL) line {line}:"))),
            "{line}: {:?}",
            r.unknown_reasons
        );
    }
}

#[test]
fn a_drop_stays_in_place_around_its_objects_create() {
    // Review probes: one side ends with the object, the other without it.
    unknown(
        "DROP SEQUENCE dbo.s;\nGO\nCREATE SEQUENCE dbo.s AS int START WITH 1;\n",
        "CREATE SEQUENCE s AS integer START WITH 1;\nDROP SEQUENCE s;\n",
        "another order",
    );
    unknown(
        "DROP SCHEMA x;\nGO\nCREATE SCHEMA x;\n",
        "CREATE SCHEMA x;\nDROP SCHEMA x;\n",
        "another order",
    );
    // Something created in the schema counts for the schema's drop.
    unknown(
        "CREATE SCHEMA x;\nGO\nDROP SCHEMA x;\nGO\nCREATE TABLE x.t (id int NOT NULL PRIMARY KEY);\n",
        "CREATE SCHEMA x;\nCREATE TABLE x.t (id integer NOT NULL PRIMARY KEY);\nDROP SCHEMA x;\n",
        "another order",
    );
    // The same order on both sides is compared.
    equal(
        "DROP SEQUENCE dbo.s;\nGO\nCREATE SEQUENCE dbo.s AS int START WITH 1;\n",
        "DROP SEQUENCE s;\nCREATE SEQUENCE s AS integer START WITH 1;\n",
    );
}

#[test]
fn a_second_default_is_not_compared() {
    // Review probe: SQL Server refuses a second default (Msg 1781),
    // PostgreSQL replaces the one there.
    unknown(
        "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL DEFAULT 1);\nGO\nALTER TABLE dbo.t ADD DEFAULT 2 FOR v;\n",
        "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL DEFAULT 1);\nALTER TABLE t ALTER COLUMN v SET DEFAULT 2;\n",
        "already gives the column a default",
    );
    // Two defaults for one column are not compared either.
    unknown(
        "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);\nGO\nALTER TABLE dbo.t ADD DEFAULT 1 FOR v;\nGO\nALTER TABLE dbo.t ADD DEFAULT 2 FOR v;\n",
        "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL);\nALTER TABLE t ALTER COLUMN v SET DEFAULT 1;\nALTER TABLE t ALTER COLUMN v SET DEFAULT 2;\n",
        "not compared",
    );
    // An identity column takes no default.
    unknown(
        "CREATE TABLE dbo.t (id int IDENTITY(1,1) NOT NULL PRIMARY KEY);\nGO\nALTER TABLE dbo.t ADD DEFAULT 2 FOR id;\n",
        "CREATE TABLE t (id integer GENERATED BY DEFAULT AS IDENTITY NOT NULL PRIMARY KEY);\nALTER TABLE t ALTER COLUMN id SET DEFAULT 2;\n",
        "not compared",
    );
    // Without the table in the file, the assumption says so.
    let r = equal(
        "ALTER TABLE dbo.t ADD DEFAULT 2 FOR v;\n",
        "ALTER TABLE t ALTER COLUMN v SET DEFAULT 2;\n",
    );
    let a = r
        .assumptions
        .iter()
        .find(|a| a.id == "ASM-DEFAULT-CONSTRAINT-NAME")
        .expect("assumption");
    assert!(a.text.contains("no default yet"), "{}", a.text);
}

const PROC_T: &str = "CREATE PROCEDURE dbo.reset AS
BEGIN
    SET XACT_ABORT ON;
    BEGIN TRANSACTION;
    DELETE FROM dbo.Log WHERE id < 0;
    TRUNCATE TABLE dbo.Orders;
    INSERT INTO dbo.Log (id) VALUES (1);
    COMMIT;
END;
";

fn proc_p(truncate: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE reset()
LANGUAGE plpgsql
AS $$
BEGIN
  DELETE FROM Log WHERE id < 0;
  {truncate}
  INSERT INTO Log (id) VALUES (1);
  COMMIT;
END;
$$;
"
    )
}

#[test]
fn truncate_is_compared_with_the_identity_restarted() {
    let r = equal(PROC_T, &proc_p("TRUNCATE TABLE Orders RESTART IDENTITY;"));
    assert!(asms(&r).contains(&"ASM-TRUNCATE"), "{:?}", asms(&r));
    // PostgreSQL keeps the identity counter unless asked.
    unknown(
        PROC_T,
        &proc_p("TRUNCATE TABLE Orders;"),
        "RESTART IDENTITY",
    );
    unknown(
        PROC_T,
        &proc_p("TRUNCATE TABLE Orders CONTINUE IDENTITY;"),
        "RESTART IDENTITY",
    );
    differs(
        PROC_T,
        &proc_p("TRUNCATE TABLE Log RESTART IDENTITY;"),
        "equiv.truncate-table",
    );
    // CASCADE empties the tables that refer to it too; several tables.
    unknown(
        PROC_T,
        &proc_p("TRUNCATE TABLE Orders RESTART IDENTITY CASCADE;"),
        "TRUNCATE",
    );
    unknown(
        PROC_T,
        &proc_p("TRUNCATE TABLE Orders, Log RESTART IDENTITY;"),
        "TRUNCATE",
    );
    // In its place among the data statements.
    let moved = "CREATE OR REPLACE PROCEDURE reset()
LANGUAGE plpgsql
AS $$
BEGIN
  TRUNCATE TABLE Orders RESTART IDENTITY;
  DELETE FROM Log WHERE id < 0;
  INSERT INTO Log (id) VALUES (1);
  COMMIT;
END;
$$;
";
    unknown(PROC_T, moved, "data statement(s)");
    // Inside a branch, or with a row count read, it is not compared.
    let t_if = PROC_T.replace(
        "    TRUNCATE TABLE dbo.Orders;",
        "    IF 1 = 1 TRUNCATE TABLE dbo.Orders;",
    );
    let p_if = proc_p("IF 1 = 1 THEN TRUNCATE TABLE Orders RESTART IDENTITY; END IF;");
    unknown(&t_if, &p_if, "inside an IF");
    let t_rc = PROC_T.replace(
        "    INSERT INTO dbo.Log (id) VALUES (1);",
        "    INSERT INTO dbo.Log (id) VALUES (@@ROWCOUNT);",
    );
    let p_rc = proc_p("TRUNCATE TABLE Orders RESTART IDENTITY;").replace(
        "  INSERT INTO Log (id) VALUES (1);",
        "  GET DIAGNOSTICS n = ROW_COUNT;\n  INSERT INTO Log (id) VALUES (n);",
    );
    unknown(&t_rc, &p_rc, "row count");
}

#[test]
fn a_truncate_kept_by_t_sql_after_a_failure_is_found() {
    // Without XACT_ABORT ON, T-SQL keeps the TRUNCATE when the INSERT after
    // it fails; the PostgreSQL procedure undoes it.
    let t = PROC_T
        .replace("    SET XACT_ABORT ON;\n", "")
        .replace("    BEGIN TRANSACTION;\n", "")
        .replace("    COMMIT;\n", "")
        .replace("    DELETE FROM dbo.Log WHERE id < 0;\n", "");
    let p = proc_p("TRUNCATE TABLE Orders RESTART IDENTITY;")
        .replace("  COMMIT;\n", "")
        .replace("  DELETE FROM Log WHERE id < 0;\n", "");
    differs(&t, &p, "txn.atomicity");
}
