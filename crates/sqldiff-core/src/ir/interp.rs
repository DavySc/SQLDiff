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

use sqldiff_syntax::Dialect;

use super::cfg::{Action, Cfg, Edge, NodeId, Op, RoutineKind, ScopeId, ScopeKind, Test, EXIT_OK};
use super::profile::{Context, ErrorKind, Profile, Situation, Status, TxnEffect};

/// A fault injected at a statement: the statement fails with `kind` the
/// first time it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub node: NodeId,
    pub kind: ErrorKind,
}

/// The environment a run starts in, and the faults to inject.
#[derive(Debug, Clone, Default)]
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
    pub trace: Vec<NodeId>,
    /// Index in `trace` of the statement the fault was injected into.
    pub fault_index: Option<usize>,
    /// Where the injected fault sent control.
    pub fault_route: Option<Routed>,
    /// Fate of each durable effect that ran, by effect key. In
    /// caller-transaction scenarios the key `caller` stands for the caller's
    /// own pending work.
    pub effects: BTreeMap<String, Fate>,
    /// Effects still uncommitted when the routine returns, left to the
    /// caller (including those the caller is assumed to commit).
    pub open: BTreeSet<String>,
    /// Effects whose statement failed part-way and kept some rows.
    pub partial: BTreeSet<String>,
    /// The statement node that produced each effect.
    pub effect_nodes: BTreeMap<String, NodeId>,
    /// Effects that had already run when the first fault hit.
    pub before_fault: BTreeSet<String>,
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
    /// Set when the fates of effects are not known (e.g. the Db2 for i
    /// commit level is not declared); control flow is still exact.
    pub durability_unknown: Option<String>,
    /// Profile rows this path relied on.
    pub rows: BTreeSet<String>,
    pub assumptions: BTreeSet<String>,
    /// Profile rows this path relied on that are marked unverified (not
    /// tested on a real system): verdicts that depend on them are capped.
    pub untested: BTreeSet<String>,
}

impl Outcome {
    /// An outcome of a run that did nothing (for findings that need no run).
    pub fn empty() -> Outcome {
        Outcome {
            exit: Exit::Normal,
            trace: Vec::new(),
            fault_index: None,
            fault_route: None,
            effects: BTreeMap::new(),
            open: BTreeSet::new(),
            partial: BTreeSet::new(),
            effect_nodes: BTreeMap::new(),
            before_fault: BTreeSet::new(),
            faulted: None,
            fault_hit: false,
            temp_reads: BTreeSet::new(),
            raised: false,
            depth: 0,
            left_open: false,
            doomed: false,
            signature: Vec::new(),
            imprecise: Vec::new(),
            durability_unknown: None,
            rows: BTreeSet::new(),
            assumptions: BTreeSet::new(),
            untested: BTreeSet::new(),
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
    Effect(String),
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
    live: BTreeMap<String, Live>,
    effect_nodes: BTreeMap<String, NodeId>,
    log: Vec<Log>,
    temp: BTreeMap<String, TempState>,
    scopes: Vec<(ScopeId, usize)>,
    savepoints: Vec<(String, usize)>,
    resume: Vec<NodeId>,
    caught: Vec<ErrorKind>,
    last_error: Option<ErrorKind>,
    /// Whether the last statement touched no rows, when a scenario says so.
    no_rows: Option<bool>,
    partial: BTreeSet<String>,
    entry_depth: u32,
    faults_done: BTreeSet<NodeId>,
    before_fault: Option<BTreeSet<String>>,
    faulted: Option<String>,
    temp_reads: BTreeSet<(NodeId, String, TempState)>,
    raised: bool,
    ret: Option<String>,
    raising: Option<ErrorKind>,
    loop_seen: HashMap<NodeId, Vec<String>>,
    signature: Vec<(String, bool)>,
    imprecise: Vec<String>,
    durability_unknown: Option<String>,
    rows: BTreeSet<String>,
    assumptions: BTreeSet<String>,
    steps: usize,
    trace: Vec<NodeId>,
    fault_index: Option<usize>,
    fault_route: Option<Routed>,
    untested: BTreeSet<String>,
}

/// Pseudo-effect for the caller's pending work in caller-transaction scenarios.
pub const CALLER: &str = "caller";

const MAX_PATHS: usize = 128;
const MAX_STEPS: usize = 4000;

enum Step {
    Go(NodeId, Box<State>),
    Fork(Vec<(NodeId, Box<State>)>),
    Done(Box<Outcome>),
}

struct Interp<'c> {
    cfg: &'c Cfg,
    prof: &'static Profile,
    sc: &'c Scenario,
}

/// Explores every path of `cfg` under `sc` and returns one outcome per path.
pub fn explore(cfg: &Cfg, sc: &Scenario) -> Vec<Outcome> {
    let it = Interp {
        cfg,
        prof: Profile::of(cfg.dialect),
        sc,
    };
    it.run()
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
        let init = self.initial();
        let first = self
            .cfg
            .node(self.cfg.entry)
            .next(Edge::Next)
            .unwrap_or(EXIT_OK);
        let mut work: Vec<(NodeId, Box<State>)> = vec![(first, Box::new(init))];
        let mut outs = Vec::new();
        while let Some((mut node, mut st)) = work.pop() {
            loop {
                st.steps += 1;
                if st.steps > MAX_STEPS {
                    st.imprecise.push("step budget exhausted".into());
                    outs.push(self.finish(Exit::Normal, *st));
                    break;
                }
                match self.step(node, st) {
                    Step::Go(n, s) => {
                        node = n;
                        st = s;
                    }
                    Step::Fork(mut v) => {
                        let (n, s) = v.remove(0);
                        if work.len() + outs.len() + v.len() < MAX_PATHS {
                            work.extend(v);
                        } else {
                            let mut s = s;
                            s.imprecise.push("path budget exhausted".into());
                            node = n;
                            st = s;
                            continue;
                        }
                        node = n;
                        st = s;
                    }
                    Step::Done(o) => {
                        outs.push(*o);
                        break;
                    }
                }
            }
        }
        outs
    }

    fn row(&self, st: &mut State, id: &str, fact: Option<&str>, status: Status) {
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
        let mut st = State {
            depth: 0,
            caller_depth: 0,
            owner: Owner::Autocommit,
            no_commit: false,
            doomed: false,
            xact_abort: sc.xact_abort,
            xact_abort_local: false,
            implicit: sc.implicit_transactions,
            live: BTreeMap::new(),
            effect_nodes: BTreeMap::new(),
            log: Vec::new(),
            temp: BTreeMap::new(),
            scopes: Vec::new(),
            savepoints: Vec::new(),
            resume: Vec::new(),
            caught: Vec::new(),
            last_error: None,
            no_rows: None,
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
            st.live.insert(CALLER.into(), Live::Pending);
            st.log.push(Log::Effect(CALLER.into()));
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

    fn step(&self, id: NodeId, mut st: Box<State>) -> Step {
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
                let text = match test {
                    Test::Unknown(x) => x.clone(),
                    _ => String::new(),
                };
                if *is_loop {
                    let digest = Self::digest(&st);
                    let seen = st.loop_seen.entry(id).or_default();
                    seen.push(digest);
                    let n = seen.len();
                    if n > 2 {
                        if seen[n - 1] != seen[n - 2] {
                            st.imprecise
                                .push(format!("loop `{text}` summarized after two iterations"));
                        }
                        st.loop_seen.remove(&id);
                        return Step::Go(f, st);
                    }
                }
                let mut a = st.clone();
                a.signature.push((text.clone(), true));
                st.signature.push((text, false));
                Step::Fork(vec![(t, a), (f, st)])
            }
            Op::Stmt(_) => self.exec(id, st),
        }
    }

    fn digest(st: &State) -> String {
        format!(
            "{}|{}|{:?}|{:?}|{}",
            st.depth,
            st.doomed,
            st.live,
            st.temp,
            st.log.len()
        )
    }

    fn exec(&self, id: NodeId, mut st: Box<State>) -> Step {
        let node = self.cfg.node(id);
        let s = node.stmt().expect("statement node");
        let d = self.cfg.dialect;
        st.last_error = None;
        st.no_rows = None;
        st.trace.push(id);

        // Injected fault.
        if !st.faults_done.contains(&id) {
            if let Some(f) = self.sc.faults.iter().find(|f| f.node == id) {
                st.faults_done.insert(id);
                if st.fault_index.is_none() {
                    st.fault_index = Some(st.trace.len() - 1);
                }
                if st.before_fault.is_none() {
                    st.before_fault = Some(st.live.keys().cloned().collect());
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
                self.row(
                    &mut st,
                    &format!("{}.transactions.doomed_writes", d.id()),
                    r.fact.as_deref(),
                    r.status,
                );
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
                    st.effect_nodes.insert(e.key.clone(), id);
                    self.write(&mut st, &e.key);
                }
            }
        }
        self.next(id, st)
    }

    fn write(&self, st: &mut State, key: &str) {
        if st.no_commit {
            st.live.insert(key.to_string(), Live::Durable);
            return;
        }
        if st.depth == 0 && st.implicit && self.cfg.dialect == Dialect::Tsql {
            st.depth = 1;
        }
        if st.depth > 0 {
            st.live.insert(key.to_string(), Live::Pending);
            st.log.push(Log::Effect(key.to_string()));
        } else {
            st.live.insert(key.to_string(), Live::Durable);
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
                if st.live.get(&k) == Some(&Live::Pending) {
                    st.live.insert(k, Live::Durable);
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
                    if st.live.get(&k) == Some(&Live::Pending) {
                        st.live.insert(k, Live::Undone);
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
        }
        .cloned();
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
                    st.effect_nodes.insert(e.key.clone(), id);
                    self.write(&mut st, &e.key);
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

    fn finish(&self, exit: Exit, st: State) -> Outcome {
        let mut st = st;
        let raised_exit = matches!(exit, Exit::Raised(_));
        let mut effects = BTreeMap::new();
        let left_open = st.owner == Owner::Autocommit && st.depth > st.caller_depth;
        let mut caller_commits = false;
        let mut open = BTreeSet::new();
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
        for (k, l) in &st.live {
            let fate = match l {
                Live::Durable => Fate::Durable,
                Live::Undone => Fate::Undone,
                // A doomed transaction can only be rolled back.
                Live::Pending if st.doomed => Fate::Undone,
                Live::Pending => match st.owner {
                    Owner::Autocommit | Owner::CallerTxn => Fate::Caller,
                    Owner::CallerStatement | Owner::Call => {
                        if raised_exit {
                            Fate::Undone
                        } else {
                            Fate::Durable
                        }
                    }
                    Owner::UnitOfWork => {
                        if raised_exit {
                            Fate::Caller
                        } else {
                            caller_commits = true;
                            open.insert(k.clone());
                            Fate::Durable
                        }
                    }
                },
            };
            if fate == Fate::Caller {
                open.insert(k.clone());
            }
            let fate = if fate == Fate::Durable && st.partial.contains(k) {
                Fate::Partial
            } else {
                fate
            };
            effects.insert(k.clone(), fate);
        }
        if caller_commits {
            st.assumptions.insert("ASM-CALLER-COMMITS".into());
        }
        Outcome {
            exit,
            trace: st.trace,
            fault_index: st.fault_index,
            fault_route: st.fault_route,
            effects,
            open,
            partial: st.partial,
            effect_nodes: st.effect_nodes,
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
            durability_unknown: st.durability_unknown,
            rows: st.rows,
            assumptions: st.assumptions,
            untested: st.untested,
        }
    }
}
