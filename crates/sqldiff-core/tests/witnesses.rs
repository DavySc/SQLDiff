//! Milestones M5a/M5b (design §14): the worked example of §7 and the
//! behavioural corpus cases yield the right witnesses and verdicts, with
//! structured outcomes for both sides; flipping a scenario assumption
//! re-runs the analysis; `what_if`, the transaction overlay and the ledger
//! answer from the same model. The T-SQL ↔ PostgreSQL divergences pinned
//! here are the ones the fault-injection oracle reproduces on SQL Server
//! 2022 and PostgreSQL 18 (`corpus/tools/fault_oracle.py`).

use sqldiff_core::ir::witness::{ScenarioKind, Witness};
use sqldiff_core::ir::ErrorKind;
use sqldiff_core::ledger::{self, Status};
use sqldiff_core::model::Side;
use sqldiff_core::{
    diff, overlay, what_if, Artifact, ChangeClass, Dialect, Finding, Options, Report, Verdict,
};

fn corpus(path: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../corpus/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn side(case: &str, file: &str, d: Dialect) -> Artifact {
    Artifact::new(file, d, corpus(&format!("{case}/{file}")))
}

fn p01(b: &str, d: Dialect) -> (Artifact, Artifact) {
    (
        side("pairs/p01-move-stock", "tsql.sql", Dialect::Tsql),
        side("pairs/p01-move-stock", b, d),
    )
}

fn finding<'r>(r: &'r Report, rule: &str) -> &'r Finding {
    r.findings
        .iter()
        .find(|f| f.rule == rule)
        .unwrap_or_else(|| panic!("no {rule}: {:#?}", r.findings))
}

fn witness<'r>(r: &'r Report, f: &Finding) -> &'r Witness {
    r.witnesses
        .iter()
        .find(|w| w.fingerprint == f.fingerprint)
        .unwrap_or_else(|| panic!("no structured witness for {}", f.rule))
}

fn flipped(id: &str, holds: bool) -> Options {
    let mut o = Options::default();
    o.flips.insert(id.into(), holds);
    o
}

#[test]
fn worked_example_tsql_db2i() {
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let r = diff(&a, &b, &Options::default());
    assert_eq!(r.verdict, Verdict::Different);

    // 1. If the INSERT fails, T-SQL rolls back the UPDATE and returns 2; Db2
    //    exits with the UPDATE already durable.
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.atomicity" && f.witness.contains("INSERT into stock_log fails"))
        .expect("witness 1");
    assert_eq!(f.verdict, Verdict::Different);
    assert_eq!(f.class, ChangeClass::Transactional);
    // With XACT_ABORT ON T-SQL still undoes the UPDATE, so the finding does
    // not rest on that default.
    assert!(!f
        .assumptions
        .contains(&"ASM-TSQL-CALLER-XACT-ABORT-OFF".to_string()));
    assert!(f.assumptions.contains(&"ASM-NO-TRIGGERS".to_string()));
    let w = witness(&r, f);
    assert_eq!(w.scenario.kind, ScenarioKind::Fault);
    assert_eq!(w.scenario.error_kind, Some(ErrorKind::Constraint));
    assert_eq!(
        w.scenario.statement.as_deref(),
        Some("the INSERT into stock_log")
    );
    assert_eq!((w.scenario.line_a, w.scenario.line_b), (Some(7), Some(6)));
    assert_eq!(w.a.exit, "returns code 2");
    assert!(
        w.a.undone.iter().any(|x| x.contains("UPDATE of stock")),
        "{:?}",
        w.a
    );
    assert!(
        w.b.durable.iter().any(|x| x.contains("UPDATE of stock")),
        "{:?}",
        w.b
    );
    assert!(w.b.exit.contains("propagates"));

    // 2. With a caller transaction the outcomes differ, but only if the
    //    default assumption is flipped.
    let f = finding(&r, "txn.caller-transaction");
    assert_eq!(f.verdict, Verdict::EqualUnderAssumptions);
    assert_eq!(
        witness(&r, f).scenario.kind,
        ScenarioKind::CallerTransaction
    );

    // 3. Return codes against an SQLSTATE.
    let f = finding(&r, "error.contract");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(f.assumptions.contains(&"ASM-CALLER-CHECKS-RC".to_string()));
}

#[test]
fn xact_abort_on_in_the_caller_reruns_the_worked_example() {
    // "With XACT_ABORT ON in the caller, witness 1 changes on the T-SQL side
    // (the batch aborts and rolls back; the return code is never set)."
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let r = diff(&a, &b, &flipped("ASM-TSQL-CALLER-XACT-ABORT-OFF", false));
    assert!(!r
        .assumptions
        .iter()
        .any(|x| x.id == "ASM-TSQL-CALLER-XACT-ABORT-OFF"));
    let w = r
        .witnesses
        .iter()
        .find(|w| {
            w.scenario.xact_abort
                && w.scenario.statement.as_deref() == Some("the INSERT into stock_log")
        })
        .expect("an INSERT-fault witness under XACT_ABORT ON");
    assert!(w.a.exit.contains("propagates"), "{:?}", w.a);
    assert!(w.a.undone.iter().any(|x| x.contains("UPDATE of stock")));
    assert!(w.b.durable.iter().any(|x| x.contains("UPDATE of stock")));
}

#[test]
fn denying_no_caller_txn_makes_caller_divergences_real() {
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let r = ledger::evaluate(&a, &b, &flipped("ASM-NO-CALLER-TXN", false));
    assert!(!r.witnesses.is_empty());
    assert!(r.witnesses.iter().all(|w| w.scenario.caller_transaction));
    assert!(r
        .findings
        .iter()
        .any(|f| f.class == ChangeClass::Transactional && f.verdict == Verdict::Different));
    assert!(!r
        .findings
        .iter()
        .any(|f| f.rule == "txn.caller-transaction"));
}

#[test]
fn m5a_tsql_pg_divergences_match_the_oracle() {
    // Oracle: a constraint fault on stock returns 1 on SQL Server and an error on PostgreSQL.
    let (a, b) = p01("pg.sql", Dialect::Postgres);
    let r = diff(&a, &b, &Options::default());
    let f = finding(&r, "txn.caller-continuation");
    assert_eq!(f.verdict, Verdict::Different);
    let w = witness(&r, f);
    assert_eq!(w.scenario.statement.as_deref(), Some("the UPDATE of stock"));
    assert!(
        w.a.control.contains("goes on with the next statement"),
        "{:?}",
        w.a
    );
    assert_eq!(w.a.exit, "returns code 1");
    assert_eq!(w.b.control, "leaves the routine with the error");
    assert_eq!(finding(&r, "error.contract").verdict, Verdict::Different);

    // Oracle: a constraint fault on orders leaves @OrderId NULL on SQL Server.
    let c = "pairs/p07-insert-return-id";
    let r = diff(
        &side(c, "tsql.sql", Dialect::Tsql),
        &side(c, "pg.sql", Dialect::Postgres),
        &Options::default(),
    );
    let f = finding(&r, "txn.caller-continuation");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(f.witness.contains("A line 6 runs next"), "{}", f.witness);
    assert_eq!(r.verdict, Verdict::Different);

    // Oracle: with a non-positive price SQL Server writes audit rows, PostgreSQL none.
    let c = "pairs/p10-temp-state-rollback";
    let r = diff(
        &side(c, "tsql.sql", Dialect::Tsql),
        &side(c, "pg.sql", Dialect::Postgres),
        &Options::default(),
    );
    let f = finding(&r, "txn.temp-state");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(f.witness.contains("still holds the rows"), "{}", f.witness);
    assert!(f.witness.contains("is empty"), "{}", f.witness);
}

#[test]
fn single_statement_routines_do_not_diverge_on_continuation() {
    // Nothing runs after the failing DELETE, so the caller sees an error
    // either way (the oracle agrees: p08 is equal).
    let c = "pairs/p08-delete-orphans";
    let r = diff(
        &side(c, "tsql.sql", Dialect::Tsql),
        &side(c, "pg.sql", Dialect::Postgres),
        &Options::default(),
    );
    assert!(!r
        .findings
        .iter()
        .any(|f| f.rule == "txn.caller-continuation"));
    assert_ne!(r.verdict, Verdict::Different);
}

#[test]
fn merge_split_is_aligned() {
    let c = "mutations/m12-merge-by-source-split";
    let r = diff(
        &side(c, "original.tsql.sql", Dialect::Tsql),
        &side(c, "mutated.db2i.sql", Dialect::Db2i),
        &Options::default(),
    );
    let f = finding(&r, "concurrency.split-statement");
    assert_eq!(f.class, ChangeClass::Concurrency);
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("the DELETE from item_price (B line 6)"),
        "{}",
        f.witness
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.atomicity" && f.witness.contains("fails as a whole"))
        .expect("split atomicity");
    assert_eq!(f.verdict, Verdict::Different);
    assert_eq!(f.b.as_ref().map(|l| l.start_line), Some(6));
}

#[test]
fn what_if_answers_one_scenario() {
    let (a, b) = p01("pg.sql", Dialect::Postgres);
    let w = what_if(
        &a,
        &b,
        &Options::default(),
        Side::A,
        7,
        ErrorKind::Constraint,
    );
    assert_eq!(
        w.scenario.statement.as_deref(),
        Some("the INSERT into stock_log")
    );
    assert!(w.raisable_a && w.raisable_b);
    assert_eq!(w.a.len(), 1);
    assert!(w.a[0].control.contains("(line 8)"), "{:?}", w.a[0]);
    assert!(w.a[0].rollback.contains("UPDATE of stock"));
    assert_eq!(w.b[0].control, "leaves the routine with the error");
    assert!(w
        .findings
        .iter()
        .any(|f| f.rule == "txn.caller-continuation"));
    assert_eq!(w.findings.len(), w.witnesses.len());

    // A statement without a write is aligned by kind and order: the COMMIT.
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let w = what_if(&a, &b, &Options::default(), Side::A, 9, ErrorKind::Lock);
    assert!(
        w.notes.iter().any(|n| n.contains("no counterpart")),
        "{:?}",
        w.notes
    );

    // A kind the statement cannot raise is reported, not invented.
    let (a, b) = p01("pg.sql", Dialect::Postgres);
    let w = what_if(&a, &b, &Options::default(), Side::A, 7, ErrorKind::NotFound);
    assert!(!w.raisable_a);
    assert!(
        w.notes.iter().any(|n| n.contains("does not raise")),
        "{:?}",
        w.notes
    );
}

#[test]
fn overlay_pairs_transaction_brackets() {
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let o = overlay(&a, &b);
    assert_eq!(o.a.len(), 1);
    assert_eq!((o.a[0].start_line, o.a[0].end_line), (4, 9));
    assert!(o.a[0].label.contains("COMMIT (line 9)"), "{}", o.a[0].label);
    assert_eq!(o.a[0].partner, Some(0));
    assert!(
        o.b[0].label.contains("no commitment control"),
        "{}",
        o.b[0].label
    );

    let c = "pairs/p10-temp-state-rollback";
    let o = overlay(
        &side(c, "tsql.sql", Dialect::Tsql),
        &side(c, "pg.sql", Dialect::Postgres),
    );
    let kinds: Vec<_> = o.b.iter().map(|x| x.label.as_str()).collect();
    assert!(
        kinds.contains(&"block with EXCEPTION (subtransaction)"),
        "{kinds:?}"
    );
    assert!(o
        .a
        .iter()
        .any(|x| x.label == "CATCH block" && x.partner.is_some()));
}

#[test]
fn ledger_ranks_sources_and_reruns() {
    let c = "pairs/p09-error-rollback";
    let (a, b) = (
        side(c, "tsql.sql", Dialect::Tsql),
        side(c, "pg.sql", Dialect::Postgres),
    );
    let (_, entries) = ledger::ledger(&a, &b, &Options::default());
    let d = entries
        .iter()
        .find(|e| e.id == "ASM-TSQL-CALLER-XACT-ABORT-OFF")
        .expect("XACT_ABORT is proven from source");
    assert_eq!(d.status, Status::Discharged);
    assert!(d.cite.as_deref().unwrap().contains("SET XACT_ABORT ON"));

    let (a, b) = p01("pg.sql", Dialect::Postgres);
    let (r, entries) = ledger::ledger(&a, &b, &Options::default());
    assert_eq!(r.verdict, Verdict::Different);
    let first = &entries[0];
    assert!(first.flips_verdict, "{entries:#?}");
    assert!(first.reruns);
    assert_eq!(first.status, Status::Open);
}

#[test]
fn declared_commit_level_is_used_and_contradictions_reported() {
    // A declared level that contradicts the routine's own SET OPTION.
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let mut o = Options::default();
    o.declared.insert(
        "ASM-DB2-COMMIT-LEVEL".into(),
        "All routines run under *CHG".into(),
    );
    let r = diff(&a, &b, &o);
    let f = finding(&r, "assumption.contradiction");
    assert!(
        f.witness.contains("SET OPTION COMMIT = *NONE"),
        "{}",
        f.witness
    );
    assert_eq!(f.b.as_ref().map(|l| l.start_line), Some(3));

    // Without SET OPTION the commit level decides durability: unknown until declared.
    let t = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.stock SET qty = 0;\n  INSERT INTO dbo.stock_log (item_id) VALUES (1);\nEND\n";
    let d = "CREATE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  UPDATE stock SET qty = 0;\n  INSERT INTO stock_log (item_id) VALUES (1);\nEND;\n";
    let (a, b) = (
        Artifact::new("a.sql", Dialect::Tsql, t),
        Artifact::new("b.sql", Dialect::Db2i, d),
    );
    let r = diff(&a, &b, &Options::default());
    assert!(r.assumptions.iter().any(|x| x.id == "ASM-DB2-COMMIT-LEVEL"));
    let mut o = Options::default();
    o.declared
        .insert("ASM-DB2-COMMIT-LEVEL".into(), "*CHG".into());
    let r2 = diff(&a, &b, &o);
    assert!(r2
        .assumptions
        .iter()
        .any(|x| x.id == "ASM-DB2-COMMIT-LEVEL"));
    let unknown = |r: &Report| {
        r.findings
            .iter()
            .filter(|f| f.witness.contains("commit level is not declared"))
            .count()
    };
    assert!(unknown(&r) > 0, "{:#?}", r.findings);
    assert_eq!(unknown(&r2), 0, "{:#?}", r2.findings);
}

#[test]
fn m5b_untested_db2_facts_cap_the_verdict() {
    // An ATOMIC compound under *NONE relies on V4, which nobody has tested
    // on a real IBM i: the divergence is shown but capped at unknown.
    let t = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  BEGIN TRAN;\n  UPDATE dbo.stock SET qty = 0;\n  INSERT INTO dbo.stock_log (item_id) VALUES (1);\n  COMMIT;\nEND\n";
    let d = "CREATE PROCEDURE p ()\nLANGUAGE SQL\nSET OPTION COMMIT = *NONE\nBEGIN ATOMIC\n  UPDATE stock SET qty = 0;\n  INSERT INTO stock_log (item_id) VALUES (1);\nEND;\n";
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, t),
        &Artifact::new("b.sql", Dialect::Db2i, d),
        &Options::default(),
    );
    let capped: Vec<&Finding> = r
        .findings
        .iter()
        .filter(|f| f.witness.contains("not tested on a real system"))
        .collect();
    assert!(!capped.is_empty(), "{:#?}", r.findings);
    assert!(capped.iter().all(|f| f.verdict == Verdict::Unknown));
    assert!(capped.iter().any(|f| f.witness.contains("vendor fact V4")));
}

// Regressions from the review of PR #23.

#[test]
fn commit_level_synonyms_do_not_contradict() {
    // p01's Db2 routine says COMMIT = *NONE; *NC is the same level.
    let (a, b) = p01("db2i.sql", Dialect::Db2i);
    let mut o = Options::default();
    o.declared
        .insert("ASM-DB2-COMMIT-LEVEL".into(), "*NC".into());
    let r = diff(&a, &b, &o);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.rule == "assumption.contradiction"),
        "{:#?}",
        r.findings
    );
    assert_eq!(ledger::canonical_commit_level("*ur"), "*CHG");
    assert_eq!(ledger::canonical_commit_level("*RS"), "*ALL");
}

#[test]
fn contradictions_on_both_sides_have_their_own_fingerprints() {
    let d = "CREATE PROCEDURE p ()\nLANGUAGE SQL\nSET OPTION COMMIT = *NONE\nBEGIN\n  UPDATE stock SET qty = 0;\nEND;\n";
    let (a, b) = (
        Artifact::new("a.sql", Dialect::Db2i, d),
        Artifact::new("b.sql", Dialect::Db2i, d),
    );
    let mut o = Options::default();
    o.declared
        .insert("ASM-DB2-COMMIT-LEVEL".into(), "*CHG".into());
    let r = diff(&a, &b, &o);
    let fps: Vec<&str> = r
        .findings
        .iter()
        .filter(|f| f.rule == "assumption.contradiction")
        .map(|f| f.fingerprint.as_str())
        .collect();
    assert_eq!(fps.len(), 2, "{:#?}", r.findings);
    assert_ne!(fps[0], fps[1]);
}

#[test]
fn relabel_without_a_hit_keeps_the_verdict() {
    let (a, b) = p01("pg.sql", Dialect::Postgres);
    let mut r = diff(&a, &b, &Options::default());
    // As after review accepted every finding.
    r.verdict = Verdict::EqualUnderAssumptions;
    r.findings.clear();
    let before = r.clone();
    ledger::relabel(&mut r, &Default::default());
    assert_eq!(r.verdict, before.verdict);
    let mut flips = std::collections::BTreeMap::new();
    flips.insert("ASM-NOT-USED".to_string(), false);
    ledger::relabel(&mut r, &flips);
    assert_eq!(r.verdict, before.verdict);
}

#[test]
fn overlay_splits_sequential_implicit_units() {
    let pg = "CREATE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  UPDATE stock SET qty = 0;\n  COMMIT;\n  INSERT INTO stock_log (item_id) VALUES (1);\n  COMMIT;\nEND;\n$$;\n";
    let ts = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  BEGIN TRAN;\n  UPDATE dbo.stock SET qty = 0;\n  COMMIT;\n  BEGIN TRAN;\n  INSERT INTO dbo.stock_log (item_id) VALUES (1);\n  COMMIT;\nEND\n";
    let o = overlay(
        &Artifact::new("a.sql", Dialect::Tsql, ts),
        &Artifact::new("b.sql", Dialect::Postgres, pg),
    );
    let units: Vec<(usize, usize)> =
        o.b.iter()
            .filter(|x| x.label.starts_with("unit of work"))
            .map(|x| (x.start_line, x.end_line))
            .collect();
    assert_eq!(units.len(), 2, "{:#?}", o.b);
    assert_eq!(units[0].1, 6);
    assert_eq!(units[1], (7, 8));
    let ta: Vec<_> =
        o.a.iter()
            .filter(|x| x.label.starts_with("transaction"))
            .collect();
    assert_eq!(ta.len(), 2);
    assert_eq!(ta[1].partner.map(|p| o.b[p].start_line), Some(7));
}
