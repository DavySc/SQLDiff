//! Statement shapes the engine reads since burndown S: PostgreSQL
//! `make_interval` named arguments (DATEADD), a T-SQL SELECT assigning
//! several variables, a message that is an expression, and INSERT without a
//! column list. Each has an equal pair and near misses.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const CATALOG_T: &str =
    "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL, w nvarchar(20) NULL);";
const CATALOG_P: &str =
    "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL, w varchar(20) NULL);";

/// A T-SQL procedure with `body`, against a PostgreSQL one with `body_p`.
/// Both declare a timestamp `@d` (datetime2(6)), a date `@dt`,
/// an int `@n`, a bigint `@b` and string `@e` (nvarchar(100)).
fn run(body: &str, body_p: &str) -> Report {
    let a = format!(
        "CREATE PROCEDURE dbo.p @id int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  DECLARE @d datetime2(6);\n  DECLARE @dt date;\n  DECLARE @n int = 3;\n  DECLARE @b bigint = 4;\n  DECLARE @e nvarchar(100) = N'e';\n{body}\nEND\n"
    );
    let b = format!(
        "CREATE OR REPLACE PROCEDURE p(p_id integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_d timestamp(6);\n  v_dt date;\n  v_n integer;\n  v_b bigint;\n  v_e varchar(100);\nBEGIN\n  v_n := 3;\n  v_b := 4;\n  v_e := 'e';\n{body_p}\nEND;\n$$;\n"
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

/// The verdict and reasons of the statement under test only: the
/// declarations around it are the same in every case.
fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|x| x.id.as_str()).collect()
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

fn not_equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{body}\n{body_p}\n{:?}",
        ids(&r)
    );
    r
}

#[test]
fn baseline_is_equal() {
    equal("", "");
}

#[test]
fn dateadd_against_make_interval() {
    let r = equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(hours => 7));",
    );
    assert!(ids(&r).contains(&"ASM-DATEADD-RANGE"), "{:?}", ids(&r));
    equal(
        "  SET @d = DATEADD(second, @n, @d);",
        "  v_d := (v_d + make_interval(secs => v_n));",
    );
    equal(
        "  SET @d = DATEADD(mi, -5, @d);",
        "  v_d := (v_d + make_interval(mins => -5));",
    );
    // Positional and other zero amounts are the same call.
    equal(
        "  SET @d = DATEADD(day, @n, @d);",
        "  v_d := (v_d + make_interval(0, 0, 0, v_n));",
    );
    equal(
        "  SET @d = DATEADD(day, @n, @d);",
        "  v_d := (v_d + make_interval(days => v_n, secs => 0.0));",
    );
    let r = equal(
        "  SET @d = DATEADD(year, 1, @d);",
        "  v_d := (v_d + make_interval(years => 1));",
    );
    assert!(ids(&r).contains(&"ASM-DATEADD-YEAR-END"), "{:?}", ids(&r));
    // Another amount, field or operand.
    not_equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(hours => 8));",
    );
    not_equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(mins => 7));",
    );
    not_equal(
        "  SET @d = DATEADD(hour, 7, @d);",
        "  v_d := (v_d + make_interval(hours => 7, mins => 1));",
    );
    not_equal(
        "  SET @d = DATEADD(day, @n, @d);",
        "  v_d := (v_d + make_interval(0, 0, 0, 0, v_n));",
    );
    // A name make_interval does not have, a repeated one, and a positional
    // argument after a named one fail in PostgreSQL.
    for bad in [
        "make_interval(hour => 7)",
        "make_interval(hours => 7, hours => 7)",
        "make_interval(hours => 7, 0)",
    ] {
        not_equal(
            "  SET @d = DATEADD(hour, 7, @d);",
            &format!("  v_d := (v_d + {bad});"),
        );
    }
    // T-SQL truncates a bigint or decimal amount to int (or fails), PostgreSQL
    // keeps a fraction of seconds.
    not_equal(
        "  SET @d = DATEADD(second, @b, @d);",
        "  v_d := (v_d + make_interval(secs => v_b));",
    );
}

#[test]
fn dateadd_on_a_date() {
    equal("  SET @dt = DATEADD(day, 1, @dt);", "  v_dt := (v_dt + 1);");
    equal(
        "  SET @dt = DATEADD(dd, @n, @dt);",
        "  v_dt := (v_dt + v_n);",
    );
    equal(
        "  SET @dt = DATEADD(week, 2, @dt);",
        "  v_dt := (v_dt + 14);",
    );
    equal(
        "  SET @dt = DATEADD(month, @n, @dt);",
        "  v_dt := (v_dt + make_interval(months => v_n))::date;",
    );
    not_equal("  SET @dt = DATEADD(day, 1, @dt);", "  v_dt := (v_dt + 2);");
    not_equal(
        "  SET @dt = DATEADD(week, 2, @dt);",
        "  v_dt := (v_dt + 2);",
    );
    not_equal(
        "  SET @dt = DATEADD(dd, @b, @dt);",
        "  v_dt := (v_dt + v_b);",
    );
    not_equal(
        "  SET @dt = DATEADD(month, @n, @dt);",
        "  v_dt := (v_dt + make_interval(days => v_n))::date;",
    );
}

/// The PL/pgSQL block the conversion writes for a SELECT assigning `pairs`
/// (variable, column) from `query`'s columns `sqldiff_c1`, ….
fn loop_block(query: &str, pairs: &[(&str, &str)]) -> String {
    let body: String = pairs
        .iter()
        .map(|(v, c)| format!("    {v} := sqldiff_r.{c};\n"))
        .collect();
    format!("  DECLARE\n  sqldiff_r record;\n  BEGIN\n  FOR sqldiff_r IN {query} LOOP\n{body}  END LOOP;\n  END;")
}

#[test]
fn select_assigning_several_variables() {
    let tsql = "  SELECT @n = v, @e = w FROM dbo.t WHERE id = @id;";
    let q = "SELECT v AS sqldiff_c1, w AS sqldiff_c2 FROM t WHERE id = p_id";
    equal(
        tsql,
        &loop_block(q, &[("v_n", "sqldiff_c1"), ("v_e", "sqldiff_c2")]),
    );
    // The order of the assignments in the loop does not matter.
    equal(
        tsql,
        &loop_block(q, &[("v_e", "sqldiff_c2"), ("v_n", "sqldiff_c1")]),
    );
    // Another query (TOP 1 without a unique order keeps "some" row on both).
    equal(
        "  SELECT TOP 1 @n = v, @e = w FROM dbo.t ORDER BY id DESC;",
        &loop_block(
            "SELECT v AS sqldiff_c1, w AS sqldiff_c2 FROM t ORDER BY id DESC LIMIT 1",
            &[("v_n", "sqldiff_c1"), ("v_e", "sqldiff_c2")],
        ),
    );
    // Values swapped between the variables, another filter.
    not_equal(
        tsql,
        &loop_block(q, &[("v_n", "sqldiff_c2"), ("v_e", "sqldiff_c1")]),
    );
    not_equal(
        tsql,
        &loop_block(
            "SELECT v AS sqldiff_c1, w AS sqldiff_c2 FROM t WHERE id = 1",
            &[("v_n", "sqldiff_c1"), ("v_e", "sqldiff_c2")],
        ),
    );
    // A T-SQL item that reads a variable the SELECT assigns depends on the
    // order T-SQL assigns in, and a variable assigned twice: not compared.
    not_equal(
        "  SELECT @n = v, @e = @e + w FROM dbo.t WHERE id = @id;",
        &loop_block(
            "SELECT v AS sqldiff_c1, v_e || w AS sqldiff_c2 FROM t WHERE id = p_id",
            &[("v_n", "sqldiff_c1"), ("v_e", "sqldiff_c2")],
        ),
    );
    not_equal(
        "  SELECT @n = v, @n = id FROM dbo.t WHERE id = @id;",
        &loop_block(
            "SELECT v AS sqldiff_c1, id AS sqldiff_c2 FROM t WHERE id = p_id",
            &[("v_n", "sqldiff_c2")],
        ),
    );
}

#[test]
fn a_message_that_is_an_expression() {
    equal(
        "  PRINT @e + N' done';",
        "  RAISE NOTICE '%', COALESCE(v_e || ' done', '');",
    );
    equal(
        "  PRINT @e;",
        "  RAISE NOTICE '%', COALESCE(CAST(v_e AS text), '');",
    );
    equal(
        "  PRINT @n;",
        "  RAISE NOTICE '%', COALESCE(CAST(v_n AS text), '');",
    );
    // PostgreSQL shows NULL as <NULL>, PRINT as an empty line.
    not_equal("  PRINT @e;", "  RAISE NOTICE '%', v_e;");
    not_equal(
        "  PRINT @e;",
        "  RAISE NOTICE '%', COALESCE(v_e, '(null)');",
    );
    // Another value, or a value with another text.
    not_equal(
        "  PRINT @e;",
        "  RAISE NOTICE '%', COALESCE(v_e || 'x', '');",
    );
    not_equal(
        "  PRINT @d;",
        "  RAISE NOTICE '%', COALESCE(CAST(v_d AS text), '');",
    );
    // PRINT cuts a message after 4,000 characters (nvarchar).
    not_equal(
        "  PRINT CAST(@e AS nvarchar(max));",
        "  RAISE NOTICE '%', COALESCE(CAST(v_e AS text), '');",
    );
    // A message an error does not match.
    not_equal(
        "  PRINT @e;",
        "  RAISE EXCEPTION '%', COALESCE(v_e, '') USING ERRCODE = 'P0001';",
    );
}

#[test]
fn insert_without_a_column_list() {
    equal(
        "  INSERT INTO dbo.t VALUES (@id, 2, N'x');",
        "  INSERT INTO t VALUES (p_id, 2, 'x');",
    );
    equal(
        "  INSERT INTO dbo.t VALUES (@id, 2, N'x');",
        "  INSERT INTO t (id, v, w) VALUES (p_id, 2, 'x');",
    );
    equal(
        "  INSERT INTO dbo.t SELECT id, v, w FROM dbo.t WHERE id = 1;",
        "  INSERT INTO t (id, v, w) SELECT id, v, w FROM t WHERE id = 1;",
    );
    // The values go to the columns in the table's order.
    not_equal(
        "  INSERT INTO dbo.t VALUES (@id, 2, N'x');",
        "  INSERT INTO t (id, w, v) VALUES (p_id, 2, 'x');",
    );
    // PostgreSQL fills the leading columns of a shorter row; T-SQL refuses it.
    not_equal(
        "  INSERT INTO dbo.t VALUES (@id, 2);",
        "  INSERT INTO t VALUES (p_id, 2);",
    );
    // T-SQL leaves an identity column out of the list, PostgreSQL does not.
    not_equal(
        "  INSERT INTO dbo.u VALUES (2);",
        "  INSERT INTO u (v) VALUES (2);",
    );
}

#[test]
fn insert_without_a_column_list_into_a_table_variable() {
    let tsql = "  DECLARE @s TABLE (street nvarchar(20), n int);\n  INSERT INTO @s VALUES (N'Elm', 1), (N'Oak', 2);";
    let pg = |cols: &str, insert: &str| {
        format!("  DROP TABLE IF EXISTS pg_temp.tv_p_s;\n  CREATE TEMP TABLE pg_temp.tv_p_s ({cols});\n  {insert}")
    };
    equal(
        tsql,
        &pg(
            "street varchar(20), n integer",
            "INSERT INTO pg_temp.tv_p_s VALUES ('Elm', 1), ('Oak', 2);",
        ),
    );
    // The columns in another order take other values.
    not_equal(
        tsql,
        &pg(
            "n integer, street varchar(20)",
            "INSERT INTO pg_temp.tv_p_s (street, n) VALUES (2, 'Elm'), (1, 'Oak');",
        ),
    );
    not_equal(
        tsql,
        &pg(
            "street varchar(20), n integer",
            "INSERT INTO pg_temp.tv_p_s VALUES ('Elm', 2), ('Oak', 1);",
        ),
    );
}
