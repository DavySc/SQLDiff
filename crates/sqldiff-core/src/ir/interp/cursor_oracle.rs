//! The merge test's cursor machine against the walk it replaced.
//!
//! `Explorer::try_merge` reads the fault-free run at a candidate tick `u`
//! from a machine it restores from a checkpoint and charges the `u - cu`
//! steps of walking there from the previous candidate `cu`. It
//! used to walk a machine from `machine_at(t)` to `u`, counting each tick.
//! The restored machine must be that walked machine. Under `cfg(test)` every
//! candidate try, while a test enables the oracle, rebuilds the old walk
//! (muted, so the meter is not charged twice) and compares:
//!
//! * the current path (`Debug` of `cur`),
//! * the paths waiting (`Debug` of every `work` entry, in order),
//! * the outcomes made (`made()` against the walked machine's `outs.len()`),
//! * the join (`tick`, `live`, number of open entries).
//!
//! The production code only calls [`check`], from one `cfg(test)` line.

use std::cell::Cell;

use sqldiff_model::work::{muted, scope, Site, Work};

use super::{Cfg, Explorer, Interp, Machine, Profile};
use crate::artifact::Analyzed;
use crate::ir::{self, ErrorKind, Fault, KeyUniverse, Scenario};
use crate::model::Side;
use crate::{diff, Artifact, Dialect, Options};

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static CHECKS: Cell<usize> = const { Cell::new(0) };
}

/// Compares `cm`, the cursor machine at tick `u` (before its step), with the
/// walk from `machine_at(t)`. Does nothing unless a test turned the oracle on.
pub(super) fn check(ex: &Explorer, cfg: &Cfg, t: usize, u: usize, cm: &Machine) {
    if !ON.with(Cell::get) {
        return;
    }
    CHECKS.with(|c| c.set(c.get() + 1));
    // The old cursor: the machine at `t` (the fault-free run, where the
    // replay left it), walked tick by tick to `u`.
    let mut old = ex.machine_at(cfg, t, true).expect("machine at t");
    old.next_node().expect("a path at t");
    let base = Interp {
        cfg,
        keys: &ex.keys,
        prof: Profile::of(cfg.dialect),
        sc: &ex.base,
    };
    muted(|| {
        for _ in t..u {
            old.next_node().expect("a path before u");
            base.tick(&mut old);
        }
    });
    old.next_node().expect("a path at u");
    let at = format!("candidate tick {u}, replay from {t}");
    assert_eq!(old.outs_before, 0);
    assert_eq!(
        format!("{:?}", cm.cur),
        format!("{:?}", old.cur),
        "cur, {at}"
    );
    assert_eq!(
        format!("{:?}", cm.work),
        format!("{:?}", old.work),
        "work, {at}"
    );
    assert_eq!(cm.made(), old.outs.len(), "outcomes made, {at}");
    let join = |m: &Machine| m.join.as_ref().map(|j| (j.tick, j.live, j.open.len()));
    assert_eq!(join(cm), join(&old), "join, {at}");
}

/// Runs `f` with the oracle on; returns how many candidate tries it checked.
fn checked<R>(f: impl FnOnce() -> R) -> (R, usize) {
    ON.with(|o| o.set(true));
    CHECKS.with(|c| c.set(0));
    let r = f();
    ON.with(|o| o.set(false));
    (r, CHECKS.with(Cell::get))
}

fn cfg_of(body: &str) -> Cfg {
    let sql = format!("CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n{body}END\nGO\n");
    let art = Artifact::new("a.sql", Dialect::Tsql, sql.as_str());
    ir::lower(&Analyzed::new(&art, Side::A, 3))
}

/// `scripts/gen-adversarial.py` section A (`proc`): `n` pairs of an UPDATE
/// and an `IF @@ROWCOUNT = 0 INSERT`.
fn pairs(n: usize, tag: usize) -> String {
    (0..n)
        .map(|k| {
            format!(
                "  UPDATE dbo.T SET Qty = Qty + {k} WHERE Id = @x + {};\n  IF @@ROWCOUNT = 0 INSERT dbo.Log (Msg, At) VALUES (N'none {k}', SYSUTCDATETIME());\n",
                if k == n - 1 { tag } else { k }
            )
        })
        .collect()
}

/// `scripts/gen-adversarial.py` section J: `k` nested IF/ELSE levels, `2^k`
/// paths without joining.
fn branches(k: u32) -> String {
    if k == 0 {
        return "    UPDATE dbo.T SET Qty = Qty + 1 WHERE Id = 1;\n".into();
    }
    format!(
        "    IF @x > {k}\n    BEGIN\n{a}    END\n    ELSE\n    BEGIN\n        UPDATE dbo.T SET Qty = Qty - {k} WHERE Id = {k};\n{a}    END\n",
        a = branches(k - 1)
    )
}

/// Every statement of `cfg` failing with every error kind, one at a time,
/// against a fault-free run (joined or not). Returns the explorer, the
/// number of replays and the work the replays were charged.
fn replay_every_fault(cfg: &Cfg, join: bool) -> (Explorer, usize, Work) {
    let keys = KeyUniverse::of(&[cfg]);
    let base = Scenario::default();
    let ex = Explorer::in_universe_joined(cfg, &base, &keys, join);
    let mut replays = 0;
    let ((), w) = scope("replays", || {
        for node in (0..cfg.nodes.len()).filter(|&n| cfg.node(n).stmt().is_some()) {
            for kind in ErrorKind::ALL {
                let sc = Scenario {
                    faults: vec![Fault { node, kind }],
                    ..base.clone()
                };
                let _ = ex.explore(cfg, &sc);
                replays += 1;
            }
        }
    });
    (ex, replays, w)
}

/// The spacing of the checkpoints: 1 until 256 are kept, then it doubles.
fn every(ex: &Explorer) -> usize {
    match ex.checkpoints.as_slice() {
        [(a, ..), (b, ..), ..] => b - a,
        _ => 1,
    }
}

/// p1x20: the shape `a_procs` is made of (20 pairs). Its fault-free run is
/// longer than 256 ticks, so its checkpoints are thinned.
#[test]
fn the_cursor_machine_is_the_walked_machine_on_a_pairs_procedure() {
    let cfg = cfg_of(&pairs(20, 0));
    let ((ex, replays, _), checks) = checked(|| replay_every_fault(&cfg, false));
    assert!(replays > 100);
    assert!(ex.merges() > 0, "no replay merged");
    assert!(ex.ticks.len() > 256 && every(&ex) > 1, "{}", every(&ex));
    assert!(checks > 100, "{checks} candidate tries checked");
}

/// The same procedure through the whole comparison (`diff`: both sides'
/// explorers, flipped scenarios, `persists`), which replays as `convert`
/// does.
#[test]
fn the_cursor_machine_is_the_walked_machine_through_a_diff() {
    let mk = |tag: usize| {
        let sql = format!(
            "CREATE PROCEDURE dbo.P0 @x int AS\nBEGIN\n  SET NOCOUNT ON;\n{}END\nGO\n",
            pairs(20, tag)
        );
        Artifact::new("a.sql", Dialect::Tsql, sql)
    };
    let (a, b) = (mk(0), mk(1));
    let (_, checks) = checked(|| {
        let _ = diff(&a, &b, &Options::default());
    });
    assert!(checks > 100, "{checks} candidate tries checked");
}

/// j_branches_10 with path joining on (as the behaviour model builds it): a
/// fault-free run of thousands of ticks, so the checkpoints are thinned
/// (`every > 1`), and replays that merge into it. Its branches write
/// different states, so nothing joins here; the next test has joins.
#[test]
fn the_cursor_machine_is_the_walked_machine_on_a_long_branching_run_with_thinned_checkpoints() {
    let cfg = cfg_of(&branches(10));
    let ((ex, replays, _), checks) = checked(|| replay_every_fault(&cfg, true));
    assert!(replays > 100);
    assert!(ex.ticks.len() > 256, "{} ticks", ex.ticks.len());
    assert!(every(&ex) > 1, "every {}", every(&ex));
    assert!(ex.merges() > 0, "no replay merged");
    assert!(checks > 100, "{checks} candidate tries checked");
}

/// Paths that really join (`ex.joined() > 0`: branches that leave the same
/// state meet again) over a run of more than 256 ticks, so the join memo's
/// finished entries are read by re-runs from thinned checkpoints.
#[test]
fn the_cursor_machine_is_the_walked_machine_with_joined_paths_and_thinned_checkpoints() {
    let body: String = (0..12)
        .map(|k| {
            format!(
                "  IF @x > {k} PRINT 'a{k}';\n  UPDATE dbo.T SET Qty = Qty + {k} WHERE Id = @x + {k};\n  IF @@ROWCOUNT = 0 INSERT dbo.Log (Msg, At) VALUES (N'none {k}', SYSUTCDATETIME());\n"
            )
        })
        .collect();
    let cfg = cfg_of(&body);
    let ((ex, replays, _), checks) = checked(|| replay_every_fault(&cfg, true));
    assert!(replays > 100);
    assert!(ex.joined() > 0, "no path joined");
    assert!(ex.ticks.len() > 256, "{} ticks", ex.ticks.len());
    assert!(every(&ex) > 1, "every {}", every(&ex));
    assert!(ex.merges() > 0, "no replay merged");
    assert!(checks > 100, "{checks} candidate tries checked");
}

/// The oracle's walks are muted: the same replays are charged the same with
/// the oracle on.
#[test]
fn the_oracle_charges_nothing() {
    let cfg = cfg_of(&pairs(6, 0));
    let (_, _, plain) = replay_every_fault(&cfg, false);
    let ((_, _, with), checks) = checked(|| replay_every_fault(&cfg, false));
    assert!(checks > 0);
    assert_eq!(
        plain.get(Site::ExplorerSteps),
        with.get(Site::ExplorerSteps)
    );
}
