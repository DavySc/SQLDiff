//! Transaction-state abstract interpretation over the CFG (design §3
//! Layer 3): open depth, savepoint stack, doomed flag, commit mode, the
//! effects not yet durable and when each becomes durable.
//!
//! The interpreter explores paths. Conditions it can decide from its own
//! state (`@@ERROR`, `@@TRANCOUNT`, `XACT_STATE()`) are evaluated; other
//! conditions fork, and each path records the decisions it took so paths of
//! two versions can be matched. Loops run at most twice; if the second
//! iteration changes the transaction state the path is marked imprecise
//! (the comparison then degrades to "unknown", never "equal"). A scenario
//! can inject a fault of one error kind at one statement.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use sqldiff_syntax::Dialect;

use crate::classify::StmtKind;

use super::cfg::{Action, Cfg, Edge, NodeId, Op, RoutineKind, ScopeId, ScopeKind, Test, EXIT_OK};
use super::delta::fate_code;
pub use super::delta::{BeforeFault, Effects, KeySet, Trace};
use super::profile::{Context, ErrorKind, Profile, Situation, Status, TxnEffect};

/// A fault injected at a statement: the statement fails with `kind` the
/// first time it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub node: NodeId,
    pub kind: ErrorKind,
}

/// The environment a run starts in, and the faults to inject.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scenario {
    /// The caller already holds an open transaction.
    pub caller_txn: bool,
    /// T-SQL session settings inherited from the caller.
    pub xact_abort: bool,
    pub implicit_transactions: bool,
    /// Db2 for i commit level when the artifact does not declare one.
    pub commit_level: Option<String>,
    pub faults: Vec<Fault>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Exit {
    Normal,
    /// `RETURN n` with n other than 0.
    Code(String),
    /// Left with an unhandled error.
    Raised(ErrorKind),
}

/// What became of an effect by the time the routine exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fate {
    Durable,
    Undone,
    /// The failing statement kept the rows it changed before the error.
    Partial,
    /// Still uncommitted in a transaction the caller owns: the caller
    /// commits or rolls it back.
    Caller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TempState {
    Empty,
    Rows,
}

/// Where the injected fault sent control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// Into a handler (`try_catch`, `exception_block`, `exit_handler`, ...).
    Handler {
        construct: &'static str,
        entry: NodeId,
    },
    /// On to the next statement (statement-level error, or no rows).
    Next(NodeId),
    /// Out of the routine with the error.
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub exit: Exit,
    /// Statement nodes run, in order.
    pub trace: Trace,
    /// Index in `trace` of the statement the fault was injected into.
    pub fault_index: Option<usize>,
    /// Where the injected fault sent control.
    pub fault_route: Option<Routed>,
    /// Fate of each durable effect that ran, by effect key. In
    /// caller-transaction scenarios the key `caller` stands for the caller's
    /// own pending work.
    pub effects: Effects,
    /// Effects still uncommitted when the routine returns, left to the
    /// caller (including those the caller is assumed to commit).
    pub open: KeySet,
    /// Effects whose statement failed part-way and kept some rows.
    pub partial: BTreeSet<String>,
    /// Effects that had already run when the first fault hit.
    pub before_fault: BeforeFault,
    /// Effect key of the statement the fault was injected into.
    pub faulted: Option<String>,
    /// The path reached a statement with an injected fault.
    pub fault_hit: bool,
    /// (reading statement node, temporary object, state seen).
    pub temp_reads: BTreeSet<(NodeId, String, TempState)>,
    /// An error message reached the caller (also when execution went on).
    pub raised: bool,
    /// Transaction depth at exit, and whether the routine left a
    /// transaction it opened.
    pub depth: u32,
    pub left_open: bool,
    pub doomed: bool,
    /// Decisions on conditions the interpreter could not evaluate.
    pub signature: Vec<(String, bool)>,
    pub imprecise: Vec<String>,
    /// Where the exploration stopped short of this path's end: the statement
    /// the step budget cut the path before, and the first statements of the
    /// branches the path budget dropped. What the path could still have run
    /// from these was not explored (see `Cfg::reachable_from`).
    pub cut: Vec<NodeId>,
    /// Set when the fates of effects are not known (e.g. the Db2 for i
    /// commit level is not declared); control flow is still exact.
    pub durability_unknown: Option<String>,
    /// Profile rows this path relied on.
    pub rows: BTreeSet<String>,
    pub assumptions: BTreeSet<String>,
    /// Profile rows this path relied on that are marked unverified (not
    /// tested on a real system): verdicts that depend on them are capped.
    pub untested: BTreeSet<String>,
    /// Effects left to the caller to commit (a unit of work whose caller
    /// commits it), which is what `ASM-CALLER-COMMITS` records.
    unit_commit: usize,
}

impl Outcome {
    /// An outcome of a run that did nothing (for findings that need no run).
    pub fn empty() -> Outcome {
        Outcome {
            exit: Exit::Normal,
            trace: Trace::default(),
            fault_index: None,
            fault_route: None,
            effects: Effects::default(),
            open: KeySet::default(),
            partial: BTreeSet::new(),
            before_fault: BeforeFault::default(),
            faulted: None,
            fault_hit: false,
            temp_reads: BTreeSet::new(),
            raised: false,
            depth: 0,
            left_open: false,
            doomed: false,
            signature: Vec::new(),
            imprecise: Vec::new(),
            cut: Vec::new(),
            durability_unknown: None,
            rows: BTreeSet::new(),
            assumptions: BTreeSet::new(),
            untested: BTreeSet::new(),
            unit_commit: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Live {
    Durable,
    Pending,
    Undone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// Statements commit on their own unless the artifact opens a transaction.
    Autocommit,
    /// Inside the calling statement, which autocommits (no caller transaction).
    CallerStatement,
    /// PostgreSQL CALL/DO at top level: one transaction, committed at the end.
    Call,
    /// Db2 for i unit of work of the activation group (commitment control).
    UnitOfWork,
    /// The caller holds an open transaction.
    CallerTxn,
}

#[derive(Debug, Clone)]
enum Log {
    /// Id of the effect key (see [`Keys`]).
    Effect(usize),
    Temp(String, TempState),
}

#[derive(Debug, Clone)]
struct State {
    depth: u32,
    caller_depth: u32,
    owner: Owner,
    no_commit: bool,
    doomed: bool,
    xact_abort: bool,
    xact_abort_local: bool,
    implicit: bool,
    /// State of each effect that ran, by key id (see [`Keys`]).
    live: Vec<Option<Live>>,
    log: Vec<Log>,
    temp: BTreeMap<String, TempState>,
    scopes: Vec<(ScopeId, usize)>,
    savepoints: Vec<(String, usize)>,
    resume: Vec<NodeId>,
    caught: Vec<ErrorKind>,
    last_error: Option<ErrorKind>,
    /// Whether the last statement touched no rows, when a scenario says so.
    no_rows: Option<bool>,
    /// The statement whose row count `no_rows` / `@@ROWCOUNT` / `FOUND`
    /// reads now (names the condition of a row-count test in a signature).
    rows_stmt: Option<NodeId>,
    /// That statement failed, so the row count is what the dialect leaves
    /// after an error (T-SQL 0, PG unchanged, Db2 the error code).
    rows_failed: bool,
    partial: BTreeSet<String>,
    entry_depth: u32,
    faults_done: BTreeSet<NodeId>,
    before_fault: Option<BeforeFault>,
    faulted: Option<String>,
    temp_reads: BTreeSet<(NodeId, String, TempState)>,
    raised: bool,
    ret: Option<String>,
    raising: Option<ErrorKind>,
    loop_seen: HashMap<NodeId, Vec<String>>,
    signature: Vec<(String, bool)>,
    imprecise: Vec<String>,
    cut: Vec<NodeId>,
    durability_unknown: Option<String>,
    rows: BTreeSet<String>,
    assumptions: BTreeSet<String>,
    steps: usize,
    trace: Vec<NodeId>,
    fault_index: Option<usize>,
    fault_route: Option<Routed>,
    untested: BTreeSet<String>,
}

impl State {
    /// Whether `self` and `b` are in the same *run state*: everything the
    /// step function reads, but for the slots of the effects `spent` (which
    /// no statement can write again) and the log entries that name them,
    /// which `b` may have and `self` must not. The fields are all listed, so
    /// a new one must be put on one side or the other. `None`: not the same;
    /// `Some(shifted)`: the same, `shifted` when `b`'s log has such entries.
    fn same_run(&self, b: &State, spent: &[usize]) -> Option<bool> {
        let State {
            depth,
            caller_depth,
            owner,
            no_commit,
            doomed,
            xact_abort,
            xact_abort_local,
            implicit,
            live,
            log,
            temp,
            scopes,
            savepoints,
            resume,
            caught,
            last_error,
            no_rows,
            rows_stmt,
            rows_failed,
            entry_depth,
            loop_seen,
            ret,
            raising,
            durability_unknown,
            steps,
            // History: it only reaches the outcome (see `history_covers`).
            partial: _,
            temp_reads: _,
            raised: _,
            signature: _,
            imprecise: _,
            cut: _,
            rows: _,
            assumptions: _,
            untested: _,
            trace: _,
            // What the fault changed, and the outcome keeps from `self`.
            faults_done: _,
            before_fault: _,
            faulted: _,
            fault_index: _,
            fault_route: _,
        } = self;
        if *depth != b.depth
            || *caller_depth != b.caller_depth
            || *owner != b.owner
            || *no_commit != b.no_commit
            || *doomed != b.doomed
            || *xact_abort != b.xact_abort
            || *xact_abort_local != b.xact_abort_local
            || *implicit != b.implicit
            || *temp != b.temp
            || *resume != b.resume
            || *caught != b.caught
            || *last_error != b.last_error
            || *no_rows != b.no_rows
            || *rows_stmt != b.rows_stmt
            || *rows_failed != b.rows_failed
            || *entry_depth != b.entry_depth
            || *loop_seen != b.loop_seen
            || *ret != b.ret
            || *raising != b.raising
            || *durability_unknown != b.durability_unknown
            || *steps != b.steps
            || live.len() != b.live.len()
            || !b.faults_done.is_empty()
        {
            return None;
        }
        // The logs: `b`'s (the fault-free path's) may hold entries for the
        // failing statement's effects, which `self`'s does not; without them
        // the two are equal. A marker into a log (a savepoint or scope) is a
        // position in it, so `b`'s markers are counted without those entries.
        let named = |l: &Log| matches!(l, Log::Effect(k) if spent.contains(k));
        if log.iter().any(named) {
            return None;
        }
        let shifted = b.log.iter().any(named);
        let mut upto = Vec::with_capacity(b.log.len() + 1);
        upto.push(0usize);
        let mut mine = log.iter();
        for y in &b.log {
            let n = upto[upto.len() - 1];
            if named(y) {
                upto.push(n);
                continue;
            }
            let same = match (mine.next(), y) {
                (Some(Log::Effect(i)), Log::Effect(j)) => i == j,
                (Some(Log::Temp(s, a)), Log::Temp(t, c)) => s == t && a == c,
                _ => false,
            };
            if !same {
                return None;
            }
            upto.push(n + 1);
        }
        if mine.next().is_some() {
            return None;
        }
        let marks = |a: &[usize], b: &[usize]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| upto.get(*y) == Some(x))
        };
        if scopes.len() != b.scopes.len()
            || savepoints.len() != b.savepoints.len()
            || scopes.iter().zip(&b.scopes).any(|(x, y)| x.0 != y.0)
            || savepoints
                .iter()
                .zip(&b.savepoints)
                .any(|(x, y)| x.0 != y.0)
        {
            return None;
        }
        let ms = |v: &[(ScopeId, usize)]| v.iter().map(|x| x.1).collect::<Vec<_>>();
        let mv = |v: &[(String, usize)]| v.iter().map(|x| x.1).collect::<Vec<_>>();
        if !marks(&ms(scopes), &ms(&b.scopes)) || !marks(&mv(savepoints), &mv(&b.savepoints)) {
            return None;
        }
        let same = live
            .iter()
            .zip(&b.live)
            .enumerate()
            .all(|(i, (x, y))| x == y || spent.contains(&i));
        same.then_some(shifted)
    }

    /// The state of a path but for `steps`, the signature, the trace and the
    /// names of the row-count tests, as text: everything the step function
    /// reads (compare [`State::same_run`]) and all the history that reaches
    /// an outcome. Two paths with equal keys at one node make equal outcomes
    /// from any continuation but for those. The fields are all listed, so a
    /// new one must be put on one side or the other.
    fn run_key(&self) -> String {
        use std::fmt::Write;
        let State {
            depth,
            caller_depth,
            owner,
            no_commit,
            doomed,
            xact_abort,
            xact_abort_local,
            implicit,
            live,
            log,
            temp,
            scopes,
            savepoints,
            resume,
            caught,
            last_error,
            no_rows,
            entry_depth,
            loop_seen,
            ret,
            raising,
            durability_unknown,
            partial,
            temp_reads,
            raised,
            imprecise,
            cut,
            rows,
            assumptions,
            untested,
            // The budget is compared by the caller.
            steps: _,
            // Only name the condition of a later row-count test in the
            // signature, which no comparison reads (the fork itself depends
            // on `no_rows`).
            rows_stmt: _,
            rows_failed: _,
            // Which statements ran and which conditions were decided: the
            // only things two joined paths may differ in, and no comparison
            // reads them (see [`Join`]).
            signature: _,
            trace: _,
            // Fault bookkeeping: the caller refuses a path that has any.
            faults_done: _,
            before_fault: _,
            faulted: _,
            fault_index: _,
            fault_route: _,
        } = self;
        #[allow(clippy::disallowed_methods)] // sorted right below
        let mut seen: Vec<(&NodeId, &Vec<String>)> = loop_seen.iter().collect();
        seen.sort();
        let mut s = String::new();
        let _ = write!(
            s,
            "{depth},{caller_depth},{owner:?},{no_commit},{doomed},{xact_abort},{xact_abort_local},{implicit}|{live:?}|{log:?}|{temp:?}|{scopes:?}|{savepoints:?}|{resume:?}|{caught:?}|"
        );
        let _ = write!(
            s,
            "{last_error:?},{no_rows:?},{entry_depth}|{seen:?}|{ret:?}|{raising:?}|{durability_unknown:?}|"
        );
        let _ = write!(
            s,
            "{partial:?}|{temp_reads:?}|{raised}|{imprecise:?}|{cut:?}|{rows:?}|{assumptions:?}|{untested:?}"
        );
        s
    }

    /// Whether what `b` (a path of the fault-free run) had recorded is also
    /// in the history of `self`, so that the union of `self`'s history and
    /// the fault-free outcome's is `self`'s outcome's. A read of a temporary
    /// table that `b` made and `self` did not (the failing statement's own,
    /// or one whose state the fault changed) is let go when `gone(node)`:
    /// the statement cannot run again, so the rest of the run cannot make
    /// that read again, and [`Explorer::patch`] leaves what `b` had made out
    /// of the fault-free outcome's.
    fn history_covers(&self, b: &State, gone: &dyn Fn(NodeId) -> bool) -> bool {
        b.partial.is_subset(&self.partial)
            && b.temp_reads
                .iter()
                .all(|r| self.temp_reads.contains(r) || gone(r.0))
            && b.rows.is_subset(&self.rows)
            && b.assumptions.is_subset(&self.assumptions)
            && b.untested.is_subset(&self.untested)
            && (!b.raised || self.raised)
    }
}

/// Pseudo-effect for the caller's pending work in caller-transaction scenarios.
pub const CALLER: &str = "caller";

pub mod pairs;

const MAX_PATHS: usize = 128;
const MAX_STEPS: usize = 4000;
/// Ticks a run with path joining may use before it stops forking (like the
/// path budget): what the longest run without joining could use is
/// `MAX_PATHS * MAX_STEPS`. Memo entries are capped separately and only
/// where they are made (`Interp::joins`).
const JOIN_TICKS: usize = 200_000;
const JOIN_ENTRIES: usize = 10_000;
const JOIN_KEY_BYTES: usize = 16 * 1024;

enum Step {
    Go(NodeId, Box<State>),
    Fork(Vec<(NodeId, Box<State>)>),
    /// A decision whose second branch the path budget drops: the first
    /// branch continues (as `Fork`'s first entry would) and the dropped
    /// branch is only named. Made when the caller says there is no room
    /// (`step_in`), so the state is not copied for a branch nobody runs.
    Pruned(NodeId, Box<State>, NodeId),
    Done(Box<Outcome>),
}

/// The whole exploration between two steps: the path being run, the forked
/// paths still to run, and the outcomes so far.
#[derive(Clone)]
struct Machine {
    cur: Option<(NodeId, Box<State>)>,
    work: Vec<(NodeId, Box<State>)>,
    outs: Vec<Outcome>,
    /// Path joining (see [`Join`]); `None` runs every path to its end.
    join: Option<Join>,
}

/// One-sided path joining.
///
/// A path that reaches a join point (a node with several predecessors) in the
/// same state as an earlier path whose subtree is finished is dropped: its
/// continuation is the earlier path's step for step, and the outcomes it
/// would make equal the earlier path's in every field a comparison reads (the
/// *state*, history included, is the same; only the statements it ran and the
/// conditions it decided differ). See `docs/behavioural-diff.md` ("Path
/// joining") for the argument and for why it is used only when the other side
/// has no decision.
///
/// The memo of arrivals belongs to the [`Explorer`] and is shared by the
/// machines it checkpoints; an entry is visible to a tick only once its
/// subtree is finished (`done_at`), so re-running the fault-free run from a
/// checkpoint drops the same paths.
type JoinKey = (NodeId, Rc<str>);
type JoinMemo = BTreeMap<JoinKey, JoinEntry>;

#[derive(Clone)]
struct Join {
    points: Rc<Vec<bool>>,
    memo: Rc<std::cell::RefCell<JoinMemo>>,
    /// The memo is being made (the fault-free run) or only read (a re-run).
    live: bool,
    /// Entries whose subtree is still running: (forked paths waiting when the
    /// path arrived, entry), innermost last.
    open: Vec<(usize, JoinKey)>,
    /// Number of the tick the machine runs next.
    tick: usize,
    joined: usize,
    /// Tick of the arrival whose entry the last dropped path joined.
    last_arrived: usize,
}

#[derive(Clone)]
struct JoinEntry {
    /// Tick of the arrival that made the entry, and its `steps`.
    arrived: usize,
    steps: usize,
    outs_from: usize,
    /// Set when the subtree is finished: the end of its outcomes and the
    /// tick from which the entry may be joined.
    outs_to: Option<usize>,
    done_at: Option<usize>,
    /// Some outcome of the subtree was cut by the step budget.
    step_cut: bool,
}

impl Join {
    fn new(cfg: &Cfg) -> Join {
        let mut preds = vec![0usize; cfg.nodes.len()];
        for n in &cfg.nodes {
            for (_, t) in &n.succ {
                preds[*t] += 1;
            }
        }
        // Only the control flow of the statements merges here; the error
        // routes (every statement has one to its successor) do not, or every
        // node would be a join point.
        Join {
            points: Rc::new(preds.iter().map(|p| *p >= 2).collect()),
            memo: Rc::default(),
            live: true,
            open: Vec::new(),
            tick: 0,
            joined: 0,
            last_arrived: 0,
        }
    }

    /// A path was taken off the work stack, which now holds `waiting` paths:
    /// the subtrees of the entries made when more were waiting are finished.
    fn close(&mut self, waiting: usize, outs: &[Outcome]) {
        if !self.live {
            return;
        }
        while let Some((mark, _)) = self.open.last() {
            if *mark <= waiting {
                break;
            }
            let Some((_, key)) = self.open.pop() else {
                break;
            };
            if let Some(e) = self.memo.borrow_mut().get_mut(&key) {
                e.outs_to = Some(outs.len());
                e.done_at = Some(self.tick);
                e.step_cut = outs[e.outs_from..]
                    .iter()
                    .any(|o| o.imprecise.iter().any(|i| i == STEP_CUT));
            }
        }
    }
}

const STEP_CUT: &str = "step budget exhausted";

impl Machine {
    /// The node the next step runs, taking the next forked path when the
    /// current one has ended.
    fn next_node(&mut self) -> Option<NodeId> {
        if self.cur.is_none() {
            self.cur = self.work.pop();
            if let Some(j) = self.join.as_mut() {
                j.close(self.work.len(), &self.outs);
            }
        }
        self.cur.as_ref().map(|(n, _)| *n)
    }
}

/// Checkpoints kept by an [`Explorer`]; when full, every other one is dropped.
const MAX_CHECKPOINTS: usize = 256;

struct Interp<'c> {
    cfg: &'c Cfg,
    keys: &'c Keys,
    prof: &'static Profile,
    sc: &'c Scenario,
}

/// The effect keys of a CFG and the caller's pseudo-effect, numbered in key
/// order: a state holds them in a vector and still lists them in the order
/// of a map keyed by name.
struct Keys {
    names: Rc<Vec<Rc<str>>>,
    ids: HashMap<Rc<str>, usize>,
}

impl Keys {
    fn of(cfg: &Cfg) -> Keys {
        Keys::in_universe(&KeyUniverse::of(&[cfg]))
    }

    fn in_universe(u: &KeyUniverse) -> Keys {
        let names = u.0.clone();
        let ids = names
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i))
            .collect();
        Keys { names, ids }
    }

    fn id(&self, key: &str) -> usize {
        self.ids[key]
    }
}

/// The effect keys of one or more CFGs, and the caller's pseudo-effect,
/// in key order. Explorers over the same universe number their effects the
/// same way, so that outcomes of two versions are compared slot by slot.
#[derive(Clone)]
pub struct KeyUniverse(Rc<Vec<Rc<str>>>);

impl KeyUniverse {
    pub(super) fn names(&self) -> &[Rc<str>] {
        &self.0
    }

    pub fn of(cfgs: &[&Cfg]) -> KeyUniverse {
        let mut all: BTreeSet<&str> = BTreeSet::from([CALLER]);
        for cfg in cfgs {
            for n in &cfg.nodes {
                if let Some(s) = n.stmt() {
                    all.extend(
                        s.effects
                            .iter()
                            .filter(|e| e.temp.is_none())
                            .map(|e| e.key.as_str()),
                    );
                }
            }
        }
        KeyUniverse(Rc::new(all.into_iter().map(Rc::from).collect()))
    }
}

/// Live effects formatted as the map from key to state they stand for.
struct LiveByName<'a>(&'a [Option<Live>], &'a [Rc<str>]);

impl std::fmt::Debug for LiveByName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(
                self.0
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| Some((&self.1[i], l.as_ref()?))),
            )
            .finish()
    }
}

/// Whether `cfg` has no node that forks a path (a test on a condition the
/// interpreter cannot evaluate, or on a row count a scenario does not fix):
/// every scenario then has exactly one path, with an empty signature. The
/// outcomes of the other side are paired with it by every combination, which
/// is what path joining needs (see [`Join`]).
pub fn decision_free(cfg: &Cfg) -> bool {
    !cfg.nodes.iter().any(|n| {
        matches!(
            n.op,
            Op::Test {
                test: Test::Unknown(_) | Test::NoRows(_),
                ..
            }
        )
    })
}

/// Explores every path of `cfg` under `sc` and returns one outcome per path.
pub fn explore(cfg: &Cfg, sc: &Scenario) -> Vec<Outcome> {
    let keys = Keys::of(cfg);
    let it = Interp {
        cfg,
        keys: &keys,
        prof: Profile::of(cfg.dialect),
        sc,
    };
    it.run()
}

/// Marks the text of a condition that is not proven to be the same fact on
/// both sides (perf plan W6c, E2): the marker names the side, so no text of
/// one side equals a text of the other and `pairs()`/`feasible()` match
/// nothing by it. Shown conditions go through [`plain_cond`].
pub(crate) fn opaque_cond(text: &str, side_a: bool) -> String {
    format!("\u{1}{}:{text}", if side_a { 'a' } else { 'b' })
}

/// A condition's text without the marker `opaque_cond` puts in front.
pub(crate) fn plain_cond(text: &str) -> &str {
    match text.strip_prefix('\u{1}') {
        Some(rest) => rest.get(2..).unwrap_or(rest),
        None => text,
    }
}

/// Runs fault scenarios without repeating the work before or after the fault.
///
/// A scenario that injects one fault runs exactly like the same scenario
/// without it until the exploration first reaches the faulting statement:
/// the scenario's faults are consulted only there. The explorer runs the
/// fault-free exploration once, remembers at which step each statement is
/// first reached and keeps checkpoints of the whole exploration (current
/// path, forked paths, outcomes so far). A fault scenario resumes from the
/// checkpoint before its statement, so its outcomes are the ones
/// [`explore`] returns, in the same order.
///
/// # State merging
///
/// The step function is deterministic and reads only the node and the
/// [`State`] of the path (and the scenario). Once the fault has been spent,
/// a path whose state equals that of a fault-free path at the same node
/// runs on exactly as that path did, so the replay stops there and takes
/// over the fault-free outcomes that descend from that path, changed by
/// what the fault left behind. A merge is made only when all of these hold,
/// and a replay that does not meet them runs to its end as before:
///
/// * same node, same `steps` (the step budget), the same number of
///   outcomes plus forked paths (the path budget), and the same *run
///   state* (everything the step function reads: [`State::same_run`]), but
///   for the slots of the failing statement's effects;
/// * those effects are final: no statement of theirs can run again, and no
///   pending-write log entry of the replay names them (nothing then reads
///   their slots). The fault-free path's log may hold entries for them (the
///   failing statement ran there and did not in the replay); the logs are
///   then equal without those entries, and the positions that savepoints
///   and scopes keep in the log are equal once counted without them. What
///   a later commit or rollback does to those entries is only the
///   fault-free side's slots, which the merge replaces. The one place the
///   step function reads the log's length, the summary of a loop with an
///   unknown condition ([`Interp::digest`]), would then differ by those
///   entries, so such a merge is refused when a loop that holds a commit,
///   rollback, raise or any other transaction action is reachable from the
///   node or from a statement the run will resume at;
/// * what the path recorded on the way (the *history*: sets of profile
///   rows, assumptions, temporary reads, partial writes) already holds what
///   the fault-free path had recorded, so the union of the replay's history
///   and the fault-free outcome's is exactly the replay's outcome's. Temporary
///   reads are the exception: a read the fault-free path made and the replay
///   did not (the failing statement's own) is let go when its statement can
///   not run again, so the fault-free outcome cannot make it again either.
///
/// What the step function does not read is history; it only reaches the
/// outcome, and [`Explorer::patch`] composes it exactly.
pub struct Explorer {
    keys: Keys,
    base: Scenario,
    outs: Vec<Outcome>,
    /// Step at which each statement is first about to run.
    first: HashMap<NodeId, usize>,
    /// (step, outcomes made by then, exploration before that step without its
    /// outcomes), in step order.
    checkpoints: Vec<(usize, usize, Machine)>,
    /// The fault-free run, tick by tick.
    ticks: Vec<Tick>,
    /// Ticks by (node, `steps`).
    at: HashMap<(NodeId, usize), Vec<u32>>,
    /// Strongly connected component of each node, numbered so that a node
    /// can reach another only from a component with a larger number.
    comp: Vec<u32>,
    /// Per node: whether a loop with an unknown condition that a commit,
    /// rollback or raise can sit in is reachable from it.
    unsafe_loop: Vec<bool>,
    /// Whether any step of the fault-free run starts in a doomed transaction:
    /// if none does, a doomed replay cannot merge into it.
    base_doomed: bool,
    /// The statements that have a durable effect with each key id.
    key_nodes: Vec<Vec<NodeId>>,
    merges: std::cell::Cell<usize>,
    joined: usize,
}

/// What the fault-free run looked like at one step.
#[derive(Clone, Copy)]
struct Tick {
    node: NodeId,
    steps: usize,
    /// Forked paths waiting, and outcomes made, when the step starts.
    work: u32,
    outs: u32,
    /// First later tick that starts with fewer forked paths waiting: the
    /// path's subtree of the exploration is the ticks up to it.
    end: u32,
    /// Outcomes made when that tick starts.
    outs_end: u32,
    /// Hash of the small part of the run state.
    small: u64,
    /// The path was dropped here by joining.
    dropped: bool,
}

impl Explorer {
    /// Runs `base` (which must inject no fault) and indexes the run.
    pub fn new(cfg: &Cfg, base: &Scenario) -> Explorer {
        Explorer::in_universe(cfg, base, &KeyUniverse::of(&[cfg]))
    }

    /// [`Explorer::new`] numbering the effects over `keys`, which must hold
    /// every effect key of `cfg`.
    pub fn in_universe(cfg: &Cfg, base: &Scenario, keys: &KeyUniverse) -> Explorer {
        Explorer::in_universe_joined(cfg, base, keys, false)
    }

    /// [`Explorer::in_universe`] joining paths of the fault-free run when
    /// `join` (see [`Join`]). Only sound where the other side of the
    /// comparison makes no decision: see [`decision_free`].
    pub fn in_universe_joined(
        cfg: &Cfg,
        base: &Scenario,
        keys: &KeyUniverse,
        join: bool,
    ) -> Explorer {
        let base = Scenario {
            faults: Vec::new(),
            ..base.clone()
        };
        let keys = Keys::in_universe(keys);
        let it = Interp {
            cfg,
            keys: &keys,
            prof: Profile::of(cfg.dialect),
            sc: &base,
        };
        let mut m = it.start();
        if join {
            m.join = Some(Join::new(cfg));
        }
        let mut first = HashMap::new();
        let mut checkpoints = Vec::new();
        let mut ticks: Vec<Tick> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        let mut every = 1;
        let mut t = 0;
        let mut base_doomed = false;
        while let Some(n) = m.next_node() {
            if t % every == 0 {
                if checkpoints.len() == MAX_CHECKPOINTS {
                    let mut i = 0;
                    checkpoints.retain(|_| {
                        i += 1;
                        i % 2 == 1
                    });
                    every *= 2;
                }
                if t % every == 0 {
                    // Outcomes are append-only, so the checkpoint keeps
                    // only how many exist; `machine_at` restores them from
                    // `outs` (copies in every checkpoint cost the memory of
                    // all of them 256 times over).
                    checkpoints.push((
                        t,
                        m.outs.len(),
                        Machine {
                            cur: m.cur.clone(),
                            work: m.work.clone(),
                            outs: Vec::new(),
                            join: m.join.clone(),
                        },
                    ));
                }
            }
            first.entry(n).or_insert(t);
            let (steps, small) = match &m.cur {
                Some((_, st)) => {
                    base_doomed |= st.doomed;
                    (st.steps, small_hash(st))
                }
                None => (0, 0),
            };
            let work = m.work.len() as u32;
            while let Some(&top) = open.last() {
                if ticks[top].work > work {
                    ticks[top].end = t as u32;
                    ticks[top].outs_end = m.outs.len() as u32;
                    open.pop();
                } else {
                    break;
                }
            }
            open.push(t);
            let joined_before = m.join.as_ref().map_or(0, |j| j.joined);
            ticks.push(Tick {
                node: n,
                steps,
                work,
                outs: m.outs.len() as u32,
                end: 0,
                outs_end: 0,
                small,
                dropped: false,
            });
            it.tick(&mut m);
            // A path dropped by joining ran nothing: a replay must not merge
            // into it (its outcomes are the joined path's).
            // Every open tick after the joined entry's arrival is
            // on the dropped path's own segment (not an ancestor of the kept
            // path): its outcome slice lacks the dropped continuation, so a
            // replay must not merge into it either.
            if let Some(j) = m.join.as_ref().filter(|j| j.joined > joined_before) {
                for &o in open.iter().rev() {
                    if o <= j.last_arrived {
                        break;
                    }
                    ticks[o].dropped = true;
                }
                ticks[t].dropped = true;
            }
            t += 1;
        }
        for top in open {
            ticks[top].end = t as u32;
            ticks[top].outs_end = m.outs.len() as u32;
        }
        let mut at: HashMap<(NodeId, usize), Vec<u32>> = HashMap::new();
        for (i, k) in ticks.iter().enumerate() {
            if k.dropped {
                continue;
            }
            at.entry((k.node, k.steps)).or_default().push(i as u32);
        }
        let mut key_nodes = vec![Vec::new(); keys.names.len()];
        for (i, n) in cfg.nodes.iter().enumerate() {
            if let Some(s) = n.stmt() {
                for e in s.effects.iter().filter(|e| e.temp.is_none()) {
                    let v: &mut Vec<NodeId> = &mut key_nodes[keys.id(&e.key)];
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
            }
        }
        Explorer {
            unsafe_loop: unsafe_loops(cfg),
            base_doomed,
            comp: components(cfg),
            keys,
            base,
            outs: m.outs,
            first,
            checkpoints,
            ticks,
            at,
            key_nodes,
            merges: std::cell::Cell::new(0),
            joined: m.join.as_ref().map_or(0, |j| j.joined),
        }
    }

    /// The fault-free scenario this explorer ran.
    pub fn base_scenario(&self) -> &Scenario {
        &self.base
    }

    /// The outcomes of the fault-free scenario.
    pub fn base(&self) -> &[Outcome] {
        &self.outs
    }

    /// How many paths of the fault-free run joining dropped.
    pub fn joined(&self) -> usize {
        self.joined
    }

    /// Whether the fault-free run reaches `node` (a statement it dropped
    /// with a joined path is still reached by the path it joined).
    pub fn reaches(&self, node: NodeId) -> bool {
        self.first.contains_key(&node)
    }

    /// How many fault scenarios ended by merging into the fault-free run.
    pub fn merges(&self) -> usize {
        self.merges.get()
    }

    /// Every path of `cfg` under `sc`, from the start.
    fn full(&self, cfg: &Cfg, sc: &Scenario) -> Vec<Outcome> {
        let it = Interp {
            cfg,
            keys: &self.keys,
            prof: Profile::of(cfg.dialect),
            sc,
        };
        it.run()
    }

    /// The fault-free exploration just before tick `t`.
    fn machine_at(&self, cfg: &Cfg, t: usize) -> Option<Machine> {
        let (mut at, made, m) = self.checkpoints.iter().rev().find(|(c, _, _)| *c <= t)?;
        let mut m = m.clone();
        m.outs = self.outs[..*made].to_vec();
        let base = Interp {
            cfg,
            keys: &self.keys,
            prof: Profile::of(cfg.dialect),
            sc: &self.base,
        };
        // The memo is complete: the re-run only reads it.
        if let Some(j) = m.join.as_mut() {
            j.live = false;
            j.open.clear();
        }
        // Re-running from the checkpoint is not counted: how far apart the
        // checkpoints are is not part of the analysis, and the ticks before
        // `t` were counted when the fault-free run was made. A fault scenario
        // is charged what it runs itself, from `t` on.
        sqldiff_model::work::muted(|| {
            while at < t {
                m.next_node()?;
                base.tick(&mut m);
                at += 1;
            }
            Some(())
        })?;
        Some(m)
    }

    /// [`explore`] of `sc`, reusing the fault-free run when `sc` differs from
    /// the base scenario only by at most one injected fault.
    pub fn explore(&self, cfg: &Cfg, sc: &Scenario) -> Vec<Outcome> {
        let same_env = Scenario {
            faults: Vec::new(),
            ..sc.clone()
        } == self.base;
        if !same_env || sc.faults.len() > 1 {
            return self.full(cfg, sc);
        }
        let Some(fault) = sc.faults.first() else {
            return self.outs.clone();
        };
        // Never reached: the run is the fault-free one.
        let Some(&t) = self.first.get(&fault.node) else {
            return self.outs.clone();
        };
        let Some(mut m) = self.machine_at(cfg, t) else {
            return self.full(cfg, sc);
        };
        if m.next_node() != Some(fault.node) {
            return self.full(cfg, sc);
        }
        // The replay runs every path to its end: what joining dropped from the
        // fault-free run cannot reach the failing statement (it is first
        // reached at tick `t`), so those paths are as in the fault-free run.
        m.join = None;
        let it = Interp {
            cfg,
            keys: &self.keys,
            prof: Profile::of(cfg.dialect),
            sc,
        };
        self.replay(cfg, &it, fault, t, m)
    }

    /// Runs the fault scenario from `m` (the fault-free exploration at tick
    /// `t`, about to run the failing statement), stopping early where it
    /// merges into the fault-free run.
    fn replay(
        &self,
        cfg: &Cfg,
        it: &Interp,
        fault: &Fault,
        t: usize,
        mut m: Machine,
    ) -> Vec<Outcome> {
        // Effects of the failing statement: the slots that differ from the
        // fault-free run for good.
        let spent: Vec<usize> = cfg
            .node(fault.node)
            .stmt()
            .map(|s| {
                let mut v: Vec<usize> = s
                    .effects
                    .iter()
                    .filter(|e| e.temp.is_none())
                    .map(|e| self.keys.id(&e.key))
                    .collect();
                v.sort_unstable();
                v.dedup();
                v
            })
            .unwrap_or_default();
        let mut after: Vec<NodeId> = vec![fault.node];
        for &k in &spent {
            after.extend(&self.key_nodes[k]);
        }
        let mut cursor: Option<(usize, Machine)> = None;
        let mut tries = 0;
        while m.next_node().is_some() {
            let doomed = m.cur.as_ref().is_some_and(|(_, p)| p.doomed);
            if tries < MAX_MERGE_TRIES && (self.base_doomed || !doomed) {
                if let Some(merged) = self.try_merge(
                    cfg,
                    it,
                    fault,
                    t,
                    &m,
                    &spent,
                    &after,
                    &mut cursor,
                    &mut tries,
                ) {
                    m.outs.extend(merged);
                    m.cur = None;
                    self.merges.set(self.merges.get() + 1);
                    continue;
                }
            }
            it.tick(&mut m);
        }
        m.outs
    }

    /// The outcomes that replace the rest of the replay `m` if its current
    /// path merges into the fault-free run here.
    #[allow(clippy::too_many_arguments)]
    fn try_merge(
        &self,
        cfg: &Cfg,
        it: &Interp,
        fault: &Fault,
        t: usize,
        m: &Machine,
        spent: &[usize],
        after: &[NodeId],
        cursor: &mut Option<(usize, Machine)>,
        tries: &mut usize,
    ) -> Option<Vec<Outcome>> {
        let (node, p) = m.cur.as_ref()?;
        let p: &State = p;
        // The fault is spent and its route is known.
        if !p.faults_done.contains(&fault.node)
            || p.faults_done.len() != 1
            || p.fault_index.is_none()
            || p.fault_route.is_none()
            || p.before_fault.is_none()
        {
            return None;
        }
        // Nothing of the failing statement can run again, from here or from
        // a handler resuming a path.
        let c = |n: NodeId| self.comp[n];
        let gone = |from: NodeId| after.iter().all(|&a| c(from) < c(a));
        if !gone(*node) || !p.resume.iter().all(|&r| gone(r)) {
            return None;
        }
        // A statement that cannot run again from here: what it read before
        // cannot be read again.
        let unreachable = |n: NodeId| c(*node) < c(n) && p.resume.iter().all(|&r| c(r) < c(n));
        let small = small_hash(p);
        let sum = (m.work.len() + m.outs.len()) as u32;
        let cands = self.at.get(&(*node, p.steps))?;
        for &u in cands {
            let k = self.ticks[u as usize];
            if k.small != small || k.work + k.outs != sum || (u as usize) < t {
                continue;
            }
            *tries += 1;
            // The fault-free run at tick u.
            let reuse = matches!(cursor, Some((cu, _)) if *cu <= u as usize);
            if !reuse {
                *cursor = Some((t, self.machine_at(cfg, t)?));
                if let Some((_, cm)) = cursor.as_mut() {
                    cm.next_node()?;
                }
            }
            let (cu, cm) = cursor.as_mut()?;
            let base = Interp {
                cfg,
                keys: &self.keys,
                prof: Profile::of(cfg.dialect),
                sc: &self.base,
            };
            while *cu < u as usize {
                cm.next_node()?;
                base.tick(cm);
                *cu += 1;
            }
            cm.next_node()?;
            let (bn, b) = cm.cur.as_ref()?;
            if bn != node || !p.history_covers(b, &unreachable) {
                continue;
            }
            match p.same_run(b, spent) {
                // A loop's summary compares the log's length, which counts
                // the failing statement's entries on the fault-free side only.
                // The run may also resume at a statement a handler body has no
                // edge to (`HandlerDone { resume }`): the loops reachable from
                // there count too.
                Some(shifted)
                    if !(shifted
                        && (self.unsafe_loop[*node]
                            || p.resume.iter().any(|&r| self.unsafe_loop[r]))) => {}
                _ => continue,
            }
            let outs = &self.outs[k.outs as usize..k.outs_end as usize];
            return Some(self.patch(it, outs, p, b, spent));
        }
        None
    }

    /// The fault-free outcomes `outs` of the path at `b`, as the replay path
    /// `p` (which has merged into it) ends them.
    fn patch(
        &self,
        it: &Interp,
        outs: &[Outcome],
        p: &State,
        b: &State,
        spent: &[usize],
    ) -> Vec<Outcome> {
        let head = Trace::new(p.trace.clone());
        let before = p.before_fault.clone().unwrap_or_default();
        outs.iter()
            .map(|o| {
                let raised_exit = matches!(o.exit, Exit::Raised(_));
                let mut partial = p.partial.clone();
                partial.extend(o.partial.iter().cloned());
                // Fates that change: the failing statement's effects, and
                // effects the replay's history marks as partial.
                let mut over: BTreeMap<usize, Option<Fate>> = BTreeMap::new();
                let mut open: BTreeMap<usize, bool> = BTreeMap::new();
                let mut unit_commit = o.unit_commit;
                for &k in spent {
                    let name = &*self.keys.names[k];
                    if p.owner == Owner::UnitOfWork
                        && !raised_exit
                        && o.open.contains(name)
                        && o.effects.contains_key(name)
                    {
                        unit_commit -= 1;
                    }
                    match p.live[k] {
                        None => {
                            over.insert(k, None);
                            open.insert(k, false);
                        }
                        Some(l) => {
                            let (fate, opened, committed) =
                                it.fate_of(l, p.owner, o.doomed, raised_exit);
                            let fate = if fate == Fate::Durable && partial.contains(name) {
                                Fate::Partial
                            } else {
                                fate
                            };
                            over.insert(k, Some(fate));
                            open.insert(k, opened);
                            unit_commit += usize::from(committed);
                        }
                    }
                }
                for k in &partial {
                    if let Some(i) = self.keys.ids.get(k.as_str()) {
                        if o.effects.get(k) == Some(&Fate::Durable) {
                            over.entry(*i).or_insert(Some(Fate::Partial));
                        }
                    }
                }
                let mut assumptions = p.assumptions.clone();
                assumptions.extend(o.assumptions.iter().cloned());
                if o.unit_commit > 0 && unit_commit == 0 {
                    assumptions.remove("ASM-CALLER-COMMITS");
                } else if unit_commit > 0 {
                    assumptions.insert("ASM-CALLER-COMMITS".into());
                }
                let mut rows = p.rows.clone();
                rows.extend(o.rows.iter().cloned());
                let mut untested = p.untested.clone();
                untested.extend(o.untested.iter().cloned());
                // Reads the fault-free path had made that this replay did not
                // were made at statements that cannot run again (see
                // `history_covers`), so they are not in `o` for a later read.
                let mut temp_reads = p.temp_reads.clone();
                temp_reads.extend(
                    o.temp_reads
                        .iter()
                        .filter(|r| !b.temp_reads.contains(*r))
                        .cloned(),
                );
                let mut signature = p.signature.clone();
                signature.extend(o.signature[b.signature.len()..].iter().cloned());
                let mut imprecise = p.imprecise.clone();
                imprecise.extend(o.imprecise[b.imprecise.len()..].iter().cloned());
                let mut cut = p.cut.clone();
                cut.extend(o.cut[b.cut.len()..].iter().copied());
                Outcome {
                    exit: o.exit.clone(),
                    trace: head.clone().joined(&o.trace, b.trace.len()),
                    fault_index: p.fault_index,
                    fault_route: p.fault_route.clone(),
                    effects: o.effects.with(over),
                    open: o.open.with(open),
                    partial,
                    before_fault: before.clone(),
                    faulted: p.faulted.clone(),
                    fault_hit: !p.faults_done.is_empty(),
                    temp_reads,
                    raised: p.raised || o.raised,
                    depth: o.depth,
                    left_open: o.left_open,
                    doomed: o.doomed,
                    signature,
                    imprecise,
                    cut,
                    durability_unknown: o.durability_unknown.clone(),
                    rows,
                    assumptions,
                    untested,
                    unit_commit,
                }
            })
            .collect()
    }
}

/// Merge attempts a fault replay makes at most: each can rebuild a state of
/// the fault-free run.
const MAX_MERGE_TRIES: usize = 16;

/// 64-bit FNV-1a as a `Hasher`: the same value on every build, platform and
/// Rust version, unlike `DefaultHasher`, whose algorithm is not promised.
struct Fnv64(u64);

impl std::hash::Hasher for Fnv64 {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Hash of the small part of the run state, to find the fault-free steps a
/// path may merge into without comparing whole states.
fn small_hash(st: &State) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = Fnv64(0xcbf2_9ce4_8422_2325);
    (
        st.depth,
        st.caller_depth,
        st.owner as u8,
        st.no_commit,
        st.doomed,
        st.xact_abort,
        st.xact_abort_local,
        st.implicit,
        st.scopes.len(),
    )
        .hash(&mut h);
    (
        st.savepoints.len(),
        st.resume.len(),
        st.caught.len(),
        st.last_error,
        st.no_rows,
        st.entry_depth,
        st.ret.is_some(),
        st.raising,
        st.temp.len(),
    )
        .hash(&mut h);
    h.finish()
}

/// Strongly connected components of the control-flow graph, numbered in
/// the order Tarjan's algorithm completes them: every edge leads to a
/// component with a smaller number or to the same one.
fn successors(cfg: &Cfg, v: NodeId) -> Vec<NodeId> {
    let node = cfg.node(v);
    let mut out: Vec<NodeId> = node.succ.iter().map(|(_, t)| *t).collect();
    for (_, r) in &node.routes {
        out.push(r.resume);
        if let Some(h) = &r.handler {
            out.push(h.entry);
        }
    }
    out
}

/// For each node, whether it can reach (or is in) a loop whose condition the
/// interpreter cannot evaluate and whose cycle holds anything but plain
/// statements: only a statement with no transaction action leaves the log
/// of a path as it was, entry for entry, whatever happens in the loop.
///
/// This also rests on the profile rows, not on the graph: a plain statement
/// can only fail after the fault in a doomed transaction, and no row for
/// that failure undoes anything. A row that rolled back a scope or the
/// whole transaction for such a failure would break the guard.
fn unsafe_loops(cfg: &Cfg) -> Vec<bool> {
    let comp = components(cfg);
    let ncomp = comp.iter().max().map_or(0, |m| *m as usize + 1);
    let mut bad = vec![false; ncomp];
    let mut has_test = vec![false; ncomp];
    for (v, n) in cfg.nodes.iter().enumerate() {
        let c = comp[v] as usize;
        match &n.op {
            Op::Test { is_loop: true, .. } => has_test[c] = true,
            Op::Stmt(s) if s.action != Action::None => bad[c] = true,
            Op::Stmt(_) => {}
            // Scope ends and handler returns also run when an error leaves.
            Op::Leave(_) | Op::Enter(_) | Op::Test { .. } | Op::Entry | Op::Nop => {}
            _ => bad[c] = true,
        }
    }
    let mut reach: Vec<bool> = (0..ncomp).map(|c| has_test[c] && bad[c]).collect();
    // Components are numbered so that edges go to smaller numbers.
    let mut by_comp: Vec<Vec<NodeId>> = vec![Vec::new(); ncomp];
    for v in 0..cfg.nodes.len() {
        by_comp[comp[v] as usize].push(v);
    }
    for c in 0..ncomp {
        if reach[c] {
            continue;
        }
        reach[c] = by_comp[c].iter().any(|&v| {
            successors(cfg, v)
                .iter()
                .any(|&w| comp[w] as usize != c && reach[comp[w] as usize])
        });
    }
    (0..cfg.nodes.len())
        .map(|v| reach[comp[v] as usize])
        .collect()
}

fn components(cfg: &Cfg) -> Vec<u32> {
    let n = cfg.nodes.len();
    let succ = |v: NodeId| -> Vec<NodeId> { successors(cfg, v) };
    const NONE: u32 = u32::MAX;
    let mut index = vec![NONE; n];
    let mut low = vec![0u32; n];
    let mut on = vec![false; n];
    let mut comp = vec![NONE; n];
    let mut stack: Vec<NodeId> = Vec::new();
    let (mut next, mut ncomp) = (0u32, 0u32);
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        let mut call: Vec<(NodeId, Vec<NodeId>, usize)> = vec![(root, succ(root), 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on[root] = true;
        while let Some((v, es, i)) = call.last_mut() {
            let v = *v;
            if *i < es.len() {
                let w = es[*i];
                *i += 1;
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on[w] = true;
                    call.push((w, succ(w), 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some((u, _, _)) = call.last() {
                    low[*u] = low[*u].min(low[v]);
                }
                if low[v] == index[v] {
                    while let Some(w) = stack.pop() {
                        on[w] = false;
                        comp[w] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
            }
        }
    }
    comp
}

/// Id of the assumption recorded when a profile row that is not confirmed is used.
pub fn row_assumption_id(fact: Option<&str>, row: &str) -> String {
    match fact {
        Some(f) => format!("ASM-FACT-{f}"),
        None => format!("ASM-ROW-{row}"),
    }
}

impl Interp<'_> {
    fn run(&self) -> Vec<Outcome> {
        self.finish_run(self.start())
    }

    fn start(&self) -> Machine {
        let init = self.initial();
        let first = self
            .cfg
            .node(self.cfg.entry)
            .next(Edge::Next)
            .unwrap_or(EXIT_OK);
        Machine {
            cur: None,
            work: vec![(first, Box::new(init))],
            outs: Vec::new(),
            join: None,
        }
    }

    fn finish_run(&self, mut m: Machine) -> Vec<Outcome> {
        while m.next_node().is_some() {
            self.tick(&mut m);
        }
        m.outs
    }

    /// One step of the current path (see [`Machine::next_node`]).
    fn tick(&self, m: &mut Machine) {
        self.tick_path(m);
        if let Some(j) = m.join.as_mut() {
            j.tick += 1;
        }
    }

    fn tick_path(&self, m: &mut Machine) {
        let Some((node, mut st)) = m.cur.take() else {
            return;
        };
        if m.join.is_some() && Self::joins(m, node, &st) {
            return;
        }
        st.steps += 1;
        sqldiff_model::work::spend(sqldiff_model::work::Site::ExplorerSteps, 1);
        if st.steps > MAX_STEPS {
            st.imprecise.push(STEP_CUT.into());
            st.cut.push(node);
            m.outs.push(self.finish(Exit::Normal, *st));
            return;
        }
        // Whether a decision's second branch would be kept (see below).
        let over = m.join.as_ref().is_some_and(|j| j.tick >= JOIN_TICKS);
        let room = m.work.len() + m.outs.len() + 1 < MAX_PATHS && !over;
        match self.step_in(node, st, room) {
            Step::Go(n, s) => m.cur = Some((n, s)),
            Step::Pruned(n, mut s, dropped) => {
                s.imprecise.push("path budget exhausted".into());
                s.cut.push(dropped);
                m.cur = Some((n, s));
            }
            Step::Fork(mut v) => {
                let (n, mut s) = v.remove(0);
                // Paths dropped by joining are not outcomes, so the path budget
                // alone does not bound the work of a joined run: its ticks are
                // bounded the same way (work budget). The memo's size is not
                // read here: a re-run from a checkpoint sees the finished
                // memo, so a size test would stop forking earlier than the
                // run that made it (`entries are capped where they are made`).
                let over = m.join.as_ref().is_some_and(|j| j.tick >= JOIN_TICKS);
                if m.work.len() + m.outs.len() + v.len() < MAX_PATHS && !over {
                    m.work.extend(v);
                } else {
                    s.imprecise.push("path budget exhausted".into());
                    s.cut.extend(v.iter().map(|(n, _)| *n));
                }
                m.cur = Some((n, s));
            }
            Step::Done(o) => m.outs.push(*o),
        }
    }

    /// Path joining: whether the path about to run `node` in state `st` is
    /// dropped because an earlier path in the same state has finished
    /// everything below `node` (see [`Join`]). Otherwise the arrival is
    /// remembered (when the memo is being made).
    fn joins(m: &mut Machine, node: NodeId, st: &State) -> bool {
        let Machine {
            work, outs, join, ..
        } = m;
        let Some(j) = join.as_mut() else {
            return false;
        };
        if !j.points[node] {
            return false;
        }
        // Fault bookkeeping is part of what an outcome says; joined runs are
        // fault-free.
        if !st.faults_done.is_empty()
            || st.fault_index.is_some()
            || st.fault_route.is_some()
            || st.before_fault.is_some()
            || st.faulted.is_some()
        {
            return false;
        }
        let text = st.run_key();
        // A state this big (a long script's log) is not worth a memo entry.
        if text.len() > JOIN_KEY_BYTES {
            return false;
        }
        let key = (node, Rc::<str>::from(text));
        let mut memo = j.memo.borrow_mut();
        let Some(e) = memo.get(&key) else {
            // Entries are capped where they are made, never where they are
            // read: a full memo only stops new entries, which every re-run
            // (it makes none) sees as absent before their arrival too.
            if j.live && memo.len() < JOIN_ENTRIES {
                memo.insert(
                    key.clone(),
                    JoinEntry {
                        arrived: j.tick,
                        steps: st.steps,
                        outs_from: outs.len(),
                        outs_to: None,
                        done_at: None,
                        step_cut: false,
                    },
                );
                drop(memo);
                j.open.push((work.len(), key));
            }
            return false;
        };
        // Still running (this path is inside the earlier one's subtree), or
        // this is the arrival that made the entry.
        let (Some(done), Some(to)) = (e.done_at, e.outs_to) else {
            return false;
        };
        if done > j.tick || e.arrived >= j.tick || e.outs_from == to {
            return false;
        }
        // The earlier path had used at most as many steps, or its subtree was
        // not cut by the step budget: this path's continuation fits the budget
        // wherever the earlier one did.
        if st.steps < e.steps && e.step_cut {
            return false;
        }
        let arrived = e.arrived;
        j.joined += 1;
        j.last_arrived = arrived;
        // A drop is one event. The unjoined cost of the dropped subtree is
        // not charged: it is exponential in the number of joins (the reason
        // for joining), and the engine never does that work.
        sqldiff_model::work::spend(sqldiff_model::work::Site::ExplorerSteps, 1);
        true
    }

    fn row(&self, st: &mut State, id: &str, fact: Option<&str>, status: Status) {
        // A row already noted has its assumption and untested entries too:
        // they follow from the row's id, fact and status alone.
        if st.rows.contains(id) {
            return;
        }
        st.rows.insert(id.to_string());
        if status != Status::Confirmed {
            st.assumptions.insert(row_assumption_id(fact, id));
        }
        if status == Status::Unverified {
            st.untested.insert(match fact {
                Some(f) => format!("vendor fact {f} ({id})"),
                None => format!("profile row {id}"),
            });
        }
    }

    fn initial(&self) -> State {
        let mode = self.prof.routine_mode(self.cfg.routine.key());
        let sc = self.sc;
        // An error inside a T-SQL trigger aborts the batch and dooms the
        // transaction whatever the caller set: the body runs as XACT_ABORT ON
        // until it sets the option itself.
        let trigger = self.cfg.dialect == Dialect::Tsql && self.cfg.routine == RoutineKind::Trigger;
        let mut st = State {
            depth: 0,
            caller_depth: 0,
            owner: Owner::Autocommit,
            no_commit: false,
            doomed: false,
            xact_abort: sc.xact_abort || trigger,
            xact_abort_local: trigger,
            implicit: sc.implicit_transactions,
            live: vec![None; self.keys.names.len()],
            log: Vec::new(),
            temp: BTreeMap::new(),
            scopes: Vec::new(),
            savepoints: Vec::new(),
            resume: Vec::new(),
            caught: Vec::new(),
            last_error: None,
            no_rows: None,
            rows_stmt: None,
            rows_failed: false,
            partial: BTreeSet::new(),
            entry_depth: 0,
            faults_done: BTreeSet::new(),
            before_fault: None,
            faulted: None,
            temp_reads: BTreeSet::new(),
            raised: false,
            ret: None,
            raising: None,
            loop_seen: HashMap::new(),
            signature: Vec::new(),
            imprecise: Vec::new(),
            cut: Vec::new(),
            durability_unknown: None,
            rows: BTreeSet::new(),
            assumptions: BTreeSet::new(),
            steps: 0,
            trace: Vec::new(),
            fault_index: None,
            fault_route: None,
            untested: BTreeSet::new(),
        };
        let key = format!(
            "{}.transactions.routine_mode.{}",
            self.cfg.dialect.id(),
            self.cfg.routine.key()
        );
        if let Some(r) = self
            .prof
            .transactions
            .routine_mode
            .get(self.cfg.routine.key())
        {
            self.row(&mut st, &key, r.fact.as_deref(), r.status);
        }
        match mode {
            "caller" => {
                st.depth = 1;
                st.caller_depth = 1;
                st.owner = if sc.caller_txn {
                    Owner::CallerTxn
                } else {
                    Owner::CallerStatement
                };
            }
            "call" => {
                st.depth = 1;
                st.caller_depth = u32::from(sc.caller_txn);
                st.owner = if sc.caller_txn {
                    Owner::CallerTxn
                } else {
                    Owner::Call
                };
            }
            "commit_level" => {
                let level = self
                    .cfg
                    .commit_level
                    .clone()
                    .or_else(|| sc.commit_level.clone());
                if self.cfg.commit_level.is_none() && level.is_some() {
                    // Declared in configuration, not stated by the routine.
                    st.assumptions.insert("ASM-DB2-COMMIT-LEVEL".into());
                }
                match &level {
                    Some(l) if self.prof.is_no_commit_level(l) => {
                        st.no_commit = true;
                        let r = &self.prof.transactions.no_commit_levels;
                        self.row(
                            &mut st,
                            "db2i.transactions.no_commit_levels",
                            r.fact.as_deref(),
                            r.status,
                        );
                    }
                    Some(_) => {}
                    None => {
                        st.assumptions.insert("ASM-DB2-COMMIT-LEVEL".into());
                        st.durability_unknown = Some(
                            "the commit level is not declared and depends on how the routine was created (fact V5)".into(),
                        );
                    }
                }
                if !st.no_commit {
                    st.depth = 1;
                    st.caller_depth = 1;
                    st.owner = if sc.caller_txn {
                        Owner::CallerTxn
                    } else {
                        Owner::UnitOfWork
                    };
                }
            }
            _ => {
                st.depth = u32::from(sc.caller_txn);
                st.caller_depth = st.depth;
                st.owner = if sc.caller_txn {
                    Owner::CallerTxn
                } else {
                    Owner::Autocommit
                };
            }
        }
        if !sc.caller_txn && self.cfg.routine != RoutineKind::Script {
            st.assumptions.insert("ASM-NO-CALLER-TXN".into());
        }
        if sc.caller_txn && st.depth > 0 {
            // The caller's own uncommitted work, so its fate can be followed.
            let c = self.keys.id(CALLER);
            st.live[c] = Some(Live::Pending);
            st.log.push(Log::Effect(c));
        }
        st.entry_depth = st.depth;
        st
    }

    fn situation(&self, st: &State) -> Situation {
        Situation {
            xact_abort: st.xact_abort,
            no_commit: st.no_commit,
            in_txn: st.depth > 0,
            context: if self.cfg.routine == RoutineKind::Script {
                Context::Script
            } else {
                Context::Routine
            },
            caller_txn: self.sc.caller_txn,
        }
    }

    fn next(&self, node: NodeId, st: Box<State>) -> Step {
        match self.cfg.node(node).next(Edge::Next) {
            Some(n) => Step::Go(n, st),
            None => Step::Done(Box::new(self.finish(Exit::Normal, *st))),
        }
    }

    fn step(&self, id: NodeId, st: Box<State>) -> Step {
        self.step_in(id, st, true)
    }

    /// One step of `st` at `id`. `room`: a decision's second branch has a
    /// place to wait; without one it is dropped, so it is not made.
    fn step_in(&self, id: NodeId, mut st: Box<State>, room: bool) -> Step {
        let node = self.cfg.node(id);
        match &node.op {
            Op::Entry | Op::Nop => self.next(id, st),
            Op::ExitOk => {
                let exit = match st.ret.clone() {
                    Some(c) if c != "0" => Exit::Code(c),
                    _ => Exit::Normal,
                };
                Step::Done(Box::new(self.finish(exit, *st)))
            }
            Op::ExitErr => {
                let k = st.raising.unwrap_or(ErrorKind::Other);
                Step::Done(Box::new(self.finish(Exit::Raised(k), *st)))
            }
            Op::Enter(s) => {
                let m = st.log.len();
                st.scopes.push((*s, m));
                self.next(id, st)
            }
            Op::Leave(s) => {
                if let Some(i) = st.scopes.iter().rposition(|(x, _)| x == s) {
                    st.scopes.truncate(i);
                }
                self.next(id, st)
            }
            Op::HandlerDone { resume } => {
                st.caught.pop();
                if *resume {
                    match st.resume.pop() {
                        Some(n) => Step::Go(n, st),
                        None => self.next(id, st),
                    }
                } else {
                    self.next(id, st)
                }
            }
            Op::Test { test, is_loop } => {
                let known = match test {
                    Test::ErrorStatus(b) => {
                        Some(st.last_error.is_some_and(|k| !self.prof.is_warning(k)) == *b)
                    }
                    Test::NoRows(b) => st.no_rows.map(|x| x == *b),
                    Test::TranCount(b) => Some((st.depth > 0) == *b),
                    Test::XactStateNonZero(b) => Some((st.depth > 0) == *b),
                    Test::XactStateDoomed => Some(st.doomed),
                    Test::XactStateCommittable => Some(st.depth > 0 && !st.doomed),
                    Test::Unknown(_) => None,
                };
                let t = node.next(Edge::True).unwrap_or(EXIT_OK);
                let f = node.next(Edge::False).unwrap_or(EXIT_OK);
                if let Some(b) = known {
                    return Step::Go(if b { t } else { f }, st);
                }
                // A row-count test is named by the statement it reads and
                // recorded as "no rows from it", so the two sides' tests
                // (`@@ROWCOUNT = 0`, `NOT FOUND`, `FOUND` on the other
                // branch) agree on what the value means.
                let (text, tv) = match test {
                    Test::Unknown(x) => (x.clone(), true),
                    Test::NoRows(b) => (self.rows_text(&st), *b),
                    _ => (String::new(), true),
                };
                if *is_loop {
                    let digest = self.digest(&st);
                    let seen = st.loop_seen.entry(id).or_default();
                    seen.push(digest);
                    let n = seen.len();
                    if n > 2 {
                        if seen[n - 1] != seen[n - 2] {
                            st.imprecise.push(format!(
                                "loop `{}` summarized after two iterations",
                                plain_cond(&text)
                            ));
                        }
                        st.loop_seen.remove(&id);
                        return Step::Go(f, st);
                    }
                }
                if !room {
                    st.signature.push((text, tv));
                    return Step::Pruned(t, st, f);
                }
                let mut a = st.clone();
                a.signature.push((text.clone(), tv));
                st.signature.push((text, !tv));
                Step::Fork(vec![(t, a), (f, st)])
            }
            Op::Stmt(_) => self.exec(id, st),
        }
    }

    /// "no rows from <statement>": the statement a row-count test reads.
    fn rows_text(&self, st: &State) -> String {
        let what = match st.rows_stmt.and_then(|n| self.cfg.node(n).stmt()) {
            Some(s) => match s.effects.first() {
                Some(e) => e.key.clone(),
                None => format!("{:?}", s.kind),
            },
            None => "the start".to_string(),
        };
        if st.rows_failed {
            // Dialect-specific after an error, so the dialect is part of the
            // identity: the two sides' tests are not the same test.
            format!("no rows after failed {what} ({})", self.cfg.dialect.id())
        } else {
            format!("no rows from {what}")
        }
    }

    fn digest(&self, st: &State) -> String {
        format!(
            "{}|{}|{:?}|{:?}|{}",
            st.depth,
            st.doomed,
            LiveByName(&st.live, &self.keys.names),
            st.temp,
            st.log.len()
        )
    }

    fn exec(&self, id: NodeId, mut st: Box<State>) -> Step {
        let node = self.cfg.node(id);
        let s = node.stmt().expect("statement node");
        let d = self.cfg.dialect;
        st.last_error = None;
        st.trace.push(id);
        // FOUND is set only by queries and DML; RAISE NOTICE and the like
        // leave it alone.
        if d != Dialect::Postgres || s.category.is_some() || s.kind == StmtKind::Select {
            st.no_rows = None;
            st.rows_stmt = Some(id);
            st.rows_failed = false;
        }
        // IMPLICIT_TRANSACTIONS opens the transaction when DML starts, even if
        // it then touches no rows or fails.
        if d == Dialect::Tsql
            && st.implicit
            && st.depth == 0
            && !st.no_commit
            && s.effects.iter().any(|e| e.temp.is_none())
        {
            st.depth = 1;
        }

        // Injected fault.
        if !st.faults_done.contains(&id) {
            if let Some(f) = self.sc.faults.iter().find(|f| f.node == id) {
                st.faults_done.insert(id);
                if st.fault_index.is_none() {
                    st.fault_index = Some(st.trace.len() - 1);
                }
                if st.before_fault.is_none() {
                    st.before_fault = Some(BeforeFault::new(
                        self.keys.names.clone(),
                        st.live.iter().map(Option::is_some).collect(),
                    ));
                    st.faulted = s.durable_effect().map(|e| e.key.clone());
                }
                if matches!(s.action, Action::Commit) {
                    if let Some(r) = &self.prof.transactions.deferred_constraints {
                        let (fa, stt) = (r.fact.clone(), r.status);
                        self.row(
                            &mut st,
                            &format!("{}.transactions.deferred_constraints", d.id()),
                            fa.as_deref(),
                            stt,
                        );
                    }
                }
                if f.kind == ErrorKind::NotFound && !s.raises.contains(&ErrorKind::NotFound) {
                    // Not an error in this dialect: the statement touches no rows.
                    st.no_rows = Some(true);
                    if st.fault_route.is_none() {
                        st.fault_route = node.next(Edge::Next).map(Routed::Next);
                    }
                    return self.next(id, st);
                }
                return self.fail(id, f.kind, st, None);
            }
        }
        // A doomed or aborted transaction rejects further work.
        if st.doomed && !matches!(s.action, Action::Rollback | Action::RollbackTo(_)) {
            let rejects = match d {
                Dialect::Postgres => !matches!(s.action, Action::Commit),
                _ => !s.effects.is_empty() && self.prof.transactions.doomed_writes.value == "error",
            };
            if rejects {
                let r = &self.prof.transactions.doomed_writes;
                let row_id = match d {
                    Dialect::Tsql => "tsql.transactions.doomed_writes",
                    Dialect::Postgres => "postgres.transactions.doomed_writes",
                    Dialect::Db2i => "db2i.transactions.doomed_writes",
                };
                debug_assert_eq!(row_id, format!("{}.transactions.doomed_writes", d.id()));
                self.row(&mut st, row_id, r.fact.as_deref(), r.status);
                return self.fail(id, ErrorKind::Other, st, None);
            }
        }
        match &s.action {
            Action::Raise { kind, form } => return self.fail(id, *kind, st, Some(form)),
            Action::Reraise { form } => {
                let k = st.caught.last().copied().unwrap_or(ErrorKind::Other);
                return self.fail(id, k, st, Some(form));
            }
            Action::Begin => match d {
                Dialect::Tsql => st.depth += 1,
                _ => {
                    if st.depth == 0 {
                        st.depth = 1;
                    }
                }
            },
            Action::Commit => {
                if let Some(k) = self.commit(&mut st) {
                    return self.fail(id, k, st, None);
                }
            }
            Action::Rollback => {
                if let Some(k) = self.rollback(&mut st) {
                    return self.fail(id, k, st, None);
                }
            }
            Action::Savepoint(n) => {
                if let Some(k) = self.savepoint(&mut st, n) {
                    return self.fail(id, k, st, None);
                }
            }
            Action::RollbackTo(n) => match st.savepoints.iter().rposition(|(x, _)| x == n) {
                Some(i) => {
                    let m = st.savepoints[i].1;
                    st.savepoints.truncate(i + 1);
                    self.undo_to(&mut st, m);
                    if d == Dialect::Postgres {
                        st.doomed = false;
                    }
                }
                None => return self.fail(id, ErrorKind::Other, st, None),
            },
            Action::Release(n) => {
                if let Some(i) = st.savepoints.iter().rposition(|(x, _)| x == n) {
                    st.savepoints.truncate(i);
                }
            }
            Action::XactAbort(v) => {
                if let Some(r) = &self.prof.transactions.session_switch {
                    let (f, s) = (r.fact.clone(), r.status);
                    self.row(
                        &mut st,
                        &format!("{}.transactions.session_switch", d.id()),
                        f.as_deref(),
                        s,
                    );
                }
                st.xact_abort = *v;
                st.xact_abort_local = true;
            }
            Action::ImplicitTransactions(v) => st.implicit = *v,
            Action::Return(code) => {
                st.ret = code.clone();
                st.scopes.clear();
                return Step::Go(EXIT_OK, st);
            }
            Action::Call(_) => {
                st.assumptions.insert("ASM-CALLEES-NO-TXN-CONTROL".into());
            }
            Action::Opaque(what) => st.imprecise.push(format!("{what} is not modelled")),
            Action::Report => {
                st.raised = true;
                st.assumptions.insert("ASM-ERROR-CONTINUES-WARNING".into());
            }
            Action::None => {}
        }
        // Temporary objects read before this statement writes.
        for t in &s.temp_reads {
            let state = st.temp.get(t).copied().unwrap_or(TempState::Empty);
            st.temp_reads.insert((id, t.clone(), state));
        }
        for e in &s.effects {
            match &e.temp {
                Some(t) => self.write_temp(&mut st, t),
                None => {
                    let k = self.keys.id(&e.key);
                    self.write(&mut st, k);
                }
            }
        }
        self.next(id, st)
    }

    fn write(&self, st: &mut State, key: usize) {
        if st.no_commit {
            st.live[key] = Some(Live::Durable);
            return;
        }
        if st.depth > 0 {
            st.live[key] = Some(Live::Pending);
            st.log.push(Log::Effect(key));
        } else {
            st.live[key] = Some(Live::Durable);
        }
    }

    fn write_temp(&self, st: &mut State, t: &str) {
        let prev = st.temp.get(t).copied().unwrap_or(TempState::Empty);
        st.temp.insert(t.to_string(), TempState::Rows);
        let Some(obj) = self.cfg.temps.get(t) else {
            return;
        };
        if let Some(r) = self.prof.temp_row(&obj.object) {
            let (id, fact, status) = (r.id.clone(), r.fact.clone(), r.status);
            self.row(st, &id, fact.as_deref(), status);
        }
        if obj.on_rollback != "keep" && st.depth > 0 && !st.no_commit {
            st.log.push(Log::Temp(t.to_string(), prev));
        }
    }

    fn make_durable(&self, st: &mut State) {
        for l in st.log.drain(..) {
            if let Log::Effect(k) = l {
                if let Some(v @ Live::Pending) = &mut st.live[k] {
                    *v = Live::Durable;
                }
            }
        }
        for (t, obj) in &self.cfg.temps {
            if obj.on_commit == "empty" {
                st.temp.insert(t.clone(), TempState::Empty);
            }
        }
        st.savepoints.clear();
        for s in st.scopes.iter_mut() {
            s.1 = 0;
        }
    }

    fn undo_to(&self, st: &mut State, marker: usize) {
        while st.log.len() > marker {
            match st.log.pop() {
                Some(Log::Effect(k)) => {
                    if let Some(v @ Live::Pending) = &mut st.live[k] {
                        *v = Live::Undone;
                    }
                }
                Some(Log::Temp(t, prev)) => {
                    let obj = self.cfg.temps.get(&t);
                    let v = if obj.is_some_and(|o| o.on_rollback == "empty") {
                        TempState::Empty
                    } else {
                        prev
                    };
                    st.temp.insert(t, v);
                }
                None => break,
            }
        }
        for s in st.scopes.iter_mut() {
            s.1 = s.1.min(marker);
        }
    }

    fn undo_all(&self, st: &mut State) {
        self.undo_to(st, 0);
        st.savepoints.clear();
        for (t, obj) in &self.cfg.temps {
            if obj.on_rollback == "empty" && !st.no_commit {
                st.temp.insert(t.clone(), TempState::Empty);
            }
        }
    }

    fn in_scope(&self, st: &State, kind: ScopeKind) -> bool {
        st.scopes
            .iter()
            .any(|(s, _)| self.cfg.scopes[*s].kind == kind)
    }

    fn txn_row(&self, st: &mut State, name: &str) -> String {
        let t = &self.prof.transactions;
        let r = match name {
            "commit_without_txn" => &t.commit_without_txn,
            "rollback_without_txn" => &t.rollback_without_txn,
            "commit_in_exception_block" => &t.commit_in_exception_block,
            "commit_in_atomic" => &t.commit_in_atomic,
            "commit_in_caller_txn" => &t.commit_in_caller_txn,
            "doomed_commit" => &t.doomed_commit,
            "nesting" => &t.nesting,
            "rollback_scope" => &t.rollback_scope,
            "savepoints" => &t.savepoints,
            "commit_in_function" => match &t.commit_in_function {
                Some(r) => r,
                None => return "allowed".into(),
            },
            _ => return String::new(),
        };
        let (v, f, s) = (r.value.clone(), r.fact.clone(), r.status);
        self.row(
            st,
            &format!("{}.transactions.{name}", self.cfg.dialect.id()),
            f.as_deref(),
            s,
        );
        v
    }

    /// Runs COMMIT; returns the error kind if it fails.
    fn commit(&self, st: &mut State) -> Option<ErrorKind> {
        let d = self.cfg.dialect;
        let routine = self.cfg.routine;
        if d == Dialect::Postgres && routine != RoutineKind::Script {
            if matches!(routine, RoutineKind::Function | RoutineKind::Trigger)
                && self.txn_row(st, "commit_in_function") == "error"
            {
                return Some(ErrorKind::Other);
            }
            if self.in_scope(st, ScopeKind::ExceptionBlock)
                && self.txn_row(st, "commit_in_exception_block") == "error"
            {
                return Some(ErrorKind::Other);
            }
        }
        if self.in_scope(st, ScopeKind::Atomic) && self.txn_row(st, "commit_in_atomic") == "error" {
            return Some(ErrorKind::Other);
        }
        if st.doomed {
            return match self.txn_row(st, "doomed_commit").as_str() {
                "rolls_back" => {
                    self.undo_all(st);
                    st.doomed = false;
                    st.depth = st.depth.saturating_sub(1).max(self.reopen_depth(st));
                    None
                }
                _ => Some(ErrorKind::Other),
            };
        }
        if st.no_commit {
            self.txn_row(st, "commit_without_txn");
            return None;
        }
        if st.depth == 0 {
            return match self.txn_row(st, "commit_without_txn").as_str() {
                "error" => Some(ErrorKind::Other),
                _ => None,
            };
        }
        if st.caller_depth > 0 {
            match self.txn_row(st, "commit_in_caller_txn").as_str() {
                "error" => return Some(ErrorKind::Other),
                "commits_caller" => {
                    self.make_durable(st);
                    return None;
                }
                _ => {}
            }
        }
        if self.txn_row(st, "nesting") == "counted" {
            st.depth -= 1;
            if st.depth == 0 {
                self.make_durable(st);
            }
        } else {
            self.make_durable(st);
            st.depth = self.reopen_depth(st);
        }
        None
    }

    /// Depth right after a COMMIT or ROLLBACK ends the current transaction:
    /// routines in a CALL or unit of work start a new one at once.
    fn reopen_depth(&self, st: &State) -> u32 {
        match st.owner {
            Owner::Call | Owner::UnitOfWork | Owner::CallerStatement | Owner::CallerTxn
                if self.cfg.dialect != Dialect::Tsql =>
            {
                1
            }
            _ => 0,
        }
    }

    fn rollback(&self, st: &mut State) -> Option<ErrorKind> {
        let d = self.cfg.dialect;
        let routine = self.cfg.routine;
        if d == Dialect::Postgres && routine != RoutineKind::Script {
            if matches!(routine, RoutineKind::Function | RoutineKind::Trigger)
                && self.txn_row(st, "commit_in_function") == "error"
            {
                return Some(ErrorKind::Other);
            }
            if self.in_scope(st, ScopeKind::ExceptionBlock)
                && self.txn_row(st, "commit_in_exception_block") == "error"
            {
                return Some(ErrorKind::Other);
            }
            if st.caller_depth > 0 && self.txn_row(st, "commit_in_caller_txn") == "error" {
                return Some(ErrorKind::Other);
            }
        }
        if self.in_scope(st, ScopeKind::Atomic) && self.txn_row(st, "commit_in_atomic") == "error" {
            return Some(ErrorKind::Other);
        }
        if st.no_commit {
            self.txn_row(st, "rollback_without_txn");
            return None;
        }
        if st.depth == 0 {
            return match self.txn_row(st, "rollback_without_txn").as_str() {
                "error" => Some(ErrorKind::Other),
                _ => None,
            };
        }
        self.txn_row(st, "rollback_scope");
        self.undo_all(st);
        st.doomed = false;
        st.depth = self.reopen_depth(st);
        None
    }

    fn savepoint(&self, st: &mut State, name: &str) -> Option<ErrorKind> {
        let t = &self.prof.transactions;
        self.txn_row(st, "savepoints");
        if st.no_commit && t.savepoints_need_commitment_control.value {
            let r = &t.savepoints_need_commitment_control;
            self.row(
                st,
                "db2i.transactions.savepoints_need_commitment_control",
                r.fact.as_deref(),
                r.status,
            );
            return Some(ErrorKind::Other);
        }
        if st.depth == 0 {
            return Some(ErrorKind::Other);
        }
        let m = st.log.len();
        st.savepoints.push((name.to_string(), m));
        None
    }

    /// Records the atomicity row behind a savepoint scope; returns whether
    /// the scope can roll back (not under a no-commit level).
    fn atomic_row(&self, st: &mut State, scope: ScopeId) -> bool {
        let d = self.cfg.dialect.id();
        match self.cfg.scopes[scope].kind {
            ScopeKind::Atomic if st.no_commit => {
                if let Some(r) = &self.prof.atomicity.atomic_under_no_commit {
                    let (f, s) = (r.fact.clone(), r.status);
                    self.row(
                        st,
                        &format!("{d}.atomicity.atomic_under_no_commit"),
                        f.as_deref(),
                        s,
                    );
                    return r.value;
                }
                false
            }
            ScopeKind::Atomic | ScopeKind::ExceptionBlock => {
                let r = &self.prof.atomicity.atomic_compound;
                let (f, s) = (r.fact.clone(), r.status);
                self.row(
                    st,
                    &format!("{d}.atomicity.atomic_compound"),
                    f.as_deref(),
                    s,
                );
                true
            }
            _ => false,
        }
    }

    /// Rolls a savepoint scope back as the error leaves it.
    fn unwind_scope(&self, st: &mut State, scope: ScopeId, marker: usize) {
        if self.atomic_row(st, scope) {
            self.undo_to(st, marker);
        }
    }

    fn apply_txn(&self, st: &mut State, e: TxnEffect, scope_marker: Option<usize>) {
        match e {
            TxnEffect::None => {}
            TxnEffect::RollbackAll => {
                if !st.no_commit {
                    self.undo_all(st);
                    if self.cfg.dialect == Dialect::Tsql {
                        st.depth = 0;
                    }
                }
                st.doomed = false;
            }
            TxnEffect::Doom => {
                if st.depth > 0 {
                    st.doomed = true;
                }
            }
            TxnEffect::RollbackScope => {
                if let Some(m) = scope_marker {
                    if !st.no_commit {
                        self.undo_to(st, m);
                    }
                }
            }
        }
    }

    fn fail(&self, id: NodeId, kind: ErrorKind, mut st: Box<State>, form: Option<&str>) -> Step {
        let node = self.cfg.node(id);
        let s = node.stmt().expect("statement node");
        st.last_error = Some(kind);
        if self.cfg.dialect == Dialect::Tsql && !st.xact_abort_local && !self.sc.xact_abort {
            st.assumptions
                .insert("ASM-TSQL-CALLER-XACT-ABORT-OFF".into());
        }
        let sit = self.situation(&st);
        let route = node.route(kind).cloned().expect("route per kind");
        let rule = match &route.handler {
            Some(h) => self.prof.caught_rule(h.construct, kind, &sit),
            // First matching row in table order; form-specific rows (THROW,
            // RAISERROR) match only their form.
            None => self.prof.unhandled.iter().find(|r| {
                (r.construct.is_none() || r.construct.as_deref() == form)
                    && (r.kinds.is_empty() || r.kinds.contains(&kind))
                    && r.when.matches(&sit)
            }),
        };
        let first_fault = st.fault_index.is_some() && st.fault_route.is_none();
        if first_fault {
            st.fault_route = Some(
                match (&route.handler, rule.as_ref().and_then(|r| r.control)) {
                    (Some(h), _) => Routed::Handler {
                        construct: h.construct,
                        entry: h.entry,
                    },
                    (None, Some(super::profile::Control::Next)) => Routed::Next(route.resume),
                    _ => Routed::Exit,
                },
            );
        }
        let Some(rule) = rule else {
            st.imprecise.push(format!("no profile row for {kind:?}"));
            st.raising = Some(kind);
            st.scopes.clear();
            return Step::Go(super::cfg::EXIT_ERR, st);
        };
        self.row(&mut st, &rule.id, rule.fact.as_deref(), rule.status);
        if kind == ErrorKind::NotFound {
            st.no_rows = Some(true);
        } else {
            st.rows_failed = true;
        }
        // The failing statement's own rows.
        if form.is_none() && s.multi_row && !self.prof.is_warning(kind) {
            let a = &self.prof.atomicity;
            let partial = if s.not_atomic {
                if let Some(r) = &a.merge_not_atomic {
                    let (f, st_) = (r.fact.clone(), r.status);
                    self.row(
                        &mut st,
                        &format!("{}.atomicity.merge_not_atomic", self.cfg.dialect.id()),
                        f.as_deref(),
                        st_,
                    );
                }
                true
            } else {
                let r = if s.kind == crate::classify::StmtKind::Merge {
                    (&a.merge, "merge")
                } else {
                    (&a.statement, "statement")
                };
                let (f, st_) = (r.0.fact.clone(), r.0.status);
                self.row(
                    &mut st,
                    &format!("{}.atomicity.{}", self.cfg.dialect.id(), r.1),
                    f.as_deref(),
                    st_,
                );
                rule.statement == super::profile::StmtEffect::Partial
            };
            if partial {
                if let Some(e) = s.durable_effect() {
                    let k = self.keys.id(&e.key);
                    self.write(&mut st, k);
                    st.partial.insert(e.key.clone());
                    st.assumptions.insert("ASM-MULTI-ROW".into());
                }
            }
        }
        match route.handler {
            Some(h) => {
                // Leave the scopes between the statement and the handler.
                for sc in &route.unwind {
                    if let Some(i) = st.scopes.iter().rposition(|(x, _)| x == sc) {
                        let m = st.scopes[i].1;
                        st.scopes.truncate(i);
                        self.unwind_scope(&mut st, *sc, m);
                    }
                }
                let marker = st.scopes.iter().rfind(|(x, _)| *x == h.scope).map(|x| x.1);
                let can_undo = self.atomic_row(&mut st, h.scope);
                let effect = match rule.transaction {
                    TxnEffect::RollbackScope if !can_undo => TxnEffect::None,
                    e => e,
                };
                self.apply_txn(&mut st, effect, marker);
                if matches!(h.construct, "try_catch" | "exception_block") {
                    if let Some(i) = st.scopes.iter().rposition(|(x, _)| *x == h.scope) {
                        st.scopes.truncate(i);
                    }
                }
                st.caught.push(kind);
                if h.construct == "continue_handler" {
                    st.resume.push(route.resume);
                }
                Step::Go(h.entry, st)
            }
            None => {
                if !self.prof.is_warning(kind) {
                    st.raised = true;
                }
                match rule.control {
                    Some(super::profile::Control::Next) => {
                        self.apply_txn(&mut st, rule.transaction, None);
                        Step::Go(route.resume, st)
                    }
                    _ => {
                        while let Some((sc, m)) = st.scopes.pop() {
                            self.unwind_scope(&mut st, sc, m);
                        }
                        self.apply_txn(&mut st, rule.transaction, None);
                        st.raising = Some(kind);
                        Step::Go(super::cfg::EXIT_ERR, st)
                    }
                }
            }
        }
    }

    /// What becomes of an effect left in state `l` when the routine ends:
    /// its fate (before a partial write is told apart), whether it is left
    /// open to the caller, and whether the caller commits it.
    fn fate_of(
        &self,
        l: Live,
        owner: Owner,
        doomed: bool,
        raised_exit: bool,
    ) -> (Fate, bool, bool) {
        match l {
            Live::Durable => (Fate::Durable, false, false),
            Live::Undone => (Fate::Undone, false, false),
            // A doomed transaction can only be rolled back.
            Live::Pending if doomed => (Fate::Undone, false, false),
            Live::Pending => match owner {
                Owner::Autocommit | Owner::CallerTxn => (Fate::Caller, true, false),
                Owner::CallerStatement | Owner::Call => {
                    if raised_exit {
                        (Fate::Undone, false, false)
                    } else {
                        (Fate::Durable, false, false)
                    }
                }
                Owner::UnitOfWork => {
                    if raised_exit {
                        (Fate::Caller, true, false)
                    } else {
                        (Fate::Durable, true, true)
                    }
                }
            },
        }
    }

    fn finish(&self, exit: Exit, st: State) -> Outcome {
        let mut st = st;
        let raised_exit = matches!(exit, Exit::Raised(_));
        let left_open = st.owner == Owner::Autocommit && st.depth > st.caller_depth;
        let mut unit_commit = 0;
        if self.cfg.dialect == Dialect::Tsql
            && self.cfg.routine == RoutineKind::Procedure
            && st.depth != st.entry_depth
        {
            if let Some(r) = &self.prof.transactions.trancount_mismatch_at_exit {
                let (f, s) = (r.fact.clone(), r.status);
                self.row(
                    &mut st,
                    "tsql.transactions.trancount_mismatch_at_exit",
                    f.as_deref(),
                    s,
                );
                st.raised = true;
            }
        }
        let n = self.keys.names.len();
        let (mut fates, mut open) = (vec![0u8; n], vec![0u8; n]);
        for (i, l) in st.live.iter().enumerate() {
            let Some(l) = l else {
                continue;
            };
            let (fate, opened, committed) = self.fate_of(*l, st.owner, st.doomed, raised_exit);
            open[i] = u8::from(opened);
            unit_commit += usize::from(committed);
            let fate = if fate == Fate::Durable
                && !st.partial.is_empty()
                && st.partial.contains(&*self.keys.names[i])
            {
                Fate::Partial
            } else {
                fate
            };
            fates[i] = fate_code(Some(fate));
        }
        if unit_commit > 0 {
            st.assumptions.insert("ASM-CALLER-COMMITS".into());
        }
        Outcome {
            exit,
            trace: Trace::new(st.trace),
            fault_index: st.fault_index,
            fault_route: st.fault_route,
            effects: Effects::dense(self.keys.names.clone(), fates),
            open: KeySet::dense(self.keys.names.clone(), open),
            partial: st.partial,
            before_fault: st.before_fault.unwrap_or_default(),
            faulted: st.faulted,
            fault_hit: !st.faults_done.is_empty(),
            temp_reads: st.temp_reads,
            raised: st.raised,
            depth: st.depth,
            left_open,
            doomed: st.doomed,
            signature: st.signature,
            imprecise: st.imprecise,
            cut: st.cut,
            durability_unknown: st.durability_unknown,
            rows: st.rows,
            assumptions: st.assumptions,
            untested: st.untested,
            unit_commit,
        }
    }
}
