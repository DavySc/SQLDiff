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
