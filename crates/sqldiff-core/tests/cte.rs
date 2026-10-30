//! Common table expressions in statement equivalence: T-SQL `WITH` against
//! the PostgreSQL `WITH [RECURSIVE]` the converter writes for it. What each
//! case relies on was run on SQL Server 2022 and PostgreSQL 18
//! (eval/cte/2026-10-30): T-SQL stops a recursion past 100 levels unless
//! `OPTION (MAXRECURSION 0)`, PostgreSQL never; both refuse anchor and
//! recursive member types that differ; a query naming a later CTE reads the
//! table in T-SQL and in PostgreSQL without RECURSIVE, the later CTE with
//! RECURSIVE; a PostgreSQL CTE naming itself without RECURSIVE reads the
//! table; T-SQL computes a CTE for each read (NEWID() differs), PostgreSQL
//! once (random() agrees).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report};

const MS_DDL: &str =
    "CREATE TABLE dbo.D (Id int NOT NULL PRIMARY KEY, Item nvarchar(max) NULL, Selected int NULL);
CREATE TABLE dbo.E (Id int NOT NULL PRIMARY KEY, V int NULL);";
const PG_DDL: &str =
    "CREATE TABLE D (Id integer NOT NULL PRIMARY KEY, Item text NULL, Selected integer NULL);
CREATE TABLE E (Id integer NOT NULL PRIMARY KEY, V integer NULL);";

fn run(a: &str, b: &str) -> Report {
    let _ci = sqldiff_core::equiv::compare::ci_collation_scope(true);
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, PG_DDL)],
        ..Options::default()
    };
    diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            format!(
                "CREATE PROCEDURE dbo.p @List nvarchar(4000), @n int
AS
BEGIN
    SET XACT_ABORT ON;
{a}
END;
"
            ),
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            format!(
                "CREATE OR REPLACE PROCEDURE p(p_list varchar(4000), p_n integer)
LANGUAGE plpgsql
AS $$
BEGIN
{b}
END;
$$;
"
            ),
        ),
        &opts,
    )
}

/// The engine's open points (`equiv.*` findings) and the assumptions.
fn check(a: &str, b: &str) -> (Vec<String>, Vec<String>) {
    let r = run(a, b);
    let open = r
        .findings
        .iter()
        .filter(|f| f.rule.starts_with("equiv."))
        .map(|f| format!("{}: {}", f.rule, f.witness))
        .collect();
    (open, r.assumptions.iter().map(|x| x.id.clone()).collect())
}

fn assert_equal(a: &str, b: &str, asm: &[&str]) {
    let (open, have) = check(a, b);
    assert!(open.is_empty(), "not shown equal:\n{a}\n{b}\n{open:#?}");
    for x in asm {
        assert!(have.iter().any(|h| h == x), "equal without {x}:\n{a}\n{b}");
    }
}

fn assert_apart(a: &str, b: &str) {
    let (open, _) = check(a, b);
    assert!(!open.is_empty(), "shown equal:\n{a}\n{b}");
}

const COUNT_A: &str = "WITH N (i) AS (SELECT 1 AS i UNION ALL SELECT i + 1 FROM N WHERE i < @n)
  INSERT INTO dbo.E (Id, V) SELECT i, i FROM N OPTION (MAXRECURSION 0);";
const COUNT_B: &str =
    "WITH RECURSIVE N (i) AS (SELECT 1 AS i UNION ALL SELECT i + 1 FROM N WHERE i < p_n)
  INSERT INTO E (Id, V) SELECT i, i FROM N;";

#[test]
fn recursive_cte_is_equal_within_the_recursion_limit() {
    assert_equal(COUNT_A, COUNT_B, &["ASM-CTE-MAXRECURSION"]);
    // Without OPTION too, and as a SELECT that assigns.
    assert_equal(
        "WITH N (i) AS (SELECT 1 AS i UNION ALL SELECT i + 1 FROM N WHERE i < @n)
  INSERT INTO dbo.E (Id, V) SELECT i, i FROM N;",
        COUNT_B,
        &["ASM-CTE-MAXRECURSION"],
    );
    // A list split: the rest after each comma.
    assert_equal(
        "WITH L (n, Rest) AS
  (
  SELECT 1 AS n, SUBSTRING(@List, 1, 4000) AS Rest WHERE @List IS NOT NULL
  UNION ALL
  SELECT n + 1, SUBSTRING(Rest, CHARINDEX(',', Rest) + 1, 4000) FROM L WHERE CHARINDEX(',', Rest) > 0
  )
  INSERT INTO dbo.D (Id, Item) SELECT n, Rest FROM L OPTION (MAXRECURSION 0);",
        "WITH RECURSIVE L (n, Rest) AS
  (
  SELECT 1 AS n, SUBSTRING(p_list, 1, 4000) AS Rest WHERE p_list IS NOT NULL
  UNION ALL
  SELECT n + 1, SUBSTRING(Rest, strpos(Rest, ',') + 1, 4000) FROM L WHERE strpos(Rest, ',') > 0
  ) INSERT INTO D (Id, Item) SELECT n, Rest FROM L;",
        &["ASM-CTE-MAXRECURSION", "ASM-TSQL-LEN-BMP"],
    );
}

#[test]
fn ctes_that_read_are_derived_tables() {
    let a = "WITH c AS (SELECT Id, Selected FROM dbo.D WHERE Selected = 1),
  c2 (k, v) AS (SELECT Id, Selected FROM c)
  INSERT INTO dbo.E (Id, V) SELECT k, v FROM c2;";
    let b = "WITH c AS (SELECT Id, Selected FROM D WHERE Selected = 1),
  c2 (k, v) AS (SELECT Id, Selected FROM c)
  INSERT INTO E (Id, V) SELECT k, v FROM c2;";
    assert_equal(a, b, &[]);
    // In a SELECT that assigns.
    assert_equal(
        "DECLARE @x int;
  WITH c AS (SELECT Id FROM dbo.D WHERE Selected = 1) SELECT @x = MAX(Id) FROM c;
  INSERT INTO dbo.E (Id, V) VALUES (1, @x);",
        "DECLARE v_x integer;
BEGIN
  WITH c AS (SELECT Id FROM D WHERE Selected = 1) SELECT MAX(Id) INTO v_x FROM c;
  INSERT INTO E (Id, V) VALUES (1, v_x);
END;",
        &[],
    );
}

#[test]
fn near_misses_stay_apart() {
    let a = COUNT_A;
    let rec = |anchor: &str, step: &str, stop: &str, item: &str| {
        format!("WITH RECURSIVE N (i) AS (SELECT {anchor} AS i UNION {step} FROM N WHERE i {stop} p_n)\n INSERT INTO E (Id, V) SELECT i, {item} FROM N;")
    };
    for b in [
        rec("1", "ALL SELECT i + 2", "<", "i"),
        rec("0", "ALL SELECT i + 1", "<", "i"),
        rec("1", "ALL SELECT i + 1", "<=", "i"),
        rec("1", "ALL SELECT i + 1", "<", "i + 1"),
        // UNION removes duplicate rows.
        rec("1", "SELECT i + 1", "<", "i"),
    ] {
        assert_apart(a, &b);
    }
    for (a, b) in [
        // Read twice: PostgreSQL may compute it once, T-SQL per read.
        (
            "WITH c AS (SELECT Id FROM dbo.D) INSERT INTO dbo.E (Id, V) SELECT x.Id, y.Id FROM c x JOIN c y ON x.Id = y.Id;",
            "WITH c AS (SELECT Id FROM D) INSERT INTO E (Id, V) SELECT x.Id, y.Id FROM c x JOIN c y ON x.Id = y.Id;",
        ),
        // A CTE named like the table the other side reads.
        (
            "WITH D AS (SELECT Id, Selected FROM dbo.D WHERE Selected = 1) INSERT INTO dbo.E (Id, V) SELECT Id, Selected FROM D;",
            "INSERT INTO E (Id, V) SELECT Id, Selected FROM D;",
        ),
        // A qualified name is the table.
        (
            "WITH D AS (SELECT Id, Selected FROM dbo.D WHERE Selected = 1) INSERT INTO dbo.E (Id, V) SELECT Id, Selected FROM dbo.D;",
            "WITH c AS (SELECT Id, Selected FROM D WHERE Selected = 1) INSERT INTO E (Id, V) SELECT Id, Selected FROM c;",
        ),
        // Another column list.
        (
            "WITH c (k, v) AS (SELECT Id, Selected FROM dbo.D) INSERT INTO dbo.E (Id, V) SELECT k, v FROM c;",
            "WITH c (v, k) AS (SELECT Id, Selected FROM D) INSERT INTO E (Id, V) SELECT k, v FROM c;",
        ),
        // PostgreSQL without RECURSIVE: the CTE's own name is the table.
        (
            "WITH D (Id, Selected) AS (SELECT Id, Selected FROM dbo.D) INSERT INTO dbo.E (Id, V) SELECT Id, Selected FROM D;",
            "WITH D (Id, Selected) AS (SELECT Id, Selected FROM D) INSERT INTO E (Id, V) SELECT Id, Selected FROM D;",
        ),
        // PostgreSQL with RECURSIVE: a later CTE's name is that CTE, where
        // T-SQL reads the table.
        (
            "WITH a AS (SELECT Id, Selected FROM E2), E2 AS (SELECT Id, Selected FROM dbo.D) INSERT INTO dbo.E (Id, V) SELECT Id, Selected FROM a;",
            "WITH RECURSIVE a AS (SELECT Id, Selected FROM E2), E2 AS (SELECT Id, Selected FROM D) INSERT INTO E (Id, V) SELECT Id, Selected FROM a;",
        ),
        // A window function in the recursive member.
        (
            "WITH N (i, r) AS (SELECT 1, CAST(1 AS bigint) UNION ALL SELECT i + 1, ROW_NUMBER() OVER (ORDER BY i) FROM N WHERE i < @n) INSERT INTO dbo.E (Id, V) SELECT i, i FROM N;",
            "WITH RECURSIVE N (i, r) AS (SELECT 1, CAST(1 AS bigint) UNION ALL SELECT i + 1, row_number() OVER (ORDER BY i) FROM N WHERE i < p_n) INSERT INTO E (Id, V) SELECT i, i FROM N;",
        ),
        // The recursive member reads the CTE twice.
        (
            "WITH N (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM N WHERE i < (SELECT MAX(i) FROM N)) INSERT INTO dbo.E (Id, V) SELECT i, i FROM N;",
            "WITH RECURSIVE N (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM N WHERE i < (SELECT MAX(i) FROM N)) INSERT INTO E (Id, V) SELECT i, i FROM N;",
        ),
        // Anchor and recursive member of other types.
        (
            "WITH N (i) AS (SELECT CAST(1 AS bigint) UNION ALL SELECT i + 1 FROM N WHERE i < @n) INSERT INTO dbo.E (Id, V) SELECT i, i FROM N;",
            "WITH RECURSIVE N (i) AS (SELECT CAST(1 AS bigint) UNION ALL SELECT CAST(i + 1 AS integer) FROM N WHERE i < p_n) INSERT INTO E (Id, V) SELECT i, i FROM N;",
        ),
    ] {
        assert_apart(a, b);
    }
}
