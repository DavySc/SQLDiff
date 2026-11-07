//! One-sided path joining (perf plan W6b): paths that reach a join point in
//! the same state are joined, and joining changes nothing a comparison
//! reads. Compared here on the explorer: the joined run's outcomes equal the
//! plain run's but for the statements run and the conditions decided, for
//! the fault-free run and for every fault scenario.

use std::collections::BTreeSet;

use sqldiff_core::artifact::Analyzed;
use sqldiff_core::ir::{self, ErrorKind, Explorer, Fault, KeyUniverse, Outcome, Scenario};
use sqldiff_core::model::Side;
use sqldiff_core::{Artifact, Dialect};

fn cfg_of(sql: &str) -> ir::Cfg {
    let art = Artifact::new("a.sql", Dialect::Tsql, sql);
    ir::lower(&Analyzed::new(&art, Side::A, 3))
}

fn proc_of(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @p int AS\nBEGIN\n{body}END\nGO\n")
}

/// The outcome without what joining may differ in.
fn stripped(o: &Outcome) -> String {
    let mut o = o.clone();
    o.trace = ir::interp::Trace::default();
    o.signature.clear();
    o.fault_index = None;
    format!("{o:?}")
}

fn set(v: &[Outcome]) -> BTreeSet<String> {
    v.iter().map(stripped).collect()
}

fn explorers(cfg: &ir::Cfg, base: &Scenario) -> (Explorer, Explorer) {
    let keys = KeyUniverse::of(&[cfg]);
    (
        Explorer::in_universe_joined(cfg, base, &keys, false),
        Explorer::in_universe_joined(cfg, base, &keys, true),
    )
}

/// Branches that change nothing the rest of the run reads rejoin.
fn quiet_ifs(n: usize) -> String {
    (0..n)
        .map(|i| format!("  IF @p > {i}\n    PRINT 'x{i}';\n"))
        .collect::<String>()
        + "  UPDATE dbo.t SET a = 1;\n"
}

#[test]
fn branches_that_leave_the_same_state_are_joined() {
    let cfg = cfg_of(&proc_of(&quiet_ifs(6)));
    let (plain, joined) = explorers(&cfg, &Scenario::default());
    assert_eq!(plain.base().len(), 64);
    assert_eq!(joined.base().len(), 1);
    assert_eq!(joined.joined(), 6);
    assert_eq!(set(plain.base()), set(joined.base()));
}

#[test]
fn branches_that_leave_different_state_are_not_joined() {
    // Each branch writes a different table: the paths never meet in a state.
    let body: String = (0..4)
        .map(|i| format!("  IF @p > {i}\n    INSERT INTO dbo.t{i}(a) VALUES (1);\n"))
        .collect();
    let cfg = cfg_of(&proc_of(&body));
    let (plain, joined) = explorers(&cfg, &Scenario::default());
    assert_eq!(joined.joined(), 0);
    assert_eq!(joined.base().len(), plain.base().len());
}

/// A branch that records history the other does not (a temporary table it
/// reads) is not joined with it: the outcomes differ in what they read.
#[test]
fn branches_with_different_history_are_not_joined() {
    let body = "  CREATE TABLE #t (a int);\n  IF @p > 0\n    SELECT a FROM #t;\n  UPDATE dbo.t SET a = 1;\n";
    let cfg = cfg_of(&proc_of(body));
    let (plain, joined) = explorers(&cfg, &Scenario::default());
    assert_eq!(set(plain.base()), set(joined.base()));
    assert_eq!(joined.joined(), 0, "the read is history of one path only");
}

fn check_faults(name: &str, sql: &str) -> usize {
    let cfg = cfg_of(sql);
    let mut joined_total = 0;
    for (caller_txn, xact_abort) in [(false, false), (true, false), (false, true)] {
        let base = Scenario {
            caller_txn,
            xact_abort,
            ..Scenario::default()
        };
        let (plain, joined) = explorers(&cfg, &base);
        joined_total += joined.joined();
        assert_eq!(set(plain.base()), set(joined.base()), "{name}");
        for n in 0..cfg.nodes.len() {
            if cfg.node(n).stmt().is_none() {
                continue;
            }
            // Joining drops a path only after a path with its state has
            // passed the node: no statement is lost.
            assert_eq!(plain.reaches(n), joined.reaches(n), "{name}: node {n}");
            for kind in ErrorKind::ALL {
                let sc = Scenario {
                    faults: vec![Fault { node: n, kind }],
                    ..base.clone()
                };
                assert_eq!(
                    set(&plain.explore(&cfg, &sc)),
                    set(&joined.explore(&cfg, &sc)),
                    "{name}: fault {kind:?} at node {n}, caller_txn {caller_txn}, xact_abort {xact_abort}\n{}",
                    cfg.dump()
                );
            }
        }
    }
    joined_total
}

#[test]
fn fault_scenarios_agree_with_the_plain_run() {
    let shapes = [
        ("quiet diamonds", proc_of(&quiet_ifs(4))),
        (
            "fault inside a diamond",
            proc_of("  IF @p > 0\n    BEGIN\n      UPDATE dbo.a SET x = 1;\n      PRINT 'a';\n    END\n  ELSE\n    PRINT 'b';\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "writes before and after",
            proc_of("  INSERT INTO dbo.a(x) VALUES (1);\n  IF @p > 0\n    PRINT 'a';\n  IF @p > 1\n    PRINT 'b';\n  INSERT INTO dbo.b(x) VALUES (2);\n  IF @p > 2\n    PRINT 'c';\n  INSERT INTO dbo.c(x) VALUES (3);\n"),
        ),
        (
            "transaction",
            proc_of("  BEGIN TRANSACTION;\n  IF @p > 0\n    PRINT 'a';\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'b';\n  COMMIT;\n"),
        ),
        (
            "try catch",
            proc_of("  BEGIN TRY\n    IF @p > 0\n      PRINT 'a';\n    UPDATE dbo.t SET a = 1;\n    IF @p > 1\n      PRINT 'b';\n  END TRY\n  BEGIN CATCH\n    IF @p > 2\n      PRINT 'c';\n  END CATCH\n"),
        ),
        (
            "loop",
            proc_of("  WHILE @p > 0\n  BEGIN\n    IF @p > 5\n      PRINT 'a';\n    SET @p = @p - 1;\n  END\n  UPDATE dbo.t SET a = 1;\n"),
        ),
    ];
    let mut joined = 0;
    for (name, sql) in &shapes {
        joined += check_faults(name, sql);
    }
    assert!(joined > 0, "the shapes must exercise joining");
}

#[test]
fn only_a_side_without_decisions_allows_joining() {
    assert!(ir::decision_free(&cfg_of(&proc_of(
        "  UPDATE dbo.t SET a = 1;\n"
    ))));
    assert!(!ir::decision_free(&cfg_of(&proc_of(&quiet_ifs(1)))));
    assert!(!ir::decision_free(&cfg_of(&proc_of(
        "  UPDATE dbo.t SET a = 1;\n  IF @@ROWCOUNT = 0\n    PRINT 'x';\n"
    ))));
}
