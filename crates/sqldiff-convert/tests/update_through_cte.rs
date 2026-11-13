//! `WITH c AS (SELECT … FROM t) UPDATE c SET col = computed` (Ola
//! Hallengren's ROW_NUMBER renumbering) is converted by row identity; every
//! other shape through a CTE stays refused.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

fn pg(src: &str) -> Conversion {
    convert(src, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn proc(body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p\nAS\nBEGIN\n  DECLARE @t TABLE (ID int IDENTITY(1,1) PRIMARY KEY, Name nvarchar(100), [Order] int, Pos int, Selected bit);\n  INSERT INTO @t (Name, Pos, Selected) VALUES (N'a', 2, 1);\n{body}\nEND\n"
    )
}

const OLA: &str = "  WITH t AS (\n  SELECT Name, [Order], ROW_NUMBER() OVER (ORDER BY Pos ASC, Name ASC) AS RowNumber\n  FROM @t t\n  WHERE Selected = 1\n  )\n  UPDATE t\n  SET [Order] = RowNumber;";

/// The output without comment lines (a refusal's reason may name `ctid`).
fn live(c: &Conversion) -> String {
    c.output
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

/// The smallest unit that holds `needle` of the source.
fn unit_of<'a>(c: &'a Conversion, src: &str, needle: &str) -> &'a sqldiff_convert::Unit {
    let at = src
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in source"));
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end.max(u.src.start + 1))
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap_or_else(|| panic!("no unit for {needle}\n{}", c.output))
}

#[test]
fn the_row_number_renumbering_is_converted_by_row_identity() {
    let src = proc(OLA);
    let c = pg(&src);
    let u = unit_of(&c, &src, "UPDATE t");
    assert_ne!(
        u.status,
        Status::NotConverted,
        "{:?}\n{}",
        u.notes,
        c.output
    );
    assert_ne!(u.status, Status::Differs, "{:?}\n{}", u.notes, c.output);
    let live: String = c
        .output
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    // The base table is updated from the CTE's query by ctid, never by the CTE.
    assert!(
        live.contains(
            "UPDATE pg_temp.tv__dbo__p__t AS sqldiff_t SET \"Order\" = sqldiff_c.RowNumber"
        ),
        "{live}"
    );
    assert!(live.contains("t.ctid AS sqldiff_rid"), "{live}");
    assert!(
        live.contains("WHERE sqldiff_t.ctid = sqldiff_c.sqldiff_rid"),
        "{live}"
    );
    assert!(
        live.contains("WHERE Selected = TRUE) AS sqldiff_c"),
        "{live}"
    );
}

#[test]
fn the_engine_does_not_call_the_converted_update_different() {
    // The engine reads the T-SQL target as the base table, so the write is
    // paired; it does not compare the statement (not proven), never `differs`.
    let src = proc(OLA);
    let c = pg(&src);
    let u = unit_of(&c, &src, "UPDATE t");
    assert!(
        matches!(
            u.status,
            Status::Unproven | Status::Assumptions | Status::Converted
        ),
        "{:?}",
        u.status
    );
}

#[test]
fn other_shapes_through_a_cte_stay_refused() {
    let cases = [
        // DISTINCT, a join, a grouping in the CTE's query.
        "  WITH t AS (SELECT DISTINCT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n;",
        "  WITH t AS (SELECT a.Name, a.[Order], 1 AS n FROM @t a JOIN @t b ON a.ID = b.ID)\n  UPDATE t SET [Order] = n;",
        "  WITH t AS (SELECT Name, [Order], COUNT(*) AS n FROM @t GROUP BY Name, [Order])\n  UPDATE t SET [Order] = n;",
        // A value that is not a computed CTE column.
        "  WITH t AS (SELECT Name, [Order], Pos FROM @t)\n  UPDATE t SET [Order] = Pos;",
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n + 1;",
        // The target column is computed in the CTE.
        "  WITH t AS (SELECT Name, ROW_NUMBER() OVER (ORDER BY Pos) AS n FROM @t)\n  UPDATE t SET n = 1;",
        // A WHERE or FROM on the UPDATE.
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n WHERE Name = N'a';",
        // Two CTEs.
        "  WITH u AS (SELECT 1 AS k), t AS (SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n;",
        // A column list renames the CTE's columns by position.
        "  WITH t ([Order], Name, n) AS (SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n;",
        // Two CTE columns with one name.
        "  WITH t AS (SELECT Name, [Order], 1 AS n, 2 AS n FROM @t)\n  UPDATE t SET [Order] = n;",
        // TOP, UNION, ORDER BY in the CTE; OUTPUT, FROM on the UPDATE.
        "  WITH t AS (SELECT TOP 1 Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n;",
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t UNION ALL SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n;",
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t ORDER BY Name)\n  UPDATE t SET [Order] = n;",
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n OUTPUT inserted.Name;",
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t)\n  UPDATE t SET [Order] = n FROM t JOIN @t x ON x.ID = 1;",
        // DELETE through a CTE.
        "  WITH t AS (SELECT Name, [Order], 1 AS n FROM @t)\n  DELETE FROM t;",
    ];
    for body in cases {
        let src = proc(body);
        let c = pg(&src);
        let u = unit_of(&c, &src, "WITH ");
        assert_eq!(u.status, Status::NotConverted, "{body}\n{}", c.output);
        assert!(!live(&c).contains("ctid"), "{body}\n{}", c.output);
    }
}

#[test]
fn a_view_has_no_row_identity() {
    let src = "CREATE VIEW dbo.v AS SELECT 1 AS a, 2 AS b;\nGO\nCREATE PROCEDURE dbo.p AS\nBEGIN\n  WITH c AS (SELECT a, b, ROW_NUMBER() OVER (ORDER BY a) AS n FROM dbo.v)\n  UPDATE c SET b = n;\nEND\n";
    let c = pg(src);
    assert!(!live(&c).contains("ctid"), "{}", c.output);
}

#[test]
fn db2_stays_refused() {
    let c = convert(&proc(OLA), Dialect::Tsql, Dialect::Db2i).unwrap();
    assert!(!live(&c).contains("ctid"), "{}", c.output);
}

#[test]
fn a_catalog_table_is_refused_but_a_temp_table_is_converted() {
    // A catalog table may be partitioned or inherited on PostgreSQL, and
    // another session may change it while the UPDATE waits: no row identity.
    let cat = "CREATE TABLE dbo.T (ID int NOT NULL PRIMARY KEY, Name nvarchar(10), [Order] int, Pos int);\nGO\nCREATE PROCEDURE dbo.p AS\nBEGIN\n  WITH c AS (SELECT Name, [Order], ROW_NUMBER() OVER (ORDER BY Pos) AS n FROM dbo.T)\n  UPDATE c SET [Order] = n;\nEND\n";
    let c = pg(cat);
    assert!(!live(&c).contains("ctid"), "{}", c.output);
    let u = unit_of(&c, cat, "WITH c");
    assert_eq!(u.status, Status::NotConverted, "{}", c.output);
    let tmp = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  CREATE TABLE #t (ID int, Name nvarchar(10), [Order] int, Pos int);\n  WITH c AS (SELECT Name, [Order], ROW_NUMBER() OVER (ORDER BY Pos) AS n FROM #t)\n  UPDATE c SET [Order] = n;\nEND\n";
    let c = pg(tmp);
    assert!(live(&c).contains("ctid"), "{}", c.output);
}

#[test]
fn a_cte_named_like_its_catalog_source_is_refused() {
    // SQL Server rejects it (Msg 252); a catalog source is refused anyway.
    let src = "CREATE TABLE dbo.T (ID int NOT NULL PRIMARY KEY, [Order] int, Pos int);\nGO\nCREATE PROCEDURE dbo.p AS\nBEGIN\n  WITH T AS (SELECT [Order], ROW_NUMBER() OVER (ORDER BY Pos) AS n FROM dbo.T)\n  UPDATE T SET [Order] = n;\nEND\n";
    assert!(!live(&pg(src)).contains("ctid"));
}
