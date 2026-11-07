//! A deterministic count of the work one file's check does.
//!
//! The engine's loops each have a local budget (explorer steps, alignment
//! cells, reach steps, ...), but nothing bounds their sum for one file. This
//! meter adds the counts those loops already keep, per site, for the duration
//! of a [`scope`]. It counts events, never time, on the thread that runs the
//! check, in program order: the same input gives the same totals whatever the
//! thread count, the order files are checked in, or what is in the store.
//!
//! This module only counts. It never changes a result or ends a loop; a cap
//! (a new bound) is a later change with its own review. The totals are read
//! by tests and by `SQLDIFF_WORK=1`, which prints one line per outermost
//! scope to stderr (a debug print, like `SQLDIFF_EDIT_STATS`).

use std::cell::{Cell, RefCell};

/// Where work is counted. Each site counts one kind of event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Site {
    /// Explorer path steps (`tick_path`).
    ExplorerSteps,
    /// Calls of the structural diff (`treediff::diff`).
    DiffCalls,
    /// Cells of the alignments (`lcs_pairs`: the product of the lengths of
    /// the part that is aligned).
    AlignCells,
    /// Steps of the edit checker's shared reach sweep.
    EditReach,
    /// Wake-ups of the edit checker's per-edit closures.
    EditClosure,
    /// Candidates tried by the witness search (`eval`).
    WitnessTries,
    /// Outcome-pair comparisons of the behaviour model: the existing
    /// per-routine and per-file allowance (`Collector::spend`), kept as it is.
    BehaviourDiffs,
}

const SITES: usize = 7;

const NAMES: [&str; SITES] = [
    "explorer_steps",
    "diff_calls",
    "align_cells",
    "edit_reach",
    "edit_closure",
    "witness_tries",
    "behaviour_diffs",
];

/// The counts of one scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    counts: [u64; SITES],
}

impl Work {
    pub fn get(&self, site: Site) -> u64 {
        self.counts[site as usize]
    }

    /// What was counted after `earlier` (a snapshot from [`now`] of the same
    /// scope).
    pub fn since(&self, earlier: &Work) -> Work {
        let mut d = Work::default();
        for i in 0..SITES {
            d.counts[i] = self.counts[i].saturating_sub(earlier.counts[i]);
        }
        d
    }

    /// The sum over all sites: the file's total work.
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }
}

impl std::fmt::Display for Work {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "total={}", self.total())?;
        for (n, c) in NAMES.iter().zip(&self.counts) {
            write!(f, " {n}={c}")?;
        }
        Ok(())
    }
}

thread_local! {
    /// The open scope's counts, if there is one.
    static METER: RefCell<Option<Work>> = const { RefCell::new(None) };
    /// Whether the open scope is the outermost one's (it prints).
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Open [`muted`] sections: counts are dropped while it is above 0.
    static MUTED: Cell<usize> = const { Cell::new(0) };
}

/// Events counted with no scope open: an entry point nobody metered. Once a
/// cap exists such an entry would run uncapped, so the cap change must bring
/// this to zero for every production path (the test entries that call the
/// engine's parts directly open a scope or accept it). Read by
/// [`unmetered`]; never part of a result.
static UNMETERED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Events counted outside any scope so far in this process.
pub fn unmetered() -> u64 {
    UNMETERED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Counts `n` events at `site` in the open scope. Call it per batch (a
/// call, a row, a run of steps), not per cell.
pub fn spend(site: Site, n: u64) {
    if MUTED.with(Cell::get) > 0 {
        return;
    }
    METER.with(|m| match m.borrow_mut().as_mut() {
        Some(w) => {
            let c = &mut w.counts[site as usize];
            *c = c.saturating_add(n);
        }
        None => {
            UNMETERED.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// The counts of the open scope so far (zero when none is open).
pub fn now() -> Work {
    METER.with(|m| m.borrow().unwrap_or_default())
}

/// Counts a recorded amount of work again, per site: what a cache or memo
/// entry cost when it was made, charged when it is used instead, so that the
/// total is what the algorithm does and not what its caches happened to save.
pub fn charge(w: &Work) {
    for (i, &n) in w.counts.iter().enumerate() {
        if n > 0 {
            spend(SITE_LIST[i], n);
        }
    }
}

const SITE_LIST: [Site; SITES] = [
    Site::ExplorerSteps,
    Site::DiffCalls,
    Site::AlignCells,
    Site::EditReach,
    Site::EditClosure,
    Site::WitnessTries,
    Site::BehaviourDiffs,
];

/// Runs `f` without counting: work that only re-creates a state whose cost
/// was counted when it was first made (a replay from a checkpoint). The
/// caller charges what the work is worth logically.
pub fn muted<R>(f: impl FnOnce() -> R) -> R {
    struct Unmute;
    impl Drop for Unmute {
        fn drop(&mut self) {
            MUTED.with(|m| m.set(m.get() - 1));
        }
    }
    MUTED.with(|m| m.set(m.get() + 1));
    let _u = Unmute;
    f()
}

/// Runs `f` with a meter and returns its result with the counts. A scope
/// inside another one shares the outer meter (its counts are the work done
/// inside it, and the outer scope still sees them).
pub fn scope<R>(label: &str, f: impl FnOnce() -> R) -> (R, Work) {
    let outer = METER.with(|m| m.borrow().is_some());
    if outer {
        let before = METER.with(|m| m.borrow().unwrap_or_default());
        let r = f();
        let after = METER.with(|m| m.borrow().unwrap_or_default());
        let mut d = Work::default();
        for i in 0..SITES {
            d.counts[i] = after.counts[i] - before.counts[i];
        }
        return (r, d);
    }
    METER.with(|m| *m.borrow_mut() = Some(Work::default()));
    DEPTH.with(|d| d.set(1));
    // A panic in `f` must not leave the meter open for the next file.
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            METER.with(|m| *m.borrow_mut() = None);
            DEPTH.with(|d| d.set(0));
        }
    }
    let close = Close;
    let r = f();
    let w = METER.with(|m| m.borrow().unwrap_or_default());
    drop(close);
    if std::env::var_os("SQLDIFF_WORK").is_some() {
        eprintln!("work: {label}: {w}");
    }
    (r, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_in_a_scope_and_nothing_outside() {
        spend(Site::DiffCalls, 5);
        assert!(unmetered() >= 5);
        let (_, w) = scope("t", || {
            spend(Site::DiffCalls, 2);
            spend(Site::AlignCells, 10);
        });
        assert_eq!(w.get(Site::DiffCalls), 2);
        assert_eq!(w.get(Site::AlignCells), 10);
        assert_eq!(w.total(), 12);
        let (_, again) = scope("t", || spend(Site::DiffCalls, 1));
        assert_eq!(again.total(), 1);
    }

    #[test]
    fn a_nested_scope_reports_its_part_and_the_outer_sees_all() {
        let (inner, outer) = scope("outer", || {
            spend(Site::DiffCalls, 1);
            let (_, i) = scope("inner", || spend(Site::DiffCalls, 4));
            spend(Site::DiffCalls, 1);
            i
        });
        assert_eq!(inner.total(), 4);
        assert_eq!(outer.total(), 6);
    }

    #[test]
    fn a_panic_closes_the_meter() {
        let r = std::panic::catch_unwind(|| scope("p", || panic!("x")));
        assert!(r.is_err());
        let (_, w) = scope("after", || ());
        assert_eq!(w.total(), 0);
    }
}
