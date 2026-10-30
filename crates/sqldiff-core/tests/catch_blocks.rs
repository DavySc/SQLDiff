//! T-SQL TRY/CATCH against a PL/pgSQL block with `EXCEPTION WHEN OTHERS`:
//! the CATCH body is compared as the branch `try … catch … end`, and only
//! where the state the error leaves cannot show (SQL Server 2022 and
//! PostgreSQL 18 behaviour checked: a failing T-SQL SELECT or UPDATE may
//! have assigned some variables, PostgreSQL undoes the block's writes, and
//! SQL Server dooms an open transaction on a conversion error even with
//! XACT_ABORT OFF).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str, d: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", d, b),
        &Options::default(),
    )
}

fn tsql(try_part: &str, catch: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n  DECLARE @e int;\n  DECLARE @m nvarchar(100);\n  BEGIN TRY\n{try_part}\n  END TRY\n  BEGIN CATCH\n{catch}\n  END CATCH;\nEND;\n"
    )
}

fn pg(try_part: &str, handler: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_e integer;\n  v_m varchar(100);\nBEGIN\n  BEGIN\n{try_part}\n  EXCEPTION WHEN OTHERS THEN\n{handler}\n  END;\nEND;\n$$;\n"
    )
}

fn not_covered(r: &Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| {
            x.contains("not covered") || x.contains("not paired") || x.contains("no counterpart")
        })
        .collect()
}

fn equal(r: &Report) -> bool {
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn print_and_rethrow_is_raise_notice_and_raise() {
    let r = run(
        &tsql("    DELETE FROM dbo.a;", "    PRINT N'failed';\n    THROW;"),
        &pg(
            "    DELETE FROM a;",
            "    RAISE NOTICE '%', 'failed';\n    RAISE;",
        ),
        Dialect::Postgres,
    );
    assert!(equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
    assert!(not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
}

#[test]
fn a_handler_that_does_not_rethrow_differs() {
    let r = run(
        &tsql("    DELETE FROM dbo.a;", "    PRINT N'failed';\n    THROW;"),
        &pg("    DELETE FROM a;", "    RAISE NOTICE '%', 'failed';"),
        Dialect::Postgres,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

#[test]
fn a_different_message_or_value_in_the_catch_is_not_equal() {
    for (c, h) in [
        (
            "    PRINT N'failed';\n    THROW;",
            "    RAISE NOTICE '%', 'failure';\n    RAISE;",
        ),
        ("    SET @e = 2;", "    v_e := 3;"),
        (
            "    SET @e = 2;\n    SET @m = N'x';",
            "    v_m := 'x';\n    v_e := 2;",
        ),
    ] {
        let r = run(
            &tsql("    SET @e = 1;", c),
            &pg("    v_e := 1;", h),
            Dialect::Postgres,
        );
        assert!(!equal(&r), "{c}\n{:#?}", r.findings);
    }
}

#[test]
fn error_number_and_message_are_not_sqlstate_and_sqlerrm() {
    let r = run(
        &tsql(
            "    DELETE FROM dbo.a;",
            "    SET @e = ERROR_NUMBER();\n    SET @m = ERROR_MESSAGE();",
        ),
        &pg(
            "    DELETE FROM a;",
            "    v_e := SQLSTATE;\n    v_m := SQLERRM;",
        ),
        Dialect::Postgres,
    );
    assert!(!equal(&r), "{:#?}", r.findings);
    let unproven = r
        .findings
        .iter()
        .filter(|f| f.rule == "equiv.unproven")
        .count();
    assert_eq!(unproven, 2, "{:#?}", r.findings);
}

#[test]
fn a_catch_inside_an_if_is_compared() {
    let a = "CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n  DECLARE @e int;\n  IF @x = 1\n  BEGIN\n    BEGIN TRY\n      SET @e = 1;\n    END TRY\n    BEGIN CATCH\n      SET @e = 2;\n    END CATCH\n  END\nEND;\n";
    let b = "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_e integer;\nBEGIN\n  IF p_x = 1 THEN\n    BEGIN\n      v_e := 1;\n    EXCEPTION WHEN OTHERS THEN\n      v_e := 2;\n    END;\n  END IF;\nEND;\n$$;\n";
    let r = run(a, b, Dialect::Postgres);
    assert!(equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
    assert!(not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
}

#[test]
fn a_statement_moved_across_the_try_boundary_is_not_paired() {
    let a = "CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n  DECLARE @e int;\n  SET @e = 1;\n  BEGIN TRY\n    SET @e = 2;\n  END TRY\n  BEGIN CATCH\n    SET @e = 3;\n  END CATCH\nEND;\n";
    let b = "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_e integer;\nBEGIN\n  BEGIN\n    v_e := 1;\n    v_e := 2;\n  EXCEPTION WHEN OTHERS THEN\n    v_e := 3;\n  END;\nEND;\n$$;\n";
    let r = run(a, b, Dialect::Postgres);
    assert!(!equal(&r), "{:#?}", r.unknown_reasons);
    assert!(!not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
}

#[test]
fn a_catch_against_plain_statements_is_not_equal() {
    let r = run(
        &tsql("    SET @e = 1;", "    SET @e = 2;"),
        "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_e integer;\n  v_m varchar(100);\nBEGIN\n  v_e := 1;\n  v_e := 2;\nEND;\n$$;\n",
        Dialect::Postgres,
    );
    assert!(!equal(&r), "{:#?}", r.unknown_reasons);
}

#[test]
fn catch_statements_using_tables_after_a_try_that_wrote_before_failing_are_not_compared() {
    // When the second INSERT fails, T-SQL keeps the first and PostgreSQL has
    // undone it: the count in the CATCH may differ.
    let r = run(
        &tsql(
            "    INSERT INTO dbo.a (c) VALUES (1);\n    INSERT INTO dbo.b (c) VALUES (1);",
            "    SELECT @e = COUNT(*) FROM dbo.a;",
        ),
        &pg(
            "    INSERT INTO a (c) VALUES (1);\n    INSERT INTO b (c) VALUES (1);",
            "    SELECT COUNT(*) INTO v_e FROM a;",
        ),
        Dialect::Postgres,
    );
    assert!(!equal(&r));
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 10: a statement inside a CATCH block that reads or changes tables, after a TRY part that may change the database"
        )),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_try_that_only_ends_with_a_write_leaves_the_catch_compared() {
    // The one write either failed (undone on both sides) or never ran.
    let r = run(
        &tsql(
            "    SET @e = 1;\n    INSERT INTO dbo.a (c) VALUES (@e);",
            "    SELECT @e = COUNT(*) FROM dbo.a;",
        ),
        &pg(
            "    v_e := 1;\n    INSERT INTO a (c) VALUES (v_e);",
            "    SELECT COUNT(*) INTO v_e FROM a;",
        ),
        Dialect::Postgres,
    );
    assert!(
        !r.unknown_reasons
            .iter()
            .any(|x| x.contains("inside a CATCH block")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_variable_a_failing_select_may_have_set_in_part_is_not_read_in_the_catch() {
    // SQL Server keeps the value of the rows before the failing one.
    let r = run(
        &tsql("    SELECT @e = c FROM dbo.a;", "    SET @x = @e;"),
        &pg("    SELECT c INTO v_e FROM a;", "    p_x := v_e;"),
        Dialect::Postgres,
    );
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 9: a statement inside a CATCH block that reads `@e`, which a failing statement of the TRY part may have assigned in part"
        )),
        "{:#?}",
        r.unknown_reasons
    );
    // A SET assigns nothing when it fails.
    let r = run(
        &tsql("    SET @e = 1 / @x;", "    SET @x = @e;"),
        &pg("    v_e := 1 / p_x;", "    p_x := v_e;"),
        Dialect::Postgres,
    );
    assert!(
        !r.unknown_reasons
            .iter()
            .any(|x| x.contains("inside a CATCH block")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_catch_that_writes_assumes_no_caller_transaction() {
    let r = run(
        &tsql(
            "    INSERT INTO dbo.a (c) VALUES (1);",
            "    INSERT INTO dbo.log (c) VALUES (1);",
        ),
        &pg(
            "    INSERT INTO a (c) VALUES (1);",
            "    INSERT INTO log (c) VALUES (1);",
        ),
        Dialect::Postgres,
    );
    assert!(equal(&r), "{:#?}\n{:#?}", r.findings, r.unknown_reasons);
    assert!(
        r.assumptions.iter().any(|x| x.id == "ASM-NO-CALLER-TXN"),
        "{:#?}",
        r.assumptions
    );
}

#[test]
fn a_catch_that_writes_after_the_routine_began_a_transaction_is_not_compared() {
    // SQL Server dooms the transaction on a conversion error: the CATCH's
    // INSERT fails (Msg 3930) where PostgreSQL's succeeds.
    let a = "CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n  BEGIN TRAN;\n  BEGIN TRY\n    INSERT INTO dbo.a (c) VALUES (1);\n  END TRY\n  BEGIN CATCH\n    INSERT INTO dbo.log (c) VALUES (1);\n  END CATCH\n  COMMIT;\nEND;\n";
    let b = "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  BEGIN\n    INSERT INTO a (c) VALUES (1);\n  EXCEPTION WHEN OTHERS THEN\n    INSERT INTO log (c) VALUES (1);\n  END;\n  COMMIT;\nEND;\n$$;\n";
    let r = run(a, b, Dialect::Postgres);
    assert!(!equal(&r));
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 8: a statement inside a CATCH block that changes the database while a transaction may be open"
        )),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_handler_for_one_condition_stays_not_covered() {
    let r = run(
        &tsql("    SET @e = 1 / @x;", "    SET @e = 0;"),
        "CREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_e integer;\n  v_m varchar(100);\nBEGIN\n  BEGIN\n    v_e := 1 / p_x;\n  EXCEPTION WHEN division_by_zero THEN\n    v_e := 0;\n  END;\nEND;\n$$;\n",
        Dialect::Postgres,
    );
    assert!(!equal(&r));
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("an EXCEPTION section is not covered")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn against_db2_for_i_a_catch_stays_not_covered() {
    let a = tsql("    SET @e = 1;", "    SET @e = 2;");
    let b = "CREATE PROCEDURE p (IN p_x INTEGER)\nLANGUAGE SQL\nBEGIN\n  DECLARE v_e INTEGER;\n  DECLARE v_m VARCHAR(100);\n  BEGIN\n    DECLARE EXIT HANDLER FOR SQLEXCEPTION\n    BEGIN\n      SET v_e = 2;\n    END;\n    SET v_e = 1;\n  END;\nEND\n";
    let r = run(&a, b, Dialect::Db2i);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("a CATCH block is not covered")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn what_the_error_left_is_not_read_in_the_catch() {
    let r = run(
        &tsql("    SET @e = 1 / @x;", "    SET @e = @@ROWCOUNT;"),
        &pg(
            "    v_e := 1 / p_x;",
            "    GET DIAGNOSTICS v_e = ROW_COUNT;",
        ),
        Dialect::Postgres,
    );
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 9: a statement inside a CATCH block that reads `@@ROWCOUNT`, which the error leaves differently"
        )),
        "{:#?}",
        r.unknown_reasons
    );
}
