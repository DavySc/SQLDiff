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
        "before it on one side",
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
