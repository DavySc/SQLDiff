//! A deterministic count of the work one file's check does.
//!
//! The engine's loops each have a local budget (explorer steps, alignment
//! cells, reach steps, ...), but nothing bounds their sum for one file. This
//! meter adds the counts those loops already keep, per site, for the duration
//! of a [`scope`]. It counts events, never time, on the thread that runs the
//! check, in program order: the same input gives the same totals whatever the
//! thread count, the order files are checked in, or what is in the store.
//!
//! What is counted is work the engine does, as a fixed function of the file:
//!
//! * A speed-up is never counted differently for hitting. The explorer cache
//!   and the flipped-scenario cache of the behaviour model count the first
//!   request for an entry (the run is made, its steps are spent) and nothing
//!   for a repeat, whether the cache answered or, with the caches off, the
//!   entry was made again (not counted: [`muted`]). The tests turn the caches
//!   off and compare. The ledger of first requests belongs to one routine
//!   pair's analysis, so it never carries over between files.
//! * Joining paths that reach the same state is part of the algorithm, not a
//!   cache: which paths join is fixed by the file (the memo is made by the
//!   fault-free run and only read afterwards). A drop is one event.
//! * A fault scenario resumed from a checkpoint counts what it runs from the
//!   fault on. The restore from the nearest checkpoint is [`muted`], so the
//!   spacing of the checkpoints does not matter; the ticks before the fault
//!   were counted when the fault-free run was made.
//! * The merge test of a replay (`Explorer::try_merge`) reads the fault-free
//!   run at each candidate tick. Walking the fault-free run from the
//!   cursor to the candidate is counted: `candidate - cursor` steps, one per
//!   tick, charged as the walk was before the machine was restored. The
//!   machine itself is restored from the nearest checkpoint (muted), so the
//!   spacing of the checkpoints does not matter here either. The count is the
//!   walk's, not the restore's.
//! * Knobs that only cut work short, such as the bound on merge tries of a
//!   replay (`MAX_MERGE_TRIES`), change what a scenario runs, so tuning one
//!   moves the totals. They are listed with the cap when it exists.
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
    /// Similarity scores of a gap between two aligned lists
    /// (`treediff::diff_gap`: the product of the two lengths).
    GapScores,
    /// Items built for a cross-dialect comparison or a refinement
    /// (`equiv::items`: one per lowered statement or control marker).
    ItemsBuilt,
    /// Statement pairs compared (`compare_pair`): one unit per pair, in a
    /// cross-dialect comparison or a refinement.
    PairCompares,
}

const SITES: usize = 10;

const NAMES: [&str; SITES] = [
    "explorer_steps",
    "diff_calls",
    "align_cells",
    "edit_reach",
    "edit_closure",
    "witness_tries",
    "behaviour_diffs",
    "gap_scores",
    "items_built",
    "pair_compares",
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
    /// Behaviour units open on this thread ([`UnitPhase`]).
    static UNITS: Cell<usize> = const { Cell::new(0) };
}

/// Marks a behaviour unit as open while it lives. The statement-level
/// [`Allowance`] and the behaviour model's caps count disjoint phases of a
/// diff, so no statement-level spend may happen inside a unit: a debug
/// assertion in [`Allowance`] checks it, and a later change cannot make one
/// budget pay for the other's work unnoticed.
#[derive(Debug)]
pub struct UnitPhase(());

impl UnitPhase {
    pub fn open() -> UnitPhase {
        UNITS.with(|u| u.set(u.get() + 1));
        UnitPhase(())
    }
}

impl Drop for UnitPhase {
    fn drop(&mut self) {
        UNITS.with(|u| u.set(u.get() - 1));
    }
}

/// Whether a behaviour unit is open on this thread.
pub fn unit_open() -> bool {
    UNITS.with(|u| u.get() > 0)
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

/// Whether counting is switched off here (inside [`muted`]). A work cap must
/// not test while muted: that work was counted when it was first done.
pub fn is_muted() -> bool {
    MUTED.with(Cell::get) > 0
}

/// Whether a scope is open on this thread: a cap reads [`now`], which is zero
/// outside one, so it would never fire.
pub fn in_scope() -> bool {
    METER.with(|m| m.borrow().is_some())
}

/// What the statement-level checker (alignment, tree diff) may still spend,
/// passed down as a parameter so the functions that use it stay pure: a
/// deterministic count, decremented in program order, never a time. Off (the
/// default) it allows everything and changes nothing.
#[derive(Debug, Default)]
pub struct Allowance {
    left: Cell<Option<u64>>,
    cap: Option<u64>,
}

impl Allowance {
    /// An allowance of `cap` units, or none (`None`).
    pub fn new(cap: Option<u64>) -> Allowance {
        Allowance {
            left: Cell::new(cap),
            cap,
        }
    }

    /// Whether `n` more units may be spent.
    pub fn allows(&self, n: u64) -> bool {
        debug_assert!(
            !unit_open() || !self.is_on(),
            "a statement-level allowance was used inside a behaviour unit"
        );
        self.left.get().map_or(true, |l| n <= l)
    }

    /// Spends `n` units (saturating); nothing when the allowance is off.
    pub fn take(&self, n: u64) {
        debug_assert!(
            !unit_open() || !self.is_on(),
            "a statement-level allowance was used inside a behaviour unit"
        );
        if let Some(l) = self.left.get() {
            self.left.set(Some(l.saturating_sub(n)));
        }
    }

    pub fn is_on(&self) -> bool {
        self.cap.is_some()
    }

    /// On, and nothing is left: the next unit of work is refused.
    pub fn exhausted(&self) -> bool {
        self.is_on() && !self.allows(1)
    }

    /// What is left (`None` when the allowance is off).
    pub fn left(&self) -> Option<u64> {
        self.left.get()
    }

    /// The whole allowance, for the witness of a cut.
    pub fn cap(&self) -> Option<u64> {
        self.cap
    }

    /// What has been spent so far.
    pub fn spent(&self) -> u64 {
        match (self.cap, self.left.get()) {
            (Some(c), Some(l)) => c - l,
            _ => 0,
        }
    }
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
    Site::GapScores,
    Site::ItemsBuilt,
    Site::PairCompares,
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
    fn the_site_list_matches_the_sites() {
        for (i, s) in SITE_LIST.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
        assert_eq!(NAMES.len(), SITES);
    }

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

#[cfg(all(test, debug_assertions))]
mod phase_tests {
    use super::*;

    #[test]
    fn an_allowance_that_is_off_is_fine_inside_a_unit() {
        let _u = UnitPhase::open();
        assert!(Allowance::new(None).allows(5));
    }

    #[test]
    #[should_panic(expected = "inside a behaviour unit")]
    fn a_statement_level_allowance_inside_a_unit_is_refused() {
        let _u = UnitPhase::open();
        Allowance::new(Some(10)).take(1);
    }

    #[test]
    fn the_phase_closes_with_its_guard() {
        {
            let _u = UnitPhase::open();
            assert!(unit_open());
        }
        assert!(!unit_open());
        Allowance::new(Some(10)).take(1);
    }
}
