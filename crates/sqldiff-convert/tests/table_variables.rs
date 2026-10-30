//! T-SQL table variables: a PostgreSQL TEMP table in pg_temp (dropped first,
//! so each run starts empty) or a Db2 for i NOT LOGGED global temporary
//! table, named after the routine and the variable so a called routine never
//! replaces its caller's table. A DECLARE that does not run exactly once, a
//! function's table variable on PostgreSQL, and a routine that rolls back
//! are not passed silently.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

fn conv(text: &str, to: Dialect) -> Conversion {
    convert(text, Dialect::Tsql, to).unwrap()
}

/// The unit whose input contains `needle`.
fn unit<'a>(c: &'a Conversion, input: &str, needle: &str) -> &'a Unit {
    let at = input
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in input"));
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end.max(u.src.start + 1))
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap_or_else(|| panic!("no unit for {needle}"))
}

/// Whether one of the unit's notes says `s`.
fn says(u: &Unit, s: &str) -> bool {
    u.notes.iter().any(|n| n.text.contains(s))
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

const PROC: &str = "CREATE PROCEDURE dbo.LogErrors @n int AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @Errors TABLE (ID int IDENTITY PRIMARY KEY, Severity int NOT NULL);\n  INSERT INTO @Errors (Severity) VALUES (@n);\n  DELETE FROM @Errors WHERE Severity < 10;\n  SELECT @n = COUNT(*) FROM @Errors;\nEND\n";

#[test]
fn postgresql_gets_a_fresh_temp_table_and_the_engine_proves_it() {
    let c = conv(PROC, Dialect::Postgres);
    let o = &c.output;
    assert!(
        o.contains("DROP TABLE IF EXISTS pg_temp.tv_logerrors_errors;\n  CREATE TEMP TABLE pg_temp.tv_logerrors_errors ("),
        "{o}"
    );
    assert!(
        o.contains("INSERT INTO pg_temp.tv_logerrors_errors (Severity) VALUES (p_n);"),
        "{o}"
    );
    assert!(
        o.contains("DELETE FROM pg_temp.tv_logerrors_errors WHERE Severity < 10;"),
        "{o}"
    );
    for needle in [
        "DECLARE @Errors",
        "INSERT INTO @Errors",
        "DELETE FROM @Errors",
    ] {
        let u = unit(&c, PROC, needle);
        assert_eq!(u.status, Status::Assumptions, "{needle}: {u:#?}\n{o}");
        assert!(
            rules(u).contains(&"ASM-TABLE-VARIABLE"),
            "{needle}: {:?}",
            rules(u)
        );
    }
}

#[test]
fn db2_names_carry_the_routine() {
    let c = conv(PROC, Dialect::Db2i);
    let o = &c.output;
    assert!(
        o.contains("DECLARE GLOBAL TEMPORARY TABLE SESSION.tv_logerrors_errors ("),
        "{o}"
    );
    assert!(
        o.contains(") ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE;"),
        "{o}"
    );
    // The DECLARE is compared, but Db2 for i column types have no verified facts yet.
    let u = unit(&c, PROC, "DECLARE @Errors");
    assert_eq!(u.status, Status::Unproven, "{u:#?}");
}

#[test]
fn caller_and_callee_get_their_own_tables() {
    // Ola Hallengren's DatabaseBackup and CommandExecute both declare
    // @Errors, and the first calls the second.
    let src = "CREATE PROCEDURE dbo.Inner AS\nBEGIN\n  DECLARE @Errors TABLE (Severity int);\n  INSERT INTO @Errors (Severity) VALUES (1);\nEND\nGO\nCREATE PROCEDURE dbo.Outer AS\nBEGIN\n  DECLARE @Errors TABLE (Severity int);\n  INSERT INTO @Errors (Severity) VALUES (2);\n  EXEC dbo.Inner;\n  SELECT COUNT(*) FROM @Errors;\nEND\n";
    for (d, schema) in [(Dialect::Postgres, "pg_temp"), (Dialect::Db2i, "SESSION")] {
        let o = conv(src, d).output;
        assert!(o.contains(&format!("{schema}.tv_inner_errors")), "{o}");
        assert!(o.contains(&format!("{schema}.tv_outer_errors")), "{o}");
    }
}

#[test]
fn a_declare_that_runs_more_than_once_is_refused() {
    // In a loop T-SQL keeps the rows from pass to pass; a temporary table
    // created there would start empty each time.
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  DECLARE @i int = 0;\n  WHILE @i < 2\n  BEGIN\n    DECLARE @seen TABLE (id int);\n    INSERT INTO @seen (id) VALUES (@i);\n    SET @i += 1;\n  END\nEND\n";
    for d in [Dialect::Postgres, Dialect::Db2i] {
        let c = conv(src, d);
        assert!(!c.output.contains("tv_p_seen ("), "{d:?}: {}", c.output);
        assert!(
            c.units
                .iter()
                .any(|u| u.status == Status::NotConverted
                    && says(u, "inside IF, WHILE or TRY/CATCH")),
            "{d:?}: {:#?}",
            c.units
        );
    }
}

#[test]
fn a_postgresql_function_table_variable_is_refused() {
    let src = "CREATE FUNCTION dbo.f() RETURNS int AS\nBEGIN\n  DECLARE @t TABLE (id int);\n  INSERT INTO @t (id) VALUES (1);\n  RETURN (SELECT COUNT(*) FROM @t);\nEND\n";
    let c = conv(src, Dialect::Postgres);
    let u = unit(&c, src, "DECLARE @t");
    assert_eq!(u.status, Status::NotConverted, "{u:#?}");
    assert!(says(u, "VOLATILE"), "{u:#?}");
}

#[test]
fn a_routine_that_rolls_back_needs_a_decision_on_postgresql() {
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @t TABLE (id int);\n  BEGIN TRAN;\n  INSERT INTO @t (id) VALUES (1);\n  ROLLBACK;\n  SELECT COUNT(*) FROM @t;\nEND\n";
    let c = conv(src, Dialect::Postgres);
    let u = unit(&c, src, "DECLARE @t");
    assert!(
        rules(u).contains(&"pg.table-variable-rollback"),
        "{:?}",
        rules(u)
    );
    assert_ne!(u.status, Status::Assumptions, "{u:#?}");
    assert_ne!(u.status, Status::Converted, "{u:#?}");
    // Db2 for i keeps the rows on rollback (V16): no decision there.
    let c = conv(src, Dialect::Db2i);
    let u = unit(&c, src, "DECLARE @t");
    assert!(
        !rules(u).contains(&"pg.table-variable-rollback"),
        "{:?}",
        rules(u)
    );
}

#[test]
fn rollback_and_rethrow_needs_no_decision() {
    // The error ends the routine, and its table variable with it.
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  DECLARE @t TABLE (id int);\n  BEGIN TRY\n    INSERT INTO dbo.x (id) VALUES (1);\n  END TRY\n  BEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK;\n    THROW;\n  END CATCH\nEND\n";
    let c = conv(src, Dialect::Postgres);
    let u = unit(&c, src, "DECLARE @t");
    assert!(
        !rules(u).contains(&"pg.table-variable-rollback"),
        "{:?}",
        rules(u)
    );
    assert!(
        c.output.contains("CREATE TEMP TABLE pg_temp.tv_p_t ("),
        "{}",
        c.output
    );
}

#[test]
fn bit_columns_of_table_variables_and_temp_tables_read_as_boolean() {
    // Ola Hallengren's `@URLs` has `Mirror bit` and reads `Mirror = 0`; a
    // kept `= 0` is a type error on the boolean column PostgreSQL gets.
    let src = "CREATE PROCEDURE dbo.p @n int OUTPUT AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @URLs TABLE (ID int, DirectoryPath nvarchar(4000), Mirror bit);\n  CREATE TABLE #t (Name nvarchar(100), Flag bit);\n  INSERT INTO @URLs (ID, DirectoryPath, Mirror) VALUES (1, N'x', 0);\n  INSERT INTO #t (Name, Flag) VALUES (N'a', 1);\n  SELECT @n = COUNT(*) FROM @URLs WHERE Mirror = 0;\n  SELECT @n = COUNT(*) FROM #t WHERE Flag = 1;\n  SELECT @n = COUNT(*) FROM @URLs u WHERE u.Mirror = 1;\nEND\n";
    let c = conv(src, Dialect::Postgres);
    let o = &c.output;
    for want in [
        "VALUES (1, 'x', FALSE);",
        "VALUES ('a', TRUE);",
        "WHERE Mirror = FALSE;",
        "WHERE Flag = TRUE;",
        "WHERE u.Mirror = TRUE;",
    ] {
        assert!(o.contains(want), "{want}\n{o}");
    }
    for needle in [
        "INSERT INTO @URLs",
        "INSERT INTO #t",
        "FROM @URLs WHERE",
        "FROM #t WHERE",
        "FROM @URLs u",
    ] {
        let u = unit(&c, src, needle);
        assert_eq!(u.status, Status::Assumptions, "{needle}: {u:#?}\n{o}");
    }
}

#[test]
fn bit_columns_in_if_and_while_conditions_read_as_boolean() {
    // Ola's DatabaseBackup: `IF (SELECT COUNT(*) FROM @Directories WHERE
    // Mirror = 0) <> …`. The condition's subqueries know their tables'
    // column types, a catalog table's as a table variable's.
    let src = "CREATE TABLE dbo.Jobs (ID int PRIMARY KEY, Done bit NOT NULL);\nGO\nCREATE PROCEDURE dbo.p @n int OUTPUT AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @Directories TABLE (ID int, Mirror bit);\n  INSERT INTO @Directories (ID, Mirror) VALUES (1, 0);\n  IF (SELECT COUNT(*) FROM @Directories WHERE Mirror = 0) <> (SELECT COUNT(*) FROM @Directories WHERE Mirror = 1)\n  BEGIN\n    SET @n = 1;\n  END\n  WHILE EXISTS (SELECT * FROM dbo.Jobs WHERE Done = 0) SET @n = 2;\nEND\n";
    let c = conv(src, Dialect::Postgres);
    let o = &c.output;
    for want in [
        "IF (SELECT COUNT(*) FROM pg_temp.tv_p_directories WHERE Mirror = FALSE) <> (SELECT COUNT(*) FROM pg_temp.tv_p_directories WHERE Mirror = TRUE) THEN",
        "WHILE EXISTS (SELECT * FROM Jobs WHERE Done = FALSE) LOOP",
    ] {
        assert!(o.contains(want), "{want}\n{o}");
    }
    assert!(!o.contains("= 0)"), "{o}");
}

#[test]
fn a_rollback_postgresql_never_runs_needs_no_decision() {
    // Ola's CATCH: `IF XACT_STATE() <> 0 ROLLBACK`, then carry on. The
    // PostgreSQL conversion refuses transaction control inside TRY and
    // CATCH, so no ROLLBACK of its own empties the temporary table.
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @t TABLE (id int);\n  BEGIN TRY\n    INSERT INTO dbo.x (id) VALUES (1);\n  END TRY\n  BEGIN CATCH\n    IF XACT_STATE() <> 0\n    BEGIN\n      ROLLBACK TRANSACTION\n    END\n    SELECT COUNT(*) FROM @t;\n  END CATCH\nEND\n";
    let c = conv(src, Dialect::Postgres);
    let u = unit(&c, src, "DECLARE @t");
    assert!(
        !rules(u).contains(&"pg.table-variable-rollback"),
        "{:?}",
        rules(u)
    );
    assert!(!c.output.contains("\n    ROLLBACK;"), "{}", c.output);
}

#[test]
fn bit_columns_inside_a_derived_table_read_as_boolean() {
    // Ola's `UPDATE … FROM @t t INNER JOIN (SELECT … FROM @s s WHERE
    // s.Selected = 1 GROUP BY …) d ON …`: the derived table's own tables
    // type its columns.
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @t TABLE (ID int IDENTITY PRIMARY KEY, Name nvarchar(100), StartPosition int);\n  DECLARE @s TABLE (Name nvarchar(100), StartPosition int, Selected bit DEFAULT 0);\n  UPDATE t\n  SET t.StartPosition = d.StartPosition\n  FROM @t t\n  INNER JOIN (SELECT t2.Name, MIN(s.StartPosition) AS StartPosition\n              FROM @t t2\n              INNER JOIN @s s ON t2.Name = s.Name\n              WHERE s.Selected = 1\n              GROUP BY t2.Name) d\n  ON t.Name = d.Name;\nEND\n";
    let c = conv(src, Dialect::Postgres);
    assert!(c.output.contains("WHERE s.Selected = TRUE"), "{}", c.output);
    assert!(!c.output.contains("s.Selected = 1"), "{}", c.output);
}
