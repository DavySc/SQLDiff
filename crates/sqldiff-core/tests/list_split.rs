//! Ola Hallengren's list splits (a recursive CTE over a delimited list) in
//! statement equivalence: T-SQL against the PostgreSQL the converter writes.
//! What each case relies on was run on SQL Server 2022 and PostgreSQL 18:
//! CHARINDEX with a start in a MAX string is bigint and gives 0 for a start
//! past the end (eval/cte/2026-10-30), LEFT and RIGHT against substr
//! (eval/builtins/2026-10-30-lr), PARSENAME against the converter's pattern
//! form (eval/builtins/2026-10-30-parsename).

use sqldiff_core::equiv::canon::parsename_pg;
use sqldiff_core::{diff, Artifact, Dialect, Options, Report};

const MS_DDL: &str = "CREATE TABLE dbo.S (Item nvarchar(max) NULL, Pos int NULL, Selected bit NULL, Rn bigint NULL, P nvarchar(128) NULL);";
const PG_DDL: &str = "CREATE TABLE S (Item text NULL, Pos integer NULL, Selected boolean NULL, Rn bigint NULL, P varchar(128) NULL);";

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
                "CREATE PROCEDURE dbo.p @List nvarchar(max), @D nvarchar(10), @n int
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
                "CREATE OR REPLACE PROCEDURE p(p_list text, p_d varchar(10), p_n integer)
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

/// Ola's split, as in DatabaseBackup: the positions of each item, a `-`
/// prefix deselects it, then the items in order and their last name part.
fn split_a(item: &str) -> String {
    format!(
        "WITH L1 (StartPosition, EndPosition, Item) AS
  (
  SELECT 1 AS StartPosition,
         ISNULL(NULLIF(CHARINDEX(@D, @List, 1), 0), LEN(@List) + 1) AS EndPosition,
         SUBSTRING(@List, 1, ISNULL(NULLIF(CHARINDEX(@D, @List, 1), 0), LEN(@List) + 1) - 1) AS Item
  WHERE @List IS NOT NULL
  UNION ALL
  SELECT CAST(EndPosition AS int) + 1 AS StartPosition,
         ISNULL(NULLIF(CHARINDEX(@D, @List, EndPosition + 1), 0), LEN(@List) + 1) AS EndPosition,
         SUBSTRING(@List, EndPosition + 1, ISNULL(NULLIF(CHARINDEX(@D, @List, EndPosition + 1), 0), LEN(@List) + 1) - EndPosition - 1) AS Item
  FROM L1
  WHERE EndPosition < LEN(@List) + 1
  ),
  L2 (Item, StartPosition, Selected) AS
  (
  SELECT CASE WHEN Item LIKE '-%' THEN RIGHT(Item, LEN(Item) - 1) ELSE Item END AS Item,
         StartPosition,
         CASE WHEN Item LIKE '-%' THEN 0 ELSE 1 END AS Selected
  FROM L1
  )
  INSERT INTO dbo.S (Item, Pos, Selected, Rn, P)
  SELECT {item}
  FROM L2
  OPTION (MAXRECURSION 0);"
    )
}

/// The converter's output for `split_a`.
fn split_b(item: &str) -> String {
    let pos = |s: &str| {
        format!("((strpos(substr(p_list, {s}), p_d) - 1 + greatest({s}, 1)) * (strpos(substr(p_list, {s}), p_d) > 0 AND length(p_d) > 0)::int)")
    };
    let (p1, pn) = (pos("1"), pos("EndPosition + 1"));
    format!(
        "WITH RECURSIVE L1 (StartPosition, EndPosition, Item) AS
  (
  SELECT 1 AS StartPosition,
         COALESCE(NULLIF({p1}, 0), length(rtrim(p_list)) + 1) AS EndPosition,
         SUBSTRING(p_list, 1, COALESCE(NULLIF({p1}, 0), length(rtrim(p_list)) + 1) - 1) AS Item
  WHERE p_list IS NOT NULL
  UNION ALL
  SELECT CAST(EndPosition AS integer) + 1 AS StartPosition,
         COALESCE(NULLIF({pn}, 0), length(rtrim(p_list)) + 1) AS EndPosition,
         SUBSTRING(p_list, EndPosition + 1, COALESCE(NULLIF({pn}, 0), length(rtrim(p_list)) + 1) - EndPosition - 1) AS Item
  FROM L1
  WHERE EndPosition < length(rtrim(p_list)) + 1
  ),
  L2 (Item, StartPosition, Selected) AS
  (
  SELECT CASE WHEN Item LIKE '-%' THEN substr(Item, length(Item) - (length(rtrim(Item)) - 1) + 1, length(rtrim(Item)) - 1) ELSE Item END AS Item,
         StartPosition,
         CASE WHEN Item LIKE '-%' THEN 0 ELSE 1 END AS Selected
  FROM L1
  ) INSERT INTO S (Item, Pos, Selected, Rn, P)
  SELECT {item}
  FROM L2;"
    )
}

const ITEM_A: &str =
    "Item, StartPosition, Selected, ROW_NUMBER() OVER (ORDER BY StartPosition), PARSENAME(Item, 1)";

fn item_b(part: u32) -> String {
    format!(
        "Item, StartPosition, (Selected <> 0), ROW_NUMBER() OVER (ORDER BY StartPosition NULLS FIRST), {}",
        parsename_pg(part).replace("__x__", "Item")
    )
}

#[test]
fn list_split_is_equal() {
    assert_equal(
        &split_a(ITEM_A),
        &split_b(&item_b(1)),
        &[
            "ASM-CTE-MAXRECURSION",
            "ASM-VALUE-SIZE-1GB",
            "ASM-TSQL-LEN-BMP",
            "ASM-ORDER-TOTAL",
        ],
    );
}

#[test]
fn list_split_near_misses_stay_apart() {
    let a = split_a(ITEM_A);
    let good = split_b(&item_b(1));
    for b in [
        // Selected the other way round.
        good.replace("(Selected <> 0)", "(Selected = 0)"),
        // Another order, or another key.
        good.replace(
            "ORDER BY StartPosition NULLS FIRST",
            "ORDER BY StartPosition DESC NULLS LAST",
        ),
        good.replace(
            "ORDER BY StartPosition NULLS FIRST",
            "ORDER BY Item NULLS FIRST",
        ),
        // Another name part.
        split_b(&item_b(2)),
        // RIGHT one character short.
        good.replace(
            "(length(rtrim(Item)) - 1) + 1,",
            "(length(rtrim(Item)) - 1),",
        ),
        // The step goes on from the same position.
        good.replace(
            "CAST(EndPosition AS integer) + 1 AS",
            "CAST(EndPosition AS integer) AS",
        ),
    ] {
        assert_apart(&a, &b);
    }
}

/// A position that grows by one each level has no bound from the anchor:
/// T-SQL's bigint cast to int is not taken as PostgreSQL's integer.
#[test]
fn growing_recursion_has_no_bound() {
    assert_apart(
        "WITH N (x) AS (SELECT CAST(1 AS bigint) UNION ALL SELECT x + 1 FROM N WHERE x < @n)
  INSERT INTO dbo.S (Pos) SELECT CAST(x AS int) FROM N OPTION (MAXRECURSION 0);",
        "WITH RECURSIVE N (x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM N WHERE x < p_n)
  INSERT INTO S (Pos) SELECT x FROM N;",
    );
}

/// PostgreSQL's right() with a count that is not a literal is not RIGHT:
/// a negative count drops characters where T-SQL raises an error.
#[test]
fn pg_right_needs_a_literal_count() {
    assert_equal(
        "INSERT INTO dbo.S (Item) SELECT RIGHT(@List, 3);",
        "INSERT INTO S (Item) SELECT right(p_list, 3);",
        &["ASM-TSQL-LEN-BMP"],
    );
    assert_apart(
        "INSERT INTO dbo.S (Item) SELECT RIGHT(@List, @n);",
        "INSERT INTO S (Item) SELECT right(p_list, p_n);",
    );
}
