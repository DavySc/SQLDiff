//! UNION, EXCEPT and INTERSECT compared across T-SQL and PostgreSQL: the
//! branches, the operator, duplicate removal under each dialect's
//! collation, and an ORDER BY of the whole. Each rule has an equal pair and
//! near misses.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const CATALOG_T: &str =
    "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL, w nvarchar(20) NULL);
CREATE TABLE dbo.t2 (id int NOT NULL PRIMARY KEY, v int NULL, w nvarchar(20) NULL);
CREATE TABLE dbo.r (id int NOT NULL, v int NULL);";
const CATALOG_P: &str =
    "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL, w varchar(20) NULL);
CREATE TABLE t2 (id integer NOT NULL PRIMARY KEY, v integer NULL, w varchar(20) NULL);
CREATE TABLE r (id integer NOT NULL, v integer NULL);";

/// A T-SQL procedure with `body`, against a PostgreSQL one with `body_p`;
/// both declare an int `@n`.
fn run(body: &str, body_p: &str) -> Report {
    let a = format!(
        "CREATE PROCEDURE dbo.p @id int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  DECLARE @n int;\n{body}\nEND\n"
    );
    let b = format!(
        "CREATE OR REPLACE PROCEDURE p(p_id integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_n integer;\nBEGIN\n{body_p}\nEND;\n$$;\n"
    );
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", Dialect::Tsql, CATALOG_T)],
        catalog_b: vec![Artifact::new("cb.sql", Dialect::Postgres, CATALOG_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, &a),
        &Artifact::new("b.sql", Dialect::Postgres, &b),
        &opts,
    )
}

fn equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert!(
        matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{body}\n{body_p}\n{:?}\n{:#?}\n{:#?}",
        r.verdict(),
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(body: &str, body_p: &str) {
    let r = run(body, body_p);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{body}\n{body_p}"
    );
}

#[test]
fn union_all_into_a_table() {
    let t = "  INSERT INTO dbo.r (id, v) SELECT id, v FROM dbo.t WHERE v > 1 UNION ALL SELECT id, v FROM dbo.t2;";
    equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, v FROM t WHERE v > 1 UNION ALL SELECT id, v FROM t2;",
    );
    // Another operator, branch order, or branch.
    not_equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, v FROM t WHERE v > 1 UNION SELECT id, v FROM t2;",
    );
    not_equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, v FROM t2 UNION ALL SELECT id, v FROM t WHERE v > 1;",
    );
    not_equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, v FROM t WHERE v > 2 UNION ALL SELECT id, v FROM t2;",
    );
    not_equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, v FROM t WHERE v > 1;",
    );
}

#[test]
fn distinct_set_operations() {
    for op in ["UNION", "EXCEPT", "INTERSECT"] {
        equal(
            &format!("  SET @n = (SELECT COUNT(*) FROM (SELECT v FROM dbo.t {op} SELECT v FROM dbo.t2) AS x);"),
            &format!("  v_n := (SELECT COUNT(*) FROM (SELECT v FROM t {op} SELECT v FROM t2) AS x);"),
        );
    }
    not_equal(
        "  SET @n = (SELECT COUNT(*) FROM (SELECT v FROM dbo.t EXCEPT SELECT v FROM dbo.t2) AS x);",
        "  v_n := (SELECT COUNT(*) FROM (SELECT v FROM t2 EXCEPT SELECT v FROM t) AS x);",
    );
    not_equal(
        "  SET @n = (SELECT COUNT(*) FROM (SELECT v FROM dbo.t UNION SELECT v FROM dbo.t2) AS x);",
        "  v_n := (SELECT COUNT(*) FROM (SELECT v FROM t UNION ALL SELECT v FROM t2) AS x);",
    );
    // Strings are made unique under each dialect's collation.
    not_equal(
        "  SET @n = (SELECT COUNT(*) FROM (SELECT w FROM dbo.t UNION SELECT w FROM dbo.t2) AS x);",
        "  v_n := (SELECT COUNT(*) FROM (SELECT w FROM t UNION SELECT w FROM t2) AS x);",
    );
    // Columns of other types are converted by each dialect's own rules.
    not_equal(
        "  SET @n = (SELECT COUNT(*) FROM (SELECT v FROM dbo.t UNION SELECT w FROM dbo.t2) AS x);",
        "  v_n := (SELECT COUNT(*) FROM (SELECT v FROM t UNION SELECT w::integer FROM t2) AS x);",
    );
    not_equal(
        "  SET @n = (SELECT COUNT(*) FROM (SELECT v FROM dbo.t UNION SELECT NULL) AS x);",
        "  v_n := (SELECT COUNT(*) FROM (SELECT v FROM t UNION SELECT NULL) AS x);",
    );
}

#[test]
fn an_ordered_set_operation() {
    let t = "  INSERT INTO dbo.r (id, v) SELECT id, v FROM dbo.t UNION ALL SELECT id, v FROM dbo.t2 ORDER BY id;";
    let p = |order: &str| {
        format!("  INSERT INTO r (id, v) SELECT id, v FROM t UNION ALL SELECT id, v FROM t2 ORDER BY {order};")
    };
    equal(t, &p("id"));
    equal(t, &p("1"));
    not_equal(t, &p("id DESC"));
    not_equal(t, &p("v"));
    // v may be NULL: T-SQL sorts NULL first, PostgreSQL last.
    let tv = "  INSERT INTO dbo.r (id, v) SELECT id, v FROM dbo.t UNION ALL SELECT id, v FROM dbo.t2 ORDER BY v;";
    not_equal(tv, &p("v"));
    equal(tv, &p("v NULLS FIRST"));
    // ORDER BY of the whole, by name or position; ids are NOT NULL.
    equal(
        "  SET @n = (SELECT TOP (1) id FROM dbo.t WHERE id IN (SELECT id FROM dbo.t UNION ALL SELECT id FROM dbo.t2) ORDER BY id);",
        "  v_n := (SELECT id FROM t WHERE id IN (SELECT id FROM t UNION ALL SELECT id FROM t2) ORDER BY id LIMIT 1);",
    );
    // A TOP or ORDER BY inside a branch, and paging of the whole, are not compared.
    not_equal(
        "  SET @n = (SELECT COUNT(*) FROM (SELECT TOP (1) v FROM dbo.t UNION ALL SELECT v FROM dbo.t2) AS x);",
        "  v_n := (SELECT COUNT(*) FROM ((SELECT v FROM t LIMIT 1) UNION ALL SELECT v FROM t2) AS x);",
    );
}

#[test]
fn an_aggregate_over_all_rows() {
    let t = "  INSERT INTO dbo.r (id, v) SELECT id, MAX(v) OVER () FROM dbo.t WHERE v > 1;";
    equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, MAX(v) OVER () FROM t WHERE v > 1;",
    );
    not_equal(
        t,
        "  INSERT INTO r (id, v) SELECT id, MIN(v) OVER () FROM t WHERE v > 1;",
    );
    // OVER keeps every row, where the aggregate alone gives one.
    not_equal(
        "  SET @n = (SELECT MAX(v) OVER () FROM dbo.t);",
        "  v_n := (SELECT MAX(v) FROM t);",
    );
    // Partitions and window ordering of aggregates are not read.
    not_equal(
        "  INSERT INTO dbo.r (id, v) SELECT id, MAX(v) OVER (PARTITION BY w) FROM dbo.t;",
        "  INSERT INTO r (id, v) SELECT id, MAX(v) OVER (PARTITION BY w) FROM t;",
    );
    // ROW_NUMBER() OVER (ORDER BY ...) numbers by its keys (ASM-ORDER-TOTAL
    // where they may tie); another direction numbers otherwise.
    equal(
        "  INSERT INTO dbo.r (id, v) SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM dbo.t;",
        "  INSERT INTO r (id, v) SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM t;",
    );
    not_equal(
        "  INSERT INTO dbo.r (id, v) SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM dbo.t;",
        "  INSERT INTO r (id, v) SELECT id, ROW_NUMBER() OVER (ORDER BY id DESC) FROM t;",
    );
    not_equal(
        "  INSERT INTO dbo.r (id, v) SELECT id, ROW_NUMBER() OVER (PARTITION BY w ORDER BY id) FROM dbo.t;",
        "  INSERT INTO r (id, v) SELECT id, ROW_NUMBER() OVER (PARTITION BY w ORDER BY id) FROM t;",
    );
}
