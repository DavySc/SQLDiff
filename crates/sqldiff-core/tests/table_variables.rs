//! A T-SQL table variable compared with the temporary table it was converted
//! to (`classify::table_variable_table` names it): the DECLARE against the
//! PostgreSQL `DROP TABLE IF EXISTS` + `CREATE TEMP TABLE` or the Db2 for i
//! `DECLARE GLOBAL TEMPORARY TABLE`, and every statement using it against
//! the statement using the temporary table.

use sqldiff_core::classify::table_variable_table;
use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

fn run(a: &str, b: &str, d: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", d, b),
        &Options::default(),
    )
}

const TSQL: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @Errors TABLE (ID int IDENTITY PRIMARY KEY, Severity int NOT NULL);\n  INSERT INTO @Errors (Severity) VALUES (16);\n  DELETE FROM @Errors WHERE Severity < 10;\nEND\n";

/// The PostgreSQL procedure with `create` for the table's creation.
fn pg(create: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n{create}\n  INSERT INTO pg_temp.tv_p_errors (Severity) VALUES (16);\n  DELETE FROM pg_temp.tv_p_errors WHERE Severity < 10;\nEND;\n$$;\n")
}

const CREATE: &str = "  DROP TABLE IF EXISTS pg_temp.tv_p_errors;\n  CREATE TEMP TABLE pg_temp.tv_p_errors (\n    ID integer GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) PRIMARY KEY,\n    Severity integer NOT NULL\n  );";

fn reasons(r: &Report) -> String {
    format!("{:#?}\n{:#?}", r.findings, r.unknown_reasons)
}

#[test]
fn names_carry_the_routine_and_fit_postgresql() {
    assert_eq!(
        table_variable_table(Some("dbo.CommandExecute"), "@Errors"),
        "tv_commandexecute_errors"
    );
    assert_eq!(
        table_variable_table(Some("[dbo].[Command_Execute]"), "@tmpDatabases"),
        "tv_commandexecute_tmp_databases"
    );
    // A script's table variables cannot meet a routine's: `tv__` never
    // starts a routine's name (its routine part is never empty).
    assert_eq!(table_variable_table(None, "@x"), "tv__x");
    let long = table_variable_table(
        Some("dbo.IndexOptimize"),
        "@CurrentUpdateStatisticsWithClauseArguments",
    );
    assert!(long.len() <= 63, "{long}");
    assert!(
        long.starts_with("tv_indexoptimize_current_update_statistics"),
        "{long}"
    );
    assert_ne!(
        long,
        table_variable_table(
            Some("dbo.IndexOptimize"),
            "@CurrentUpdateStatisticsWithClauseArguments2"
        )
    );
}

#[test]
fn converted_table_variable_is_equal() {
    let r = run(TSQL, &pg(CREATE), P);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{}",
        reasons(&r)
    );
    assert!(r.unknown_reasons.is_empty(), "{}", reasons(&r));
    assert!(r.assumptions.iter().any(|a| a.id == "ASM-TABLE-VARIABLE"));
    assert!(
        !r.findings.iter().any(|f| f.rule == "write.unpaired"),
        "{}",
        reasons(&r)
    );
}

#[test]
fn a_column_that_differs_is_found() {
    let b = pg(&CREATE.replace("Severity integer NOT NULL", "Severity text NOT NULL"));
    let r = run(TSQL, &b, P);
    assert_eq!(r.verdict(), Verdict::Different, "{}", reasons(&r));
    assert!(
        r.findings
            .iter()
            .any(|f| f.witness.contains("Tables (A line 4, B line 6) differ")),
        "{}",
        reasons(&r)
    );
}

#[test]
fn creation_without_drop_is_not_proven() {
    let b = pg(&CREATE.replace("  DROP TABLE IF EXISTS pg_temp.tv_p_errors;\n", ""));
    let r = run(TSQL, &b, P);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("without dropping it first")),
        "{}",
        reasons(&r)
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn on_commit_drop_is_not_proven() {
    let b = pg(&CREATE.replace("\n  );", "\n  ) ON COMMIT DROP;"));
    let r = run(TSQL, &b, P);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("options after the column list")),
        "{}",
        reasons(&r)
    );
}

#[test]
fn another_name_is_not_paired() {
    // The caller's table: a routine that reads it is not the conversion.
    let b = pg(CREATE).replace("tv_p_errors", "tv_caller_errors");
    let r = run(TSQL, &b, P);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("creates no temporary table pg_temp.tv_p_errors")),
        "{}",
        reasons(&r)
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn use_before_creation_is_not_proven() {
    let b = pg(&format!("  DELETE FROM pg_temp.tv_p_errors;\n{CREATE}"));
    let r = run(TSQL, &b, P);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains("before line")),
        "{}",
        reasons(&r)
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn creation_in_a_loop_is_not_proven() {
    let b = pg(&format!("  FOR i IN 1..2 LOOP\n{CREATE}\n  END LOOP;"));
    let r = run(TSQL, &b, P);
    assert!(
        r.unknown_reasons.iter().any(
            |x| x.contains("inside a branch, loop") || x.contains("creates no temporary table")
        ),
        "{}",
        reasons(&r)
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn a_rollback_is_not_proven_on_postgresql() {
    let a = TSQL.replace(
        "  DELETE FROM @Errors WHERE Severity < 10;\n",
        "  DELETE FROM @Errors WHERE Severity < 10;\n  BEGIN TRAN;\n  ROLLBACK;\n",
    );
    let r = run(&a, &pg(CREATE), P);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains("rolls back")),
        "{}",
        reasons(&r)
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn db2_declaration_must_keep_rows_on_rollback() {
    let b = |opts: &str| {
        format!("CREATE OR REPLACE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DECLARE GLOBAL TEMPORARY TABLE SESSION.tv_p_errors (\n    ID INTEGER GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) NOT NULL PRIMARY KEY,\n    Severity INTEGER NOT NULL\n  ) {opts};\n  INSERT INTO SESSION.tv_p_errors (Severity) VALUES (16);\n  DELETE FROM SESSION.tv_p_errors WHERE Severity < 10;\nEND;\n")
    };
    let logged = run(
        TSQL,
        &b("ON COMMIT PRESERVE ROWS LOGGED WITH REPLACE"),
        Dialect::Db2i,
    );
    assert!(
        logged
            .unknown_reasons
            .iter()
            .any(|x| x.contains("ON ROLLBACK PRESERVE ROWS")),
        "{}",
        reasons(&logged)
    );
    // The right options: compared, but Db2 for i column types have no verified facts yet.
    let kept = run(
        TSQL,
        &b("ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE"),
        Dialect::Db2i,
    );
    assert!(
        kept.unknown_reasons
            .iter()
            .any(|x| x.contains("table variable @Errors") && x.contains("Db2 for i column types")),
        "{}",
        reasons(&kept)
    );
    assert!(
        !kept.findings.iter().any(|f| f.rule == "write.unpaired"),
        "{}",
        reasons(&kept)
    );
}

#[test]
fn a_dgtt_with_start_with_binds() {
    // `START WITH` inside the identity options is not the table's AS query.
    let b = "CREATE OR REPLACE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DECLARE GLOBAL TEMPORARY TABLE SESSION.tv_p_errors (ID INTEGER GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) NOT NULL PRIMARY KEY, Message VARCHAR(10)) ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE;\n  INSERT INTO SESSION.tv_p_errors (Message) VALUES ('x');\nEND;\n";
    let r = diff(
        &Artifact::new("b.sql", Dialect::Db2i, b),
        &Artifact::new("b.sql", Dialect::Db2i, b),
        &Options::default(),
    );
    assert!(
        !r.findings.iter().any(|f| f.rule.starts_with("bind.")),
        "{}",
        reasons(&r)
    );
}

/// Ola Hallengren's `@URLs` with its `Mirror bit`, read with `= 0`; on
/// PostgreSQL the column is boolean, compared with `cmp` and stored as `val`.
fn mirror(cmp: &str, val: &str) -> (String, String) {
    let a = "CREATE PROCEDURE dbo.p @n int OUTPUT AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @URLs TABLE (ID int, DirectoryPath nvarchar(4000), Mirror bit);\n  INSERT INTO @URLs (ID, DirectoryPath, Mirror) VALUES (1, N'x', 0);\n  SELECT @n = COUNT(*) FROM @URLs WHERE Mirror = 0;\nEND\n".to_string();
    let b = format!("CREATE OR REPLACE PROCEDURE p(INOUT p_n integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  DROP TABLE IF EXISTS pg_temp.tv_p_ur_ls;\n  CREATE TEMP TABLE pg_temp.tv_p_ur_ls (\n    ID integer,\n    DirectoryPath varchar(4000),\n    Mirror boolean\n  );\n  INSERT INTO pg_temp.tv_p_ur_ls (ID, DirectoryPath, Mirror) VALUES (1, 'x', {val});\n  SELECT COUNT(*) INTO p_n FROM pg_temp.tv_p_ur_ls WHERE Mirror {cmp};\nEND;\n$$;\n");
    (a, b)
}

#[test]
fn table_variable_columns_have_their_declared_types() {
    // A bit column turned boolean, still compared with 0: PostgreSQL has no
    // `boolean = integer`. Without the column's type this passed.
    let (a, b) = mirror("= 0", "FALSE");
    let r = run(&a, &b, P);
    assert_eq!(r.verdict(), Verdict::Different, "{}", reasons(&r));
    assert!(
        r.findings.iter().any(|f| f.rule == "equiv.bool-type-error"),
        "{}",
        reasons(&r)
    );
    // And a number stored in it.
    let (a, b) = mirror("= FALSE", "0");
    let r = run(&a, &b, P);
    assert_eq!(r.verdict(), Verdict::Different, "{}", reasons(&r));
    // Written as the converter writes it, the two are equal.
    let (a, b) = mirror("= FALSE", "FALSE");
    let r = run(&a, &b, P);
    assert!(
        r.findings.is_empty() && r.unknown_reasons.is_empty(),
        "{}",
        reasons(&r)
    );
}

#[test]
fn temp_table_columns_have_their_declared_types() {
    // `#t` became `CREATE TEMP TABLE t`: the bare name is the temporary
    // table (PostgreSQL looks in pg_temp first) and its columns are typed.
    let a = "CREATE PROCEDURE dbo.p @n int OUTPUT AS\nBEGIN\n  SET XACT_ABORT ON;\n  CREATE TABLE #t (Name nvarchar(100), Flag bit);\n  INSERT INTO #t (Name, Flag) VALUES (N'a', 1);\n  SELECT @n = COUNT(*) FROM #t WHERE Flag = 1;\nEND\n";
    let b = |cmp: &str| {
        format!("CREATE OR REPLACE PROCEDURE p(INOUT p_n integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  CREATE TEMP TABLE t (\n    Name varchar(100),\n    Flag boolean\n  );\n  INSERT INTO t (Name, Flag) VALUES ('a', TRUE);\n  SELECT COUNT(*) INTO p_n FROM t WHERE Flag {cmp};\nEND;\n$$;\n")
    };
    let r = run(a, &b("= 1"), P);
    assert_eq!(r.verdict(), Verdict::Different, "{}", reasons(&r));
    assert!(
        r.findings.iter().any(|f| f.rule == "equiv.bool-type-error"),
        "{}",
        reasons(&r)
    );
    let r = run(a, &b("= TRUE"), P);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.rule == "write.unpaired" || f.rule == "equiv.bool-type-error"),
        "{}",
        reasons(&r)
    );
    // A permanent `t` written before the temporary one exists is still a
    // durable write that must pair.
    let b2 = "CREATE OR REPLACE PROCEDURE p(INOUT p_n integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  INSERT INTO t (Name, Flag) VALUES ('a', TRUE);\n  CREATE TEMP TABLE t (\n    Name varchar(100),\n    Flag boolean\n  );\n  INSERT INTO t (Name, Flag) VALUES ('a', TRUE);\n  SELECT COUNT(*) INTO p_n FROM t WHERE Flag = TRUE;\nEND;\n$$;\n";
    let r = run(a, b2, P);
    assert!(
        r.findings.iter().any(|f| f.rule == "write.unpaired"),
        "{}",
        reasons(&r)
    );
}
