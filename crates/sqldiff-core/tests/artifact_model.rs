//! An artifact with several routines and statements between them
//! (T-SQL against PostgreSQL): the routines are paired by kind and name,
//! each T-SQL AFTER trigger with the statement triggers and functions written
//! for it, and the statements between them are compared as script segments.
//! Any shape counts, not only a particular sample's.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

/// The table the routines work on, as the catalogs of both sides know it.
fn run(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new(
            "catalog.sql",
            Dialect::Tsql,
            "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, n int NULL);",
        )],
        catalog_b: vec![Artifact::new(
            "catalog.sql",
            Dialect::Postgres,
            "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, n integer NULL);",
        )],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    )
}

const TSQL: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, n int NULL);
GO
CREATE PROCEDURE dbo.p1 @x int AS
BEGIN
  SET NOCOUNT ON;
  UPDATE dbo.t SET n = @x WHERE id = 1;
END;
GO
CREATE VIEW dbo.v1 AS SELECT id FROM dbo.t;
GO
CREATE TRIGGER dbo.tr1 ON dbo.t AFTER INSERT AS
BEGIN
  UPDATE dbo.t SET n = 0 WHERE id IN (SELECT id FROM inserted);
END;
GO
CREATE FUNCTION dbo.f1(@a int) RETURNS int AS
BEGIN
  RETURN @a + 1;
END;
GO
";

fn pg(proc_set: &str, trig_set: &str, func_ret: &str) -> String {
    format!(
        "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, n integer NULL);
CREATE OR REPLACE PROCEDURE p1(p_x integer)
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE t SET n = {proc_set} WHERE id = 1;
END;
$$;
CREATE OR REPLACE VIEW v1 AS
SELECT id FROM t;
CREATE OR REPLACE FUNCTION tr1() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE t SET n = {trig_set} WHERE id IN (SELECT id FROM inserted);
  RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER tr1
AFTER INSERT ON t
REFERENCING NEW TABLE AS inserted
FOR EACH STATEMENT
EXECUTE FUNCTION tr1();
CREATE OR REPLACE FUNCTION f1(p_a integer) RETURNS integer
LANGUAGE plpgsql
AS $$
BEGIN
  RETURN {func_ret};
END;
$$;
"
    )
}

fn not_covered(r: &Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| x.contains("several routines") || x.contains("outside the routines"))
        .collect()
}

#[test]
fn routines_trigger_and_view_in_one_file_are_each_compared() {
    let r = run(TSQL, &pg("p_x", "0", "p_a + 1"));
    assert_eq!(not_covered(&r), Vec::<&String>::new());
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
}

#[test]
fn a_difference_in_any_routine_of_the_file_is_found() {
    // Procedure, trigger function and function each differ in turn: the
    // pairing must not let one routine's text stand in for another's.
    for (p, t, f) in [
        ("p_x + 1", "0", "p_a + 1"),
        ("p_x", "1", "p_a + 1"),
        ("p_x", "0", "p_a + 2"),
    ] {
        let r = run(TSQL, &pg(p, t, f));
        assert_eq!(not_covered(&r), Vec::<&String>::new(), "{p} {t} {f}");
        assert_ne!(
            r.verdict(),
            Verdict::EqualUnderAssumptions,
            "{p} {t} {f}: {:#?}",
            r.unknown_reasons
        );
        assert_ne!(r.verdict(), Verdict::Equal);
    }
}

#[test]
fn two_triggers_for_one_event_of_a_table_are_not_paired_by_guess() {
    // Both T-SQL triggers fire on INSERT of t; side B has one trigger for
    // that event. Which function belongs to which is not decided: the file
    // is not proven equal.
    let tsql = format!(
        "{TSQL}CREATE TRIGGER dbo.tr2 ON dbo.t AFTER INSERT AS
BEGIN
  UPDATE dbo.t SET n = 5 WHERE id IN (SELECT id FROM inserted);
END;
GO
"
    );
    let r = run(&tsql, &pg("p_x", "0", "p_a + 1"));
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert_ne!(r.verdict(), Verdict::Equal);
}

#[test]
fn a_refused_routine_without_output_does_not_shift_the_others() {
    // A DDL trigger has no PostgreSQL counterpart in this file: it is
    // reported, and the routines after it still meet their own.
    let tsql = format!(
        "CREATE TRIGGER dbo.ddl1 ON DATABASE FOR DDL_DATABASE_LEVEL_EVENTS AS
BEGIN
  SET NOCOUNT ON;
END;
GO
{TSQL}"
    );
    let r = run(&tsql, &pg("p_x", "0", "p_a + 1"));
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains("line 1")),
        "{:?}",
        r.unknown_reasons
    );
}
