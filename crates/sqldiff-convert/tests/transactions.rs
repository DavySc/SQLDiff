//! T-SQL transaction control → PostgreSQL. A `BEGIN TRAN … COMMIT` that
//! wraps all the durable work of a procedure or script batch is dropped: the
//! CALL or DO is one transaction already, and inside a caller's transaction
//! both sides leave the work to the caller. Other COMMITs stay, under
//! `ASM-NO-CALLER-TXN` (callers call in autocommit). Behaviour checked on
//! SQL Server 2022 and PostgreSQL 18 (unconverted-burndown/transactions.md):
//! an inner COMMIT in T-SQL only decrements @@TRANCOUNT; PostgreSQL refuses a
//! COMMIT in a procedure CALLed inside a transaction block; an error in a
//! CALL without transaction control rolls back the whole CALL.

use std::collections::BTreeMap;

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options, Verdict};

const TABLES: &str = "CREATE TABLE dbo.A (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
CREATE TABLE dbo.B (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
";

/// `src` converted to PostgreSQL with the tables in both catalogs; `caller`
/// runs the check as if every caller held an open transaction
/// (`ASM-NO-CALLER-TXN` denied).
fn pg_in(src: &str, caller: bool) -> Conversion {
    let mut cat = Catalog::default();
    for f in [TABLES, src] {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let target = rewrite_in(TABLES, Dialect::Tsql, Dialect::Postgres, None)
        .output()
        .to_string();
    let mut flips = BTreeMap::new();
    if caller {
        flips.insert("ASM-NO-CALLER-TXN".to_string(), false);
    }
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", Dialect::Postgres, &target)],
        flips,
        ..Options::default()
    };
    check(
        src,
        rewrite_in(src, Dialect::Tsql, Dialect::Postgres, Some(&cat)),
        &opts,
    )
}

fn pg(src: &str) -> Conversion {
    pg_in(src, false)
}

/// Lines of the output that run COMMIT (not the comment left in its place).
fn commits(c: &Conversion) -> usize {
    c.output
        .lines()
        .filter(|l| l.trim_start().starts_with("COMMIT"))
        .count()
}

fn decisions(c: &Conversion) -> Vec<String> {
    c.units
        .iter()
        .flat_map(|u| &u.notes)
        .filter(|n| n.status == Status::Decision)
        .map(|n| n.rule.clone())
        .collect()
}

fn txn_differences(c: &Conversion) -> Vec<String> {
    c.report
        .findings
        .iter()
        .filter(|f| f.rule.starts_with("txn.") && f.verdict == Verdict::Different)
        .map(|f| format!("{}: {}", f.rule, f.witness))
        .collect()
}

fn has_note(c: &Conversion, rule: &str) -> bool {
    c.units
        .iter()
        .flat_map(|u| &u.notes)
        .any(|n| n.rule == rule)
}

const WRAPPED: &str = "CREATE PROCEDURE dbo.Move @Id int AS
BEGIN
    SET NOCOUNT ON;
    SET XACT_ABORT ON;
    DECLARE @n int = 1;
    BEGIN TRAN;
    UPDATE dbo.A SET Qty = Qty - @n WHERE Id = @Id;
    UPDATE dbo.B SET Qty = Qty + @n WHERE Id = @Id;
    COMMIT;
END;
";

#[test]
fn a_wrapper_around_all_the_work_is_dropped() {
    let c = pg(WRAPPED);
    assert_eq!(commits(&c), 0, "{}", c.output);
    assert!(c
        .output
        .contains("-- BEGIN TRANSACTION: the call is one transaction"));
    assert!(decisions(&c).is_empty(), "{:?}", decisions(&c));
    assert!(!has_note(&c, "pg.proc-transaction"));
    assert!(
        c.units
            .iter()
            .flat_map(|u| &u.trace)
            .filter(|s| s.rule == "tsql.txn.wrapper")
            .count()
            == 2
    );
}

#[test]
fn the_dropped_wrapper_is_equal_with_and_without_a_caller_transaction() {
    for caller in [false, true] {
        let c = pg_in(WRAPPED, caller);
        assert!(
            txn_differences(&c).is_empty(),
            "caller transaction {caller}: {:?}\n{}",
            txn_differences(&c),
            c.output
        );
        assert_ne!(c.report.verdict(), Verdict::Different, "{caller}");
    }
}

const BATCH: &str = "BEGIN TRANSACTION

DECLARE @Qty int = 5

INSERT dbo.A (Id, Qty) VALUES (1, @Qty), (2, @Qty)
COMMIT
GO
";

#[test]
fn a_script_batch_with_one_insert_drops_its_wrapper() {
    // WWI pds150: no XACT_ABORT, but one statement fails as a whole.
    let c = pg(BATCH);
    assert!(c.output.starts_with("DO $$"), "{}", c.output);
    assert_eq!(commits(&c), 0, "{}", c.output);
    assert!(decisions(&c).is_empty());
    // T-SQL carries on after the failed INSERT and runs the COMMIT,
    // PostgreSQL stops at the error. That difference is the batch's own: the
    // conversion that kept the COMMIT (and needed a decision) had the same
    // one. Nothing about durability differs.
    let rules: Vec<String> = txn_differences(&c)
        .into_iter()
        .map(|d| d.split(':').next().unwrap().to_string())
        .collect();
    assert_eq!(rules, ["txn.caller-continuation"], "{}", c.output);
}

#[test]
fn two_writes_without_xact_abort_keep_the_commit() {
    // A failing first UPDATE ends only itself in T-SQL: COMMIT keeps the
    // second. The wrapper is not dropped.
    let c = pg("CREATE PROCEDURE dbo.Move @Id int AS
BEGIN
    BEGIN TRAN;
    UPDATE dbo.A SET Qty = Qty - 1 WHERE Id = @Id;
    UPDATE dbo.B SET Qty = Qty + 1 WHERE Id = @Id;
    COMMIT;
END;
");
    assert_eq!(commits(&c), 1, "{}", c.output);
    assert!(has_note(&c, "ASM-NO-CALLER-TXN"));
    assert!(decisions(&c).is_empty());
}

const OUTSIDE: &str = "CREATE PROCEDURE dbo.Move @Id int AS
BEGIN
    SET XACT_ABORT ON;
    UPDATE dbo.A SET Qty = 0 WHERE Id = @Id;
    BEGIN TRAN;
    UPDATE dbo.B SET Qty = Qty + 1 WHERE Id = @Id;
    UPDATE dbo.A SET Qty = Qty + 1 WHERE Id = @Id;
    COMMIT;
END;
";

#[test]
fn work_outside_the_pair_commits_on_its_own() {
    // T-SQL commits the first UPDATE on its own (no transaction is open):
    // a COMMIT follows it, so a failure in the transaction leaves it.
    let c = pg(OUTSIDE);
    assert_eq!(commits(&c), 2, "{}", c.output);
    assert!(
        c.output
            .contains("WHERE Id = p_id;\n  COMMIT;\n  -- BEGIN TRANSACTION"),
        "{}",
        c.output
    );
    assert!(txn_differences(&c).is_empty(), "{:?}", txn_differences(&c));
    assert_eq!(
        c.report.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        c.report
    );
    assert!(
        c.units.iter().all(|u| u.status == Status::Assumptions),
        "{:#?}",
        c.units
    );
    // Inside a caller's transaction they differ: what ASM-NO-CALLER-TXN covers.
    assert!(!txn_differences(&pg_in(OUTSIDE, true)).is_empty());
}

#[test]
fn a_return_before_the_commit_keeps_it() {
    // T-SQL returns with the transaction still open (Msg 266).
    let c = pg("CREATE PROCEDURE dbo.Move @Id int AS
BEGIN
    SET XACT_ABORT ON;
    BEGIN TRAN;
    UPDATE dbo.A SET Qty = 0 WHERE Id = @Id;
    IF @Id < 0 RETURN;
    UPDATE dbo.B SET Qty = Qty + 1 WHERE Id = @Id;
    COMMIT;
END;
");
    assert_eq!(commits(&c), 1, "{}", c.output);
}

#[test]
fn calls_inside_the_pair_keep_it() {
    // The callee may control transactions itself.
    let c = pg("CREATE PROCEDURE dbo.Move @Id int AS
BEGIN
    SET XACT_ABORT ON;
    BEGIN TRAN;
    UPDATE dbo.A SET Qty = 0 WHERE Id = @Id;
    EXEC dbo.Other @Id;
    COMMIT;
END;
");
    assert_eq!(commits(&c), 1, "{}", c.output);
}

const TWO_TRANSACTIONS: &str = "CREATE PROCEDURE dbo.Load AS
BEGIN
    SET XACT_ABORT ON;
    BEGIN TRAN;
    INSERT dbo.A (Id, Qty) VALUES (1, 1);
    COMMIT;
    BEGIN TRAN;
    INSERT dbo.B (Id, Qty) VALUES (1, 1);
    COMMIT;
END;
";

#[test]
fn several_transactions_keep_their_commits_under_the_autocommit_assumption() {
    let c = pg(TWO_TRANSACTIONS);
    assert_eq!(commits(&c), 2, "{}", c.output);
    assert!(decisions(&c).is_empty(), "{:?}", decisions(&c));
    for u in c.units.iter().filter(|u| {
        u.trace
            .iter()
            .any(|s| s.rule == "tsql.txn.begin" || s.rule == "tsql.txn.commit")
    }) {
        assert!(
            u.notes
                .iter()
                .any(|n| n.rule == "ASM-NO-CALLER-TXN" && n.status == Status::Assumptions),
            "{u:?}"
        );
    }
    // In autocommit both sides keep the first INSERT when the second fails.
    assert!(txn_differences(&c).is_empty(), "{:?}", txn_differences(&c));
}

#[test]
fn several_transactions_differ_when_callers_hold_a_transaction() {
    // What the assumption covers: inside a caller's transaction T-SQL's
    // COMMITs only decrement @@TRANCOUNT, PostgreSQL's end the caller's
    // transaction (or are refused).
    let c = pg_in(TWO_TRANSACTIONS, true);
    assert!(!txn_differences(&c).is_empty(), "{}", c.output);
}

#[test]
fn a_return_code_after_the_commit_still_drops_it() {
    // WWI DW MigrateStaged*: `COMMIT; RETURN 0;`.
    let c = pg(&WRAPPED.replace("    COMMIT;\n", "    COMMIT;\n    RETURN 0;\n"));
    assert_eq!(commits(&c), 0, "{}", c.output);
    assert!(txn_differences(&c).is_empty(), "{:?}", txn_differences(&c));
}

#[test]
fn a_statement_after_the_commit_keeps_it() {
    // If it failed, T-SQL's work would stay committed.
    let c = pg(&WRAPPED.replace(
        "    COMMIT;\n",
        "    COMMIT;\n    SET @n = (SELECT Qty FROM dbo.A WHERE Id = @Id);\n",
    ));
    assert_eq!(commits(&c), 1, "{}", c.output);
}

const NO_TRAN: &str = "CREATE PROCEDURE dbo.Load @N int AS
BEGIN
    SET XACT_ABORT ON;
    DECLARE @i int = 0;
    INSERT dbo.A (Id, Qty) VALUES (@N, 0);
    WHILE @i < @N
    BEGIN
        SET @i = @i + 1;
        UPDATE dbo.A SET Qty = Qty + 1 WHERE Id = @N;
    END;
    IF @N > 10
        DELETE FROM dbo.B WHERE Id = @N;
END;
";

#[test]
fn statements_without_a_transaction_commit_one_by_one() {
    // SQL Server 2022 commits each statement on its own; PostgreSQL 18
    // allows COMMIT inside an IF of a CALLed procedure.
    let src = NO_TRAN.replace(
        "    WHILE @i < @N\n    BEGIN\n        SET @i = @i + 1;\n        UPDATE",
        "    BEGIN\n        SET @i = @i + 1;\n        UPDATE",
    );
    let c = pg(&src);
    assert_eq!(commits(&c), 3, "{}", c.output);
    assert!(has_note(&c, "ASM-NO-CALLER-TXN"), "{}", c.output);
    assert!(txn_differences(&c).is_empty(), "{:?}", txn_differences(&c));
    assert_eq!(
        c.report.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        c.report
    );
    assert!(
        c.units.iter().all(|u| u.status == Status::Assumptions),
        "{:#?}",
        c.units
    );
    assert!(!txn_differences(&pg_in(&src, true)).is_empty());
}

#[test]
fn a_commit_inside_a_loop_is_emitted_but_not_proven() {
    // PostgreSQL 18 allows COMMIT inside a WHILE of a CALLed procedure, but
    // the behaviour model summarizes loops after two iterations: the
    // statements stay unproven.
    let c = pg(NO_TRAN);
    assert_eq!(commits(&c), 3, "{}", c.output);
    assert!(
        c.output
            .contains("Qty + 1 WHERE Id = p_n;\n    COMMIT;\n  END LOOP;"),
        "{}",
        c.output
    );
    assert_eq!(c.report.verdict(), Verdict::Unknown, "{:?}", c.report);
    assert!(
        c.units.iter().any(|u| u.status == Status::Unproven),
        "{:#?}",
        c.units
    );
}

#[test]
fn without_xact_abort_on_nothing_is_emulated() {
    // An error leaves the batch running or not by error class: not exact.
    let c = pg(&NO_TRAN.replace("    SET XACT_ABORT ON;\n", ""));
    assert_eq!(commits(&c), 0, "{}", c.output);
    let c = pg(&NO_TRAN.replace("XACT_ABORT ON", "XACT_ABORT OFF"));
    assert_eq!(commits(&c), 0, "{}", c.output);
}

#[test]
fn a_single_write_needs_no_commit() {
    // A statement is atomic on both sides: one write is one transaction.
    let c = pg("CREATE PROCEDURE dbo.Zero @Id int AS
BEGIN
    SET XACT_ABORT ON;
    UPDATE dbo.A SET Qty = 0 WHERE Id = @Id;
END;
");
    assert_eq!(commits(&c), 0, "{}", c.output);
    assert_eq!(commits(&c), 0, "{}", c.output);
}

#[test]
fn try_catch_or_rollback_is_not_emulated() {
    for src in [
        NO_TRAN.replace(
            "    IF @N > 10",
            "    BEGIN TRY\n        DELETE FROM dbo.B WHERE Id = 0;\n    END TRY\n    BEGIN CATCH\n        PRINT 'x';\n    END CATCH;\n    IF @N > 10",
        ),
        NO_TRAN.replace(
            "        DELETE FROM dbo.B WHERE Id = @N;",
            "        BEGIN BEGIN TRAN; DELETE FROM dbo.B WHERE Id = @N; ROLLBACK; END;",
        ),
    ] {
        let c = pg(&src);
        assert_eq!(commits(&c), 0, "{}", c.output);
    }
}

#[test]
fn an_owner_procedure_is_not_emulated() {
    // PostgreSQL 18 refuses COMMIT in a SECURITY DEFINER procedure.
    let c = pg(&NO_TRAN.replace(" AS\nBEGIN", "\nWITH EXECUTE AS OWNER\nAS\nBEGIN"));
    assert!(c.output.contains("SECURITY DEFINER"), "{}", c.output);
    assert_eq!(commits(&c), 0, "{}", c.output);
}
