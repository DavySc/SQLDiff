//! Behavioural findings from the semantic IR on the corpus mutations that
//! need it (design §7, §12): each must be reported for the right reason.

use sqldiff_core::{diff, Artifact, ChangeClass, Dialect, Options, Report, Verdict};

fn mutation(case: &str, d: Dialect) -> Report {
    let dir = format!(
        "{}/../../corpus/mutations/{case}",
        env!("CARGO_MANIFEST_DIR")
    );
    let ext = d.id();
    let read = |side: &str| std::fs::read_to_string(format!("{dir}/{side}.{ext}.sql")).unwrap();
    diff(
        &Artifact::new("original.sql", d, read("original")),
        &Artifact::new("mutated.sql", d, read("mutated")),
        &Options::default(),
    )
}

fn finding<'r>(r: &'r Report, rule: &str) -> &'r sqldiff_core::Finding {
    r.findings
        .iter()
        .find(|f| f.rule == rule)
        .unwrap_or_else(|| panic!("no {rule}: {:#?}", r.findings))
}

#[test]
fn m02_commit_moved_before_last_dml() {
    let r = mutation("m02-commit-before-last-dml", Dialect::Tsql);
    let f = finding(&r, "txn.atomicity");
    assert_eq!(f.verdict, Verdict::Different);
    assert_eq!(f.class, ChangeClass::Transactional);
    assert!(
        f.witness.contains("INSERT into stock_log fails"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side A (T-SQL) undoes the UPDATE of stock"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side B (T-SQL) keeps the UPDATE of stock"),
        "{}",
        f.witness
    );
}

#[test]
fn m05_table_variable_replaced_by_temp_table() {
    let r = mutation("m05-table-variable-to-temp-table", Dialect::Tsql);
    let f = finding(&r, "txn.temp-state");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("reads the temporary table attempted"),
        "{}",
        f.witness
    );
    assert!(f.witness.contains("still holds the rows"), "{}", f.witness);
}

#[test]
fn m06_continue_handler_removed() {
    let r = mutation("m06-continue-handler-removed", Dialect::Db2i);
    assert_eq!(r.verdict, Verdict::Different);
    let f = finding(&r, "error.contract");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("side A (Db2 for i) returns normally"),
        "{}",
        f.witness
    );
    assert!(f.witness.contains("propagates"), "{}", f.witness);
    let c = finding(&r, "error.control");
    assert!(
        c.witness
            .contains("does not run the INSERT into audit_event"),
        "{}",
        c.witness
    );
}

#[test]
fn m07_exit_handler_turned_into_continue() {
    let r = mutation("m07-exit-to-continue", Dialect::Db2i);
    let f = finding(&r, "error.control");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("If the INSERT into order_line fails"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side A (Db2 for i) does not run the UPDATE of stock"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side B (Db2 for i) keeps the UPDATE of stock"),
        "{}",
        f.witness
    );
}

#[test]
fn m08_atomic_compound_dropped() {
    let r = mutation("m08-atomic-dropped", Dialect::Db2i);
    let f = finding(&r, "txn.atomicity");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("If the second UPDATE of stock fails"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side A (Db2 for i) undoes the first UPDATE of stock"),
        "{}",
        f.witness
    );
    assert!(
        f.witness.contains("for the caller to commit or roll back"),
        "{}",
        f.witness
    );
    assert!(f
        .assumptions
        .contains(&"ASM-CALLER-MAY-COMMIT-AFTER-ERROR".to_string()));
}

#[test]
fn m09_commit_level_none() {
    let r = mutation("m09-commit-level-none", Dialect::Db2i);
    let f = finding(&r, "txn.atomicity");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("If the UPDATE of stock fails"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side A (Db2 for i) undoes the INSERT into order_line"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side B (Db2 for i) keeps the INSERT into order_line"),
        "{}",
        f.witness
    );
    finding(&r, "txn.statement-atomicity");
}

#[test]
fn identical_routines_have_no_behavioural_findings() {
    for (case, d) in [
        ("m07-exit-to-continue", Dialect::Db2i),
        ("m05-table-variable-to-temp-table", Dialect::Tsql),
        ("m02-commit-before-last-dml", Dialect::Tsql),
    ] {
        let dir = format!(
            "{}/../../corpus/mutations/{case}",
            env!("CARGO_MANIFEST_DIR")
        );
        let t = std::fs::read_to_string(format!("{dir}/original.{}.sql", d.id())).unwrap();
        let r = diff(
            &Artifact::new("a.sql", d, t.clone()),
            &Artifact::new("b.sql", d, t),
            &Options::default(),
        );
        assert_eq!(r.verdict, Verdict::Equal, "{case}: {:#?}", r.findings);
    }
}

#[test]
fn caller_transaction_and_error_contract_on_the_worked_example() {
    let fx = |n: &str| {
        std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/{n}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, fx("movestock.tsql.sql")),
        &Artifact::new("b.sql", Dialect::Db2i, fx("movestock.db2i.sql")),
        &Options::default(),
    );
    let c = finding(&r, "txn.caller-transaction");
    // Equal under the default assumption that there is no caller transaction.
    assert_eq!(c.verdict, Verdict::EqualUnderAssumptions);
    assert!(c.assumptions.contains(&"ASM-NO-CALLER-TXN".to_string()));
    assert!(
        c.witness
            .contains("caller already holds an open transaction"),
        "{}",
        c.witness
    );
    assert!(
        c.witness.contains("for the caller to commit or roll back"),
        "{}",
        c.witness
    );
    let e = finding(&r, "error.contract");
    assert!(e.witness.contains("returns code 1"), "{}", e.witness);
    assert!(e.assumptions.contains(&"ASM-CALLER-CHECKS-RC".to_string()));
}

fn pair(a: (&str, Dialect), b: (&str, Dialect)) -> Report {
    diff(
        &Artifact::new("a.sql", a.1, a.0.to_string()),
        &Artifact::new("b.sql", b.1, b.0.to_string()),
        &Options::default(),
    )
}

#[test]
fn caller_work_committed_by_one_side_only() {
    // T-SQL COMMIT inside the caller's transaction only decrements
    // @@TRANCOUNT; Db2 for i COMMIT commits the caller's work too.
    let r = pair(
        (
            "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRAN;\n  INSERT INTO t (c) VALUES (1);\n  COMMIT;\nEND",
            Dialect::Tsql,
        ),
        (
            "CREATE PROCEDURE p ()\nLANGUAGE SQL\nSET OPTION COMMIT = *CHG\nBEGIN\n  INSERT INTO t (c) VALUES (1);\n  COMMIT;\nEND",
            Dialect::Db2i,
        ),
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.caller-transaction" && f.witness.contains("caller's earlier work"))
        .unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert!(
        f.witness
            .contains("side A (T-SQL) leaves the caller's earlier work to the caller"),
        "{}",
        f.witness
    );
    assert!(
        f.witness
            .contains("side B (Db2 for i) commits the caller's earlier work"),
        "{}",
        f.witness
    );
}

#[test]
fn error_contract_keeps_the_proven_difference() {
    // Failing t1 is unknown (side B's handler branches on @x); failing t2
    // is a proven difference (code 1 against an error). The pair keeps one
    // error.contract finding, the proven one.
    let r = pair(
        (
            "CREATE PROCEDURE p @x int AS\nBEGIN\n  BEGIN TRY\n    INSERT INTO t1 (c) VALUES (1);\n    INSERT INTO t2 (c) VALUES (1);\n  END TRY\n  BEGIN CATCH\n    RETURN 1;\n  END CATCH\nEND",
            Dialect::Tsql,
        ),
        (
            "CREATE PROCEDURE p @x int AS\nBEGIN\n  BEGIN TRY\n    INSERT INTO t1 (c) VALUES (1);\n  END TRY\n  BEGIN CATCH\n    IF @x = 1 RETURN 1;\n    THROW;\n  END CATCH\n  INSERT INTO t2 (c) VALUES (1);\nEND",
            Dialect::Tsql,
        ),
    );
    let contracts: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "error.contract")
        .collect();
    assert_eq!(contracts.len(), 1, "{:#?}", r.findings);
    assert_eq!(
        contracts[0].verdict,
        Verdict::Different,
        "{:#?}",
        contracts[0]
    );
    assert!(
        contracts[0].witness.contains("t2"),
        "{}",
        contracts[0].witness
    );
}

#[test]
fn temp_state_read_without_a_durable_write() {
    // The CATCH block counts the rows of the temporary table after ROLLBACK:
    // a table variable keeps them, a #temp table loses them.
    let body = |decl: &str, t: &str| {
        format!(
            "CREATE PROCEDURE p AS\nBEGIN\n  {decl};\n  DECLARE @n int;\n  BEGIN TRY\n    BEGIN TRAN;\n    INSERT INTO {t} (id) VALUES (1);\n    INSERT INTO t (c) VALUES (1);\n    COMMIT;\n  END TRY\n  BEGIN CATCH\n    ROLLBACK;\n    SELECT @n = COUNT(*) FROM {t};\n  END CATCH\nEND"
        )
    };
    let a = body("DECLARE @log TABLE (id int)", "@log");
    let b = body("CREATE TABLE #log (id int)", "#log");
    let r = pair((&a, Dialect::Tsql), (&b, Dialect::Tsql));
    let f = finding(&r, "txn.temp-state");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(f.witness.contains("the read of log"), "{}", f.witness);
    assert!(f.a.is_some() && f.b.is_some(), "{f:#?}");
}

#[test]
fn temp_state_on_two_tables_counts_once_per_path() {
    // The CATCH INSERT reads two temporary tables. Side B refills both on
    // one of its two CATCH paths only, so the read differs on some paths:
    // one finding, Unknown, not Different.
    let a = "CREATE PROCEDURE p @x int AS\nBEGIN\n  DECLARE @a TABLE (id int);\n  DECLARE @b TABLE (id int);\n  DECLARE @n int;\n  BEGIN TRY\n    BEGIN TRAN;\n    INSERT INTO @a (id) VALUES (1);\n    INSERT INTO @b (id) VALUES (1);\n    INSERT INTO t (c) VALUES (1);\n    COMMIT;\n  END TRY\n  BEGIN CATCH\n    ROLLBACK;\n    INSERT INTO audit (c) SELECT x.id FROM @a x JOIN @b y ON x.id = y.id;\n  END CATCH\nEND";
    let b = "CREATE PROCEDURE p @x int AS\nBEGIN\n  CREATE TABLE #a (id int);\n  CREATE TABLE #b (id int);\n  DECLARE @n int;\n  BEGIN TRY\n    BEGIN TRAN;\n    INSERT INTO #a (id) VALUES (1);\n    INSERT INTO #b (id) VALUES (1);\n    INSERT INTO t (c) VALUES (1);\n    COMMIT;\n  END TRY\n  BEGIN CATCH\n    ROLLBACK;\n    IF @x = 1\n    BEGIN\n      INSERT INTO #a (id) VALUES (1);\n      INSERT INTO #b (id) VALUES (1);\n    END\n    INSERT INTO audit (c) SELECT x.id FROM #a x JOIN #b y ON x.id = y.id;\n  END CATCH\nEND";
    let r = pair((a, Dialect::Tsql), (b, Dialect::Tsql));
    let ts: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "txn.temp-state")
        .collect();
    assert_eq!(ts.len(), 1, "{:#?}", r.findings);
    assert_eq!(ts[0].verdict, Verdict::Unknown, "{:#?}", ts[0]);
}
