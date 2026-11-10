//! Behavioural comparison of two artifacts on the semantic IR: both sides
//! are lowered and run under the same scenarios (no fault; each aligned
//! write failing with each error kind it can raise; the caller holding an
//! open transaction), and the outcomes are compared. This replaces the
//! summary-level checker that used to live in `facts.rs` and keeps its rule
//! ids: `txn.atomicity`, `txn.statement-atomicity`, `txn.caller-transaction`
//! and `error.contract`, plus `error.control`, `txn.temp-state` and
//! `txn.durability`.
//!
//! Paired exploration (design §3 Layer 5 item 4): effects are aligned by key
//! (kind, folded target, occurrence), and a MERGE one side splits into
//! separate statements is aligned with its parts. Per scenario and path the
//! outcome tuples are compared (control target, transaction effect, durable
//! effects, how the caller's unit of work goes on); the first divergence of
//! each kind becomes a finding with a plain-language witness and a
//! structured one ([`Witness`]). Rules added for this: `error.control`
//! (semantic: which statements run after the error), `txn.durability` for the
//! work that run keeps, `txn.caller-continuation` and
//! `concurrency.split-statement`. [`what_if`] answers one scenario on demand,
//! and [`Env`] re-runs the analysis under flipped scenario assumptions.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use sqldiff_syntax::{ast, Dialect, SyntaxNode};

use super::cfg::{Cfg, Effect, NodeId};
use super::delta::{scan_differing, KeyProbe};
use super::interp::{
    Effects, Exit, Explorer, Fate, Fault, KeyUniverse, Outcome, Routed, Scenario, TempState,
};
use super::profile::ErrorKind;
use super::witness::{PathStep, ScenarioInfo, ScenarioKind, SideOutcome, WhatIf, Witness};
use crate::artifact::Analyzed;
use crate::classify::StmtKind;
use crate::model::{
    fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, Severity, Verdict,
};

/// Text of the assumptions the IR records, by id.
pub fn assumption(id: &str) -> Assumption {
    let text = match id {
        "ASM-NO-CALLER-TXN" => "The caller does not hold an open transaction".to_string(),
        "ASM-TSQL-CALLER-XACT-ABORT-OFF" => "The caller's XACT_ABORT setting is OFF".to_string(),
        "ASM-ERROR-CONTINUES-WARNING" => {
            "A statement error that T-SQL reports and goes on after is reported as a warning here".to_string()
        }
        "ASM-NO-TRIGGERS" => {
            "No triggers on the written tables change the error or transaction outcome".to_string()
        }
        "ASM-CALLER-COMMITS" => "The caller commits its unit of work after a successful call".to_string(),
        "ASM-CALLER-MAY-COMMIT-AFTER-ERROR" => {
            "A caller may commit its unit of work after the call fails (it does not always roll back)".to_string()
        }
        "ASM-DB2-COMMIT-LEVEL" => "The routine's commit level (SET OPTION COMMIT) as created".to_string(),
        "ASM-CALLEES-NO-TXN-CONTROL" => {
            "Called routines do not commit or roll back, and their writes follow the caller's transaction".to_string()
        }
        "ASM-CALLER-CHECKS-RC" => "Callers inspect the return code".to_string(),
        "ASM-MULTI-ROW" => "The statement can affect several rows".to_string(),
        "ASM-CONCURRENT-WRITERS" => {
            "Other sessions can change the tables the routine reads while it runs".to_string()
        }
        "ASM-READ-COMMITTED" => {
            "The routine runs at READ COMMITTED (the default), so each statement sees the data committed when it starts".to_string()
        }
        other => match other.strip_prefix("ASM-FACT-") {
            Some(f) => format!(
                "Vendor fact {f} holds as tabled in the dialect profile (not fully verified; see docs/vendor-facts-verification.md)"
            ),
            None => match other.strip_prefix("ASM-ROW-") {
                Some(r) => format!("Profile row {r} holds as tabled"),
                None => other.to_string(),
            },
        },
    };
    Assumption::default(id, &text)
}

struct SideIr<'x, 'a> {
    a: &'x Analyzed<'a>,
    cfg: Cfg,
    /// Document-order position and node of the first effect with each key.
    at: HashMap<String, (usize, NodeId)>,
    /// Plain reads of each temporary object, in document order.
    reads: HashMap<String, Vec<NodeId>>,
    /// Number of effects per key without its `#n` occurrence suffix.
    bases: HashMap<String, usize>,
    /// Line of each node, found once: every outcome of every scenario names
    /// its effects with their lines, and finding a line walks the whole
    /// statement, so long scripts went cubic.
    line_nos: Vec<OnceCell<Option<usize>>>,
}

impl<'x, 'a> SideIr<'x, 'a> {
    /// Lowers one side and indexes its effects and reads: the scenarios
    /// look keys up for every outcome pair, so a scan per lookup made large
    /// routines cubic.
    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: sorts each entry independently
    fn new(a: &'x Analyzed<'a>, other: &Analyzed, routine: Option<SyntaxNode>) -> Self {
        let cfg = super::cfg::lower_routine(a, other_temps(a, other), routine);
        let mut at = HashMap::new();
        let mut bases = HashMap::new();
        for (i, (n, e)) in cfg.effect_nodes().into_iter().enumerate() {
            at.entry(e.key.clone()).or_insert((i, n));
            if let Some((b, _)) = e.key.rsplit_once('#') {
                *bases.entry(b.to_string()).or_insert(0) += 1;
            }
        }
        let mut reads: HashMap<String, Vec<NodeId>> = HashMap::new();
        for i in 0..cfg.nodes.len() {
            let Some(s) = cfg.node(i).stmt() else {
                continue;
            };
            if s.durable_effect().is_some() {
                continue;
            }
            for (j, t) in s.temp_reads.iter().enumerate() {
                if !s.temp_reads[..j].contains(t) {
                    reads.entry(t.clone()).or_default().push(i);
                }
            }
        }
        #[allow(clippy::iter_over_hash_type)] // each value is sorted on its own
        for v in reads.values_mut() {
            v.sort_by_key(|&i| cfg.node(i).syntax.as_ref().map(|s| s.text_range().start()));
        }
        SideIr {
            a,
            line_nos: (0..cfg.nodes.len()).map(|_| OnceCell::new()).collect(),
            cfg,
            at,
            reads,
            bases,
        }
    }
}

impl SideIr<'_, '_> {
    fn name(&self) -> String {
        format!(
            "side {:?} ({})",
            self.a.side,
            self.a.dialect().display_name()
        )
    }

    fn effect(&self, key: &str) -> Option<(NodeId, &Effect)> {
        let &(_, n) = self.at.get(key)?;
        Some((n, self.cfg.node(n).stmt()?.durable_effect()?))
    }

    /// Statements that read temporary object `t` without a durable effect
    /// of their own, in document order.
    fn plain_reads(&self, t: &str) -> &[NodeId] {
        self.reads.get(t).map_or(&[], Vec::as_slice)
    }

    /// Key of a statement that reads a temporary object: its durable effect,
    /// or `Read:T#n` for the n-th plain read of `T`.
    fn read_key(&self, n: NodeId, t: &str) -> Option<String> {
        let s = self.cfg.node(n).stmt()?;
        if let Some(e) = s.durable_effect() {
            return Some(e.key.clone());
        }
        let i = self.plain_reads(t).iter().position(|&x| x == n)?;
        Some(format!("Read:{t}#{}", i + 1))
    }

    /// The statement behind an effect or read key.
    fn locate(&self, key: &str) -> Option<NodeId> {
        if let Some((n, _)) = self.effect(key) {
            return Some(n);
        }
        let (t, i) = key.strip_prefix("Read:")?.rsplit_once('#')?;
        let i: usize = i.parse().ok()?;
        self.plain_reads(t).get(i.checked_sub(1)?).copied()
    }

    fn syntax(&self, n: NodeId) -> Option<&SyntaxNode> {
        self.cfg.node(n).syntax.as_ref()
    }

    fn line(&self, n: NodeId) -> String {
        self.line_no(n)
            .map(|l| format!("{:?} line {l}", self.a.side))
            .unwrap_or_default()
    }

    /// Effect phrase, with an ordinal when the same write occurs more than once.
    fn phrase(&self, key: &str) -> String {
        if let Some((t, n)) = key.strip_prefix("Read:").and_then(|k| k.rsplit_once('#')) {
            let name = t.to_lowercase();
            return if self.plain_reads(t).len() > 1 {
                format!("the read of {name} (occurrence {n})")
            } else {
                format!("the read of {name}")
            };
        }
        let Some((_, e)) = self.effect(key) else {
            return key.to_string();
        };
        let p = effect_phrase(e);
        let (base, n) = key.rsplit_once('#').unwrap_or((key, "1"));
        let repeated = self.bases.get(base).is_some_and(|&c| c > 1);
        if !repeated {
            return p;
        }
        let ord = match n {
            "1" => "first",
            "2" => "second",
            "3" => "third",
            "4" => "fourth",
            _ => return format!("{p} (occurrence {n})"),
        };
        p.replacen("the ", &format!("the {ord} "), 1)
    }

    fn phrase_at(&self, key: &str) -> String {
        match self.locate(key) {
            Some(n) => format!("{} ({})", self.phrase(key), self.line(n)),
            None => key.to_string(),
        }
    }
}

fn effect_phrase(e: &Effect) -> String {
    let upsert = e.key.starts_with("Upsert:");
    match e.kind {
        _ if upsert && e.kind != StmtKind::Merge => format!("the upsert into {}", e.target),
        StmtKind::Insert => format!("the INSERT into {}", e.target),
        StmtKind::Update => format!("the UPDATE of {}", e.target),
        StmtKind::Delete => format!("the DELETE from {}", e.target),
        StmtKind::Merge => format!("the MERGE into {}", e.target),
        StmtKind::Call => format!("the call to {}", e.target),
        _ => format!("the write to {}", e.target),
    }
}

fn fate_phrase(s: &SideIr, key: &str, f: Option<Fate>) -> String {
    let e = s.phrase_at(key);
    match f {
        Some(Fate::Durable) => format!("keeps {e} durable"),
        Some(Fate::Undone) => format!("undoes {e}"),
        Some(Fate::Partial) => format!("keeps the rows {e} changed before the error"),
        Some(Fate::Caller) => {
            format!("leaves {e} uncommitted, for the caller to commit or roll back")
        }
        None => format!("does not run {e}"),
    }
}

fn exit_phrase(o: &Outcome) -> String {
    match &o.exit {
        Exit::Normal if o.raised => "returns normally after sending the error to the caller".into(),
        Exit::Normal => "returns normally".into(),
        Exit::Code(c) => format!("returns code {c}"),
        Exit::Raised(_) => "exits and the error (SQLSTATE) propagates to the caller".into(),
    }
}

/// How the caller sees the end of the routine. Returning normally after an
/// error message went out (T-SQL statement-level errors) looks like an error
/// to the caller, the same as an exit with the error.
fn exit_class(o: &Outcome) -> String {
    match &o.exit {
        Exit::Normal if o.raised => "error".into(),
        Exit::Normal => "normal".into(),
        Exit::Code(c) => format!("code {c}"),
        Exit::Raised(_) => "error".into(),
    }
}

/// Session-level facts that change the scenarios themselves, so flipping
/// them re-runs the analysis instead of only re-labelling verdicts
/// (design §9 "flipping one re-runs the analysis").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// `ASM-NO-CALLER-TXN` denied: the caller holds an open transaction.
    pub caller_txn: bool,
    /// `ASM-TSQL-CALLER-XACT-ABORT-OFF` denied: the caller runs with XACT_ABORT ON.
    pub xact_abort: bool,
    /// Db2 for i commit level declared for routines that do not state one
    /// (`ASM-DB2-COMMIT-LEVEL` in a pack).
    pub commit_level: Option<String>,
    /// Build no witnesses (`Options::skip_witnesses`).
    pub skip_witnesses: bool,
    /// Budgets lowered by `Options::work_budgets` (routine, file).
    pub work_budgets: Option<(u64, u64)>,
    /// Work caps replaced by `Options::work_caps` (unit, file).
    pub work_caps: Option<(u64, u64)>,
    /// The condition pairs (start offset on side A, on side B) the identity
    /// analysis proved to be the same fact about the input
    /// (`equiv::identity`, perf plan W6c E2). `None`: no analysis ran for
    /// this pair of files, and equal condition text is matched as before.
    /// `Some`: `pairs()` and `feasible()` match a condition's text across
    /// the sides only when every condition with that text is in such a pair.
    pub identified: Option<std::sync::Arc<BTreeSet<(usize, usize)>>>,
    /// Where stored pair results are kept (`PairCache`); a performance
    /// handle, never part of what a pair result depends on.
    pub pair_cache: Option<PairCacheHandle>,
}

/// A store of routine-pair results (W4.4 step 2). Bytes in, bytes out: the
/// caller frames, verifies and evicts; the reuse decision is made in
/// [`analyze_runs`], never here.
pub trait PairCache: Send + Sync {
    /// The build the keys are made under (part of every key).
    fn build(&self) -> &str;
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn put(&self, key: &str, bytes: &[u8]);
    /// Whether a result about to be reused is built again and compared
    /// (the sampled re-check).
    fn recheck(&self, key: &str) -> bool;
    /// What happened to a pair, for the report.
    fn note(&self, event: PairEvent, key: &str);
}

/// What `analyze_runs` did with one pair, reported to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairEvent {
    /// Taken from the cache.
    Reused,
    /// Built (not found, or the entry state did not allow the stored one).
    Built,
    /// Taken, built again and found equal.
    Verified,
    /// Taken, built again and found different: the fresh result was used.
    Mismatch,
}

#[derive(Clone)]
pub struct PairCacheHandle(pub std::sync::Arc<dyn PairCache>);

impl std::fmt::Debug for PairCacheHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairCacheHandle")
    }
}

impl PartialEq for PairCacheHandle {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PairCacheHandle {}

/// Assumptions whose flip changes the scenarios (and so is re-run, not relabelled).
pub const RERUN_ASSUMPTIONS: [&str; 2] = ["ASM-NO-CALLER-TXN", "ASM-TSQL-CALLER-XACT-ABORT-OFF"];

impl Env {
    /// The environment the flips describe: id → holds.
    pub fn from_flips(flips: &BTreeMap<String, bool>) -> Env {
        Env {
            caller_txn: flips.get("ASM-NO-CALLER-TXN") == Some(&false),
            xact_abort: flips.get("ASM-TSQL-CALLER-XACT-ABORT-OFF") == Some(&false),
            commit_level: None,
            skip_witnesses: false,
            work_budgets: None,
            work_caps: None,
            identified: None,
            pair_cache: None,
        }
    }

    /// The environment of a diff: session flips plus declared facts.
    pub fn from_options(opts: &crate::Options) -> Env {
        Env {
            commit_level: opts
                .declared
                .get("ASM-DB2-COMMIT-LEVEL")
                .and_then(|t| crate::ledger::declared_commit_level(t)),
            skip_witnesses: opts.skip_witnesses,
            work_budgets: opts.work_budgets,
            work_caps: opts.work_caps,
            identified: None,
            pair_cache: opts.pair_cache.clone(),
            ..Env::from_flips(&opts.flips)
        }
    }

    fn scenario(&self) -> Scenario {
        Scenario {
            caller_txn: self.caller_txn,
            xact_abort: self.xact_abort,
            commit_level: self.commit_level.clone(),
            ..Scenario::default()
        }
    }
}

#[derive(Debug, Clone)]
enum Kind {
    Fault {
        key: String,
        kind: ErrorKind,
        na: NodeId,
        nb: NodeId,
    },
    Baseline,
    CallerTxn,
}

/// A statement on one side that the other side splits into several: a MERGE
/// whose branch (`THEN DELETE`, ...) the other side runs as its own statement
/// on the same table (design §7 upserts and MERGE).
#[derive(Debug, Clone)]
struct Split {
    /// The single statement is on side A.
    merge_on_a: bool,
    merge_key: String,
    /// The other side's separate statement for the branch.
    part_key: String,
}

struct Div {
    rule: &'static str,
    class: ChangeClass,
    severity: Severity,
    anchor: String,
    /// Anchor on side B when it is not the same effect key.
    anchor_b: Option<String>,
    text: String,
    assumptions: Vec<String>,
    /// Depends on the fate of effects (capped by `durability_unknown`).
    durability: bool,
}

impl Div {
    fn new(
        rule: &'static str,
        class: ChangeClass,
        severity: Severity,
        anchor: &str,
        text: String,
    ) -> Div {
        Div {
            rule,
            class,
            severity,
            anchor: anchor.to_string(),
            anchor_b: None,
            text,
            assumptions: Vec::new(),
            durability: false,
        }
    }
}

/// The statement that runs after the injected fault, if any.
fn after_fault(o: &Outcome) -> Option<NodeId> {
    o.fault_index.and_then(|i| o.trace.get(i + 1)).copied()
}

/// The routine carries on past the failing statement without a handler and
/// runs more of its own statements: the caller's error status (`@@ERROR`) is
/// reset by them, so only the message tells the caller something failed.
fn continues(o: &Outcome) -> bool {
    matches!(o.fault_route, Some(Routed::Next(_)))
        && after_fault(o).is_some()
        && !matches!(o.exit, Exit::Raised(_))
}

/// The slots on which two shared fate arrays differ, remembered per pair of
/// arrays: an outcome that merged into the fault-free run shares that
/// run's array, so comparing two of them starts from this and costs the
/// slots they changed.
///
/// One cache serves a whole routine pair. It is keyed by the arrays'
/// identity (a pair of `Rc` addresses) and keeps the arrays alive, so an
/// address is never reused while its entry exists. Eviction clears
/// everything once the arrays held reach `BYTES`; it only costs time, the
/// differences are a pure function of the two arrays.
#[derive(Default)]
struct DiffCache {
    #[allow(clippy::type_complexity)]
    pairs: RefCell<(
        HashMap<(usize, usize), (Rc<Vec<u8>>, Rc<Vec<u8>>, Rc<Vec<u32>>)>,
        usize,
    )>,
}

impl DiffCache {
    /// Bytes of fate arrays the cache may keep alive before it starts over.
    const BYTES: usize = 64 << 20;

    fn of(&self, a: &Rc<Vec<u8>>, b: &Rc<Vec<u8>>) -> Rc<Vec<u32>> {
        let mut c = self.pairs.borrow_mut();
        let key = (Rc::as_ptr(a) as usize, Rc::as_ptr(b) as usize);
        if let Some((_, _, d)) = c.0.get(&key) {
            return d.clone();
        }
        let d = Rc::new(scan_differing(a, b));
        let cost = a.len() + b.len() + 4 * d.len() + 64;
        if c.1 + cost > Self::BYTES {
            c.0.clear();
            c.1 = 0;
        }
        c.1 += cost;
        c.0.insert(key, (a.clone(), b.clone(), d.clone()));
        d
    }
}

const NO_POS: u32 = u32::MAX;

/// What the diff of two sides' outcomes looks up for every key, by effect
/// slot, and the differences of shared fate arrays already found.
struct Align {
    dc: DiffCache,
    /// Document-order position of the first effect with each key on each
    /// side (`NO_POS` for none).
    pos_a: Vec<u32>,
    pos_b: Vec<u32>,
}

impl Align {
    fn new(keys: &KeyUniverse, sa: &SideIr, sb: &SideIr) -> Align {
        let pos = |s: &SideIr| -> Vec<u32> {
            keys.names()
                .iter()
                .map(|k| s.at.get(&**k).map_or(NO_POS, |&(p, _)| p as u32))
                .collect()
        };
        Align {
            dc: DiffCache::default(),
            pos_a: pos(sa),
            pos_b: pos(sb),
        }
    }
}

enum Differing {
    /// Effect slots, when both outcomes number their effects alike.
    Slots(Vec<usize>),
    Names(Vec<Rc<str>>),
}

/// The effects whose fate differs between two outcomes (including effects
/// that ran in only one), in no particular order.
fn differing(a: &Effects, b: &Effects, al: &Align) -> Differing {
    if let Some(v) = a.differing(b, &|x, y| al.dc.of(x, y)) {
        return Differing::Slots(v);
    }
    // Not over the same keys: compare by name.
    let mut out = Vec::new();
    let (mut ia, mut ib) = (a.iter().peekable(), b.iter().peekable());
    loop {
        match (ia.peek(), ib.peek()) {
            (None, None) => return Differing::Names(out),
            (Some(&(ka, fa)), Some(&(kb, fb))) if ka == kb => {
                if fa != fb {
                    out.push(ka.clone());
                }
                ia.next();
                ib.next();
            }
            (Some(&(ka, _)), Some(&(kb, _))) if ka < kb => {
                out.push(ka.clone());
                ia.next();
            }
            (Some(&(ka, _)), None) => {
                out.push(ka.clone());
                ia.next();
            }
            (_, Some(&(kb, _))) => {
                out.push(kb.clone());
                ib.next();
            }
        }
    }
}

/// Temporary objects an outcome reads, by read key: (key, object, state seen).
type Reads = BTreeSet<(String, String, TempState)>;

fn reads(s: &SideIr, o: &Outcome) -> Reads {
    o.temp_reads
        .iter()
        .filter_map(|(n, t, st)| Some((s.read_key(*n, t)?, t.clone(), *st)))
        .collect()
}

/// Differences between one outcome of each side under one scenario. The
/// text of a divergence is written only when `need(rule, anchor)`: a caller
/// that already has the divergence from another outcome pair only counts it,
/// and writing the text for every pair of a branchy routine was most of the
/// work.
#[allow(clippy::too_many_arguments)]
fn diff(
    sa: &SideIr,
    sb: &SideIr,
    splits: &[Split],
    sc: &Kind,
    (oa, ra): (&Outcome, &Reads),
    (ob, rb): (&Outcome, &Reads),
    need: &dyn Fn(&str, &str) -> bool,
    al: &Align,
) -> Vec<Div> {
    let text = |rule: &str, anchor: &str, f: &dyn Fn() -> String| {
        if need(rule, anchor) {
            f()
        } else {
            String::new()
        }
    };
    let mut out = Vec::new();
    let fault_key = match sc {
        Kind::Fault { key, .. } => Some(key.as_str()),
        _ => None,
    };
    // 1. The failing statement's own rows.
    if let Some(fk) = fault_key {
        let (fa, fb) = (oa.effects.get(fk), ob.effects.get(fk));
        if (fa == Some(&Fate::Partial)) != (fb == Some(&Fate::Partial)) {
            let (p, q) = if fa == Some(&Fate::Partial) {
                (sa, sb)
            } else {
                (sb, sa)
            };
            let mut d = Div::new(
                "txn.statement-atomicity",
                ChangeClass::Transactional,
                Severity::High,
                fk,
                text("txn.statement-atomicity", fk, &|| {
                    format!(
                    "{} rolls the statement back; {} keeps the rows it changed before the error, because statements are atomic only under commitment control",
                    q.name(),
                    p.name()
                )
                }),
            );
            d.assumptions = vec!["ASM-MULTI-ROW".into()];
            d.durability = true;
            out.push(d);
        }
    }
    // 2a. The caller's own uncommitted work (caller-transaction scenario).
    let caller = super::interp::CALLER;
    let (ca, cb) = (
        oa.effects.get(caller).copied(),
        ob.effects.get(caller).copied(),
    );
    if (ca.is_some() || cb.is_some()) && ca != cb {
        let caller_phrase = |f: Option<Fate>| match f {
            Some(Fate::Durable) => "commits the caller's earlier work",
            Some(Fate::Undone) => "rolls back the caller's earlier work",
            Some(Fate::Caller) => "leaves the caller's earlier work to the caller",
            Some(Fate::Partial) | None => "leaves the caller's transaction in an unknown state",
        };
        // The caller's work ran before anything here: under a caller
        // transaction the environment asserts, a fault makes it atomicity.
        let rule = match sc {
            Kind::CallerTxn => "txn.caller-transaction",
            Kind::Fault { .. } => "txn.atomicity",
            _ => "txn.durability",
        };
        let mut d = Div::new(
            rule,
            ChangeClass::Transactional,
            Severity::Critical,
            caller,
            text(rule, caller, &|| {
                format!(
                    "{} {}; {} {}",
                    sa.name(),
                    caller_phrase(ca),
                    sb.name(),
                    caller_phrase(cb)
                )
            }),
        );
        d.durability = true;
        out.push(d);
    }
    // 2. Effects that ran on both sides with different fates; the first of
    // each kind in document order is reported.
    type First = Option<((usize, usize), String, Option<Fate>, Option<Fate>)>;
    let (mut first_fate, mut first_control): (First, First) = (None, None);
    {
        // Slot by slot when both outcomes number their effects alike; the
        // position of each key in both sides is looked up once per pair.
        let fault_slot = fault_key.and_then(|k| oa.effects.index_of(k));
        let mut consider = |ia: usize, ib: usize, name: &dyn Fn() -> String, fa, fb| {
            let slot = match (fa, fb) {
                (Some(_), Some(_)) => &mut first_fate,
                _ => &mut first_control,
            };
            if !matches!(slot, Some((o, ..)) if *o <= (ia, ib)) {
                *slot = Some(((ia, ib), name(), fa, fb));
            }
        };
        match differing(&oa.effects, &ob.effects, al) {
            Differing::Slots(v) => {
                for i in v {
                    if Some(i) == fault_slot {
                        continue;
                    }
                    let (ia, ib) = (al.pos_a[i], al.pos_b[i]);
                    if ia == NO_POS || ib == NO_POS {
                        continue;
                    }
                    consider(
                        ia as usize,
                        ib as usize,
                        &|| oa.effects.name(i).to_string(),
                        oa.effects.fate_at(i),
                        ob.effects.fate_at(i),
                    );
                }
            }
            Differing::Names(v) => {
                for k in v {
                    let k = &*k;
                    if Some(k) == fault_key {
                        continue;
                    }
                    let (Some(&(ia, _)), Some(&(ib, _))) = (sa.at.get(k), sb.at.get(k)) else {
                        continue;
                    };
                    consider(
                        ia,
                        ib,
                        &|| k.to_string(),
                        oa.effects.get(k).copied(),
                        ob.effects.get(k).copied(),
                    );
                }
            }
        }
    }
    let fate_diffs: Vec<_> = first_fate
        .map(|(_, k, fa, fb)| {
            let ran_before = oa.before_fault.contains(&k) || ob.before_fault.contains(&k);
            (k, fa, fb, ran_before)
        })
        .into_iter()
        .collect();
    let control_diffs: Vec<_> = first_control
        .map(|(_, k, fa, fb)| (k, fa, fb))
        .into_iter()
        .collect();
    let scenario_rule = |before: bool| match sc {
        Kind::CallerTxn => "txn.caller-transaction",
        Kind::Fault { .. } if before => "txn.atomicity",
        _ => "txn.durability",
    };
    if let Some((k, fa, fb, before)) = fate_diffs.first() {
        let mut asm = vec!["ASM-NO-TRIGGERS".to_string()];
        if (matches!(fa, Some(Fate::Caller)) || matches!(fb, Some(Fate::Caller)))
            && matches!(sc, Kind::Fault { .. })
        {
            asm.push("ASM-CALLER-MAY-COMMIT-AFTER-ERROR".into());
        }
        let mut d = Div::new(
            scenario_rule(*before),
            ChangeClass::Transactional,
            Severity::Critical,
            k,
            text(scenario_rule(*before), k, &|| {
                format!(
                    "{} {}; {} {}",
                    sa.name(),
                    fate_phrase(sa, k, *fa),
                    sb.name(),
                    fate_phrase(sb, k, *fb)
                )
            }),
        );
        d.assumptions = asm;
        d.durability = true;
        out.push(d);
    }
    // 3. Effects that run on one side only (after the fault): the control
    // target differs (semantic), and when the extra statement's work stays,
    // so does the durable state (transactional).
    if fault_key.is_some() {
        if let Some((k, fa, fb)) = control_diffs.first() {
            let text = if need("error.control", k) || need("txn.durability", k) {
                format!(
                    "{} {}; {} {}",
                    sa.name(),
                    fate_phrase(sa, k, *fa),
                    sb.name(),
                    fate_phrase(sb, k, *fb)
                )
            } else {
                String::new()
            };
            out.push(Div::new(
                "error.control",
                ChangeClass::Semantic,
                Severity::High,
                k,
                text.clone(),
            ));
            let persisted = matches!(fa, Some(Fate::Durable | Fate::Partial))
                || matches!(fb, Some(Fate::Durable | Fate::Partial));
            if persisted {
                let mut d = Div::new(
                    "txn.durability",
                    ChangeClass::Transactional,
                    Severity::Critical,
                    k,
                    text,
                );
                d.assumptions = vec!["ASM-NO-TRIGGERS".into()];
                d.durability = true;
                out.push(d);
            }
        }
    }
    // 4. How the routine ends.
    let exit_differs = exit_class(oa) != exit_class(ob);
    if exit_differs && !matches!(sc, Kind::Baseline) {
        let mut asm = Vec::new();
        if matches!(oa.exit, Exit::Code(_)) || matches!(ob.exit, Exit::Code(_)) {
            asm.push("ASM-CALLER-CHECKS-RC".to_string());
        }
        let rule = if matches!(sc, Kind::CallerTxn) {
            "txn.caller-transaction"
        } else {
            "error.contract"
        };
        let mut d = Div::new(
            rule,
            if rule == "error.contract" {
                ChangeClass::Semantic
            } else {
                ChangeClass::Transactional
            },
            Severity::High,
            fault_key.unwrap_or("exit"),
            text(rule, fault_key.unwrap_or("exit"), &|| {
                format!(
                    "{} {}; {} {}",
                    sa.name(),
                    exit_phrase(oa),
                    sb.name(),
                    exit_phrase(ob)
                )
            }),
        );
        d.assumptions = asm;
        out.push(d);
    }
    // 5. Whether the caller's unit of work survives the failure: one side
    // reports the error and carries on (so the caller's batch goes on and can
    // commit), the other raises it (the calling statement fails and, in a
    // transaction block, the caller's transaction is aborted).
    if let Some(fk) = fault_key {
        let pair = if continues(oa) && matches!(ob.exit, Exit::Raised(_)) {
            Some((sa, oa, sb, ob))
        } else if continues(ob) && matches!(oa.exit, Exit::Raised(_)) {
            Some((sb, ob, sa, oa))
        } else {
            None
        };
        if let Some((c, oc, x, _)) = pair {
            let t = text("txn.caller-continuation", fk, &|| {
                let next = after_fault(oc)
                    .map(|n| c.line(n))
                    .filter(|l| !l.is_empty())
                    .map(|l| format!(" ({l} runs next)"))
                    .unwrap_or_default();
                let ends = match &oc.exit {
                    Exit::Code(code) => format!("returns code {code}"),
                    _ => "returns normally".to_string(),
                };
                format!(
                    "{} reports the error and carries on{next}; the call {ends}, so the caller's batch goes on and can commit its work; {} raises the error, so the calling statement fails and a caller's transaction block is aborted",
                    c.name(),
                    x.name()
                )
            });
            out.push(Div::new(
                "txn.caller-continuation",
                ChangeClass::Transactional,
                Severity::High,
                fk,
                t,
            ));
        }
    }
    // 6. A statement split in two: when the single statement fails, it
    // changes nothing; the split side has already run its first part.
    if let Some(fk) = fault_key {
        for s in splits.iter().filter(|s| s.merge_key == fk) {
            let (m, om, p, op) = if s.merge_on_a {
                (sa, oa, sb, ob)
            } else {
                (sb, ob, sa, oa)
            };
            let kept = matches!(
                op.effects.get(s.part_key.as_str()),
                Some(Fate::Durable | Fate::Partial)
            );
            let merge_kept = matches!(
                om.effects.get(s.merge_key.as_str()),
                Some(Fate::Durable | Fate::Partial)
            );
            if kept && !merge_kept && op.before_fault.contains(s.part_key.as_str()) {
                let mut d = Div::new(
                    "txn.atomicity",
                    ChangeClass::Transactional,
                    Severity::Critical,
                    &s.merge_key,
                    text(
                        "txn.atomicity",
                        if s.merge_on_a {
                            &s.merge_key
                        } else {
                            &s.part_key
                        },
                        &|| {
                            format!(
                            "{} does it in one statement, which fails as a whole and changes nothing; {} has already run {}, which stays durable",
                            m.name(),
                            p.name(),
                            p.phrase_at(&s.part_key)
                        )
                        },
                    ),
                );
                if !s.merge_on_a {
                    d.anchor = s.part_key.clone();
                    d.anchor_b = Some(s.merge_key.clone());
                } else {
                    d.anchor_b = Some(s.part_key.clone());
                }
                d.assumptions = vec!["ASM-NO-TRIGGERS".into()];
                d.durability = true;
                out.push(d);
            }
        }
    }
    // 7. Temporary state seen by aligned statements. One finding per
    // statement (the first temporary table that differs): the scenario
    // counts a divergence once per outcome pair.
    let mut reported: Vec<&String> = Vec::new();
    for (k, t, _) in ra.symmetric_difference(rb) {
        if reported.contains(&k) || sa.locate(k).is_none() || sb.locate(k).is_none() {
            continue;
        }
        reported.push(k);
        let state = |r: &Reads| {
            r.iter()
                .find(|(x, y, _)| x == k && y == t)
                .map(|(_, _, s)| match s {
                    TempState::Rows => "still holds the rows written earlier",
                    TempState::Empty => "is empty (the rows were rolled back or never written)",
                })
                .unwrap_or("is not read")
        };
        out.push(Div::new(
            "txn.temp-state",
            ChangeClass::Transactional,
            Severity::High,
            k,
            text("txn.temp-state", k, &|| {
                format!(
                    "{} reads the temporary table {}, which on {} {} and on {} {}",
                    sa.phrase(k),
                    t.to_lowercase(),
                    sa.name(),
                    state(ra),
                    sb.name(),
                    state(rb)
                )
            }),
        ));
    }
    out
}

fn path_text(o: &Outcome) -> String {
    if o.signature.is_empty() {
        return String::new();
    }
    let conds: Vec<String> = o
        .signature
        .iter()
        .map(|(c, b)| {
            format!(
                "`{}` is {}",
                super::interp::plain_cond(c).to_lowercase(),
                if *b { "true" } else { "false" }
            )
        })
        .collect();
    format!(" on the path where {}", conds.join(" and "))
}

fn scenario_intro(sa: &SideIr, sb: &SideIr, sc: &Kind, oa: &Outcome) -> String {
    let path = path_text(oa);
    match sc {
        Kind::Fault { key, kind, na, nb } => {
            let (la, lb) = (sa.line(*na), sb.line(*nb));
            let what = if sa.effect(key).is_some() {
                sa.phrase(key)
            } else {
                sa.stmt_phrase(*na)
            };
            format!("If {what} fails with {} ({la}; {lb}){path}", kind.phrase())
        }
        Kind::Baseline if path.is_empty() => "When every statement succeeds".to_string(),
        Kind::Baseline => format!("When no statement fails{path}"),
        Kind::CallerTxn => format!("If the caller already holds an open transaction{path}"),
    }
}

fn scenario_info(sa: &SideIr, sb: &SideIr, sc: &Kind, env: &Env, oa: &Outcome) -> ScenarioInfo {
    let (kind, statement, error_kind, line_a, line_b) = match sc {
        Kind::Fault { key, kind, na, nb } => (
            ScenarioKind::Fault,
            Some(if sa.effect(key).is_some() {
                sa.phrase(key)
            } else {
                sa.stmt_phrase(*na)
            }),
            Some(*kind),
            sa.line_no(*na),
            sb.line_no(*nb),
        ),
        Kind::Baseline => (ScenarioKind::Baseline, None, None, None, None),
        Kind::CallerTxn => (ScenarioKind::CallerTransaction, None, None, None, None),
    };
    ScenarioInfo {
        kind,
        statement,
        error_kind,
        line_a,
        line_b,
        caller_transaction: env.caller_txn || matches!(sc, Kind::CallerTxn),
        xact_abort: env.xact_abort,
        path: oa
            .signature
            .iter()
            .map(|(c, v)| PathStep {
                condition: super::interp::plain_cond(c).to_lowercase(),
                value: *v,
            })
            .collect(),
        text: scenario_intro(sa, sb, sc, oa),
    }
}

fn construct_phrase(c: &str) -> &'static str {
    match c {
        "try_catch" => "CATCH block",
        "exception_block" => "EXCEPTION clause",
        "continue_handler" => "CONTINUE handler",
        "exit_handler" => "EXIT handler",
        "undo_handler" => "UNDO handler",
        _ => "handler",
    }
}

/// The outcome tuple of one side, for structured witnesses and `what_if`.
fn side_outcome(s: &SideIr, o: &Outcome) -> SideOutcome {
    let next = after_fault(o).and_then(|n| s.line_no(n));
    let control = match &o.fault_route {
        None => "runs to the end".to_string(),
        Some(Routed::Handler { construct, .. }) => match next {
            Some(l) => format!("enters the {} (line {l})", construct_phrase(construct)),
            None => format!("enters the {}", construct_phrase(construct)),
        },
        Some(Routed::Next(_)) => match next {
            Some(l) => format!("reports the error and goes on with the next statement (line {l})"),
            None => "reports the error and goes on; nothing else runs".to_string(),
        },
        Some(Routed::Exit) => "leaves the routine with the error".to_string(),
    };
    let mut so = SideOutcome {
        control,
        exit: exit_phrase(o),
        ..SideOutcome::default()
    };
    for (k, f) in o.effects.iter() {
        let p = if &**k == super::interp::CALLER {
            "the caller's earlier work".to_string()
        } else {
            s.phrase_at(k)
        };
        match f {
            Fate::Durable => so.durable.push(p),
            Fate::Undone => so.undone.push(p),
            Fate::Partial => so.partial.push(p),
            Fate::Caller => so.left_to_caller.push(p),
        }
    }
    let rolled: Vec<String> = o
        .effects
        .iter()
        .filter(|(k, f)| **f == Fate::Undone && o.before_fault.contains(k))
        .map(|(k, _)| {
            if &**k == super::interp::CALLER {
                "the caller's earlier work".to_string()
            } else {
                s.phrase(k)
            }
        })
        .collect();
    so.rollback = if o.fault_index.is_none() {
        "nothing fails".to_string()
    } else if rolled.is_empty() {
        if o.partial.is_empty() {
            "the failing statement only".to_string()
        } else {
            "nothing: the failing statement keeps the rows it changed".to_string()
        }
    } else {
        format!("the failing statement and {}", rolled.join(", "))
    };
    so.transaction = if o.doomed {
        "the transaction is doomed: it can only be rolled back".to_string()
    } else if o.left_open {
        format!(
            "leaves a transaction it opened still open (depth {})",
            o.depth
        )
    } else if !o.open.is_empty() {
        "work is pending in a transaction the caller owns".to_string()
    } else {
        "no transaction left open".to_string()
    };
    let mut path: Vec<usize> = o.trace.iter().filter_map(|n| s.line_no(*n)).collect();
    path.dedup();
    so.path = path;
    so.not_proven = o.imprecise.iter().map(|i| i.to_string()).collect();
    so.not_proven.extend(o.durability_unknown.iter().cloned());
    so
}

/// Folded text of a condition, so paths of two dialects can be matched:
/// case, quoting, default schemas and parameter prefixes are not compared.
pub(crate) fn norm_cond(c: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let toks: Vec<String> = c
        .to_lowercase()
        .replace(['[', ']', '"', '`'], "")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if matches!(t.as_str(), "dbo" | "public") && toks.get(i + 1).is_some_and(|x| x == ".") {
            i += 2;
            continue;
        }
        let t = t.trim_start_matches('@');
        let t = t.strip_prefix("p_").unwrap_or(t);
        out.push(t.to_string());
        i += 1;
    }
    out.join(" ")
}

/// Statements of a unit: the larger count of the two sides' statement nodes.
fn statements_of(sa: &SideIr, sb: &SideIr) -> u64 {
    let n = |s: &SideIr| s.cfg.nodes.iter().filter(|n| n.stmt().is_some()).count() as u64;
    n(sa).max(n(sb))
}

/// `statements_of` for a routine pair before its collector exists (the file
/// cap needs the largest unit cap first). Lowers both sides once more.
fn pair_statements(
    a: &Analyzed,
    b: &Analyzed,
    ra: Option<SyntaxNode>,
    rb: Option<SyntaxNode>,
) -> u64 {
    let n = |x: &Analyzed, y: &Analyzed, r: Option<SyntaxNode>| {
        let cfg = super::cfg::lower_routine(x, other_temps(x, y), r);
        cfg.nodes.iter().filter(|n| n.stmt().is_some()).count() as u64
    };
    n(a, b, ra).max(n(b, a, rb))
}

/// T-SQL compared against PostgreSQL or Db2 for i: the other side's
/// temporary tables, which a table variable may have been converted to.
/// Read from `other`'s cached set (`Analyzed::temp_table_names`), so the
/// whole-file scan runs once per `Analyzed`, not once per pair.
fn other_temps(x: &Analyzed, other: &Analyzed) -> HashSet<String> {
    if x.dialect() == Dialect::Tsql && other.dialect() != Dialect::Tsql {
        other.temp_table_names().clone()
    } else {
        HashSet::new()
    }
}

/// The unknown tests of a graph by folded condition text (`norm_cond`), with
/// the start offset of each.
fn tests_by_text(c: &Cfg) -> BTreeMap<String, Vec<usize>> {
    let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for n in &c.nodes {
        if let super::cfg::Op::Test {
            test: super::cfg::Test::Unknown(t),
            ..
        } = &n.op
        {
            // A test with no source position cannot be identified.
            let at = n
                .syntax
                .as_ref()
                .map_or(usize::MAX, |s| sqldiff_syntax::ast::sig_range(s).0);
            m.entry(norm_cond(t)).or_default().push(at);
        }
    }
    m
}

/// Perf plan W6c, E2: `pairs()` and `feasible()` treat two conditions with
/// the same folded text as one decision. That is only right when the text is
/// the same fact about the input on both sides. A text keeps matching only
/// if every condition with it, on either side, is in a pair `done` holds
/// with a condition of the same text on the other side; the tests of every
/// other text are marked (`opaque_cond`), so no signature of A equals one of
/// B by them: the exact pairing falls back to all combinations (the verdict
/// is then capped unless all combinations agree) and `feasible` keeps the
/// pair. Verdicts only get stricter.
fn mark_unidentified(a: &mut Cfg, b: &mut Cfg, done: &BTreeSet<(usize, usize)>) {
    let (ta, tb) = (tests_by_text(a), tests_by_text(b));
    // Which tests (by start offset and text) have a counterpart in `done`
    // with the same text: read off `done` once, not searched for per test.
    fn by_pos(t: &BTreeMap<String, Vec<usize>>) -> BTreeMap<usize, Vec<&String>> {
        let mut m: BTreeMap<usize, Vec<&String>> = BTreeMap::new();
        for (text, xs) in t {
            for &x in xs {
                m.entry(x).or_default().push(text);
            }
        }
        m
    }
    let (pa, pb) = (by_pos(&ta), by_pos(&tb));
    let mut covered_a: BTreeSet<(usize, &String)> = BTreeSet::new();
    let mut covered_b: BTreeSet<(usize, &String)> = BTreeSet::new();
    for &(x, y) in done {
        let (Some(tx), Some(ty)) = (pa.get(&x), pb.get(&y)) else {
            continue;
        };
        for text in tx.iter().filter(|t| ty.contains(t)) {
            covered_a.insert((x, text));
            covered_b.insert((y, text));
        }
    }
    let ident: BTreeSet<&String> = ta
        .iter()
        .filter(|(text, xs)| {
            tb.get(*text).is_some_and(|ys| {
                xs.iter().all(|x| covered_a.contains(&(*x, *text)))
                    && ys.iter().all(|y| covered_b.contains(&(*y, *text)))
            })
        })
        .map(|(t, _)| t)
        .collect();
    for (cfg, is_a) in [(a, true), (b, false)] {
        for n in &mut cfg.nodes {
            if let super::cfg::Op::Test {
                test: super::cfg::Test::Unknown(t),
                ..
            } = &mut n.op
            {
                if !ident.contains(&norm_cond(t)) {
                    *t = super::interp::opaque_cond(t, is_a).into();
                }
            }
        }
    }
}

/// Pairs the outcomes of both sides: by path signature when both sides
/// branch on the same conditions, otherwise every combination (and the
/// verdict is capped at unknown unless all combinations agree).
fn pairs(pa: &[Outcome], pb: &[Outcome]) -> (Vec<(usize, usize)>, bool) {
    if pa.len() == 1 && pb.len() == 1 {
        return (vec![(0, 0)], true);
    }
    // Each distinct condition text is folded once and each folded text gets
    // an id, so two ids are equal exactly when the folded texts are. Ids
    // only serve equality and lookups here; nothing is ordered by them.
    let mut raw_ids: HashMap<&str, u32> = HashMap::new();
    let mut folded_ids: HashMap<String, u32> = HashMap::new();
    fn sig_of<'a>(
        o: &'a Outcome,
        raw_ids: &mut HashMap<&'a str, u32>,
        folded_ids: &mut HashMap<String, u32>,
    ) -> Vec<(u32, bool)> {
        o.signature
            .iter()
            .map(|(c, b)| {
                let id = match raw_ids.get(&**c) {
                    Some(&id) => id,
                    None => {
                        let f = norm_cond(c);
                        let next = folded_ids.len() as u32;
                        let id = *folded_ids.entry(f).or_insert(next);
                        raw_ids.insert(&**c, id);
                        id
                    }
                };
                (id, *b)
            })
            .collect()
    }
    let siga: Vec<Vec<(u32, bool)>> = pa
        .iter()
        .map(|o| sig_of(o, &mut raw_ids, &mut folded_ids))
        .collect();
    let sigb: Vec<Vec<(u32, bool)>> = pb
        .iter()
        .map(|o| sig_of(o, &mut raw_ids, &mut folded_ids))
        .collect();
    // Outcome indices ordered by (signature, index): the first outcome with a
    // signature is found by binary search, and equal sets by a dedup walk.
    let order = |s: &[Vec<(u32, bool)>]| -> Vec<usize> {
        let mut ix: Vec<usize> = (0..s.len()).collect();
        ix.sort_unstable_by(|&x, &y| s[x].cmp(&s[y]).then(x.cmp(&y)));
        ix
    };
    let (oa, ob) = (order(&siga), order(&sigb));
    let distinct = |s: &[Vec<(u32, bool)>], o: &[usize]| -> Vec<usize> {
        let mut d: Vec<usize> = Vec::new();
        for &i in o {
            match d.last() {
                Some(&l) if s[l] == s[i] => {}
                _ => d.push(i),
            }
        }
        d
    };
    let (da, db) = (distinct(&siga, &oa), distinct(&sigb, &ob));
    if da.len() == db.len() && da.iter().zip(&db).all(|(&i, &j)| siga[i] == sigb[j]) {
        // The first outcome of B with each signature.
        let v = siga
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let at = ob.partition_point(|&j| sigb[j] < *s);
                (i, ob[at])
            })
            .collect();
        return (v, true);
    }
    // Not the same set of paths: every pair of outcomes except those no input
    // can produce (see `feasible`).
    let (fa, fb): (Vec<_>, Vec<_>) = (
        siga.iter().map(|x| once_flags(x)).collect(),
        sigb.iter().map(|y| once_flags(y)).collect(),
    );
    let mut v = Vec::new();
    for (i, x) in siga.iter().enumerate() {
        for (j, y) in sigb.iter().enumerate() {
            if feasible_with(x, &fa[i], y, &fb[j]) {
                v.push((i, j));
            }
        }
    }
    (v, false)
}

/// Whether one input could take path `a` on side A and path `b` on side B.
/// The paths decide conditions in order; the same condition (after folding)
/// decided with two values makes the pair impossible. At the first position
/// where the two decide different conditions, nothing is known about the
/// rest (the sides may decide in another order, or a condition may be
/// another spelling of one the other side decides), so the pair stays. This
/// only ever removes pairs a real input cannot produce, so every input still
/// has its pair and a divergence shown by all remaining pairs is shown by
/// every input.
///
/// Conditions are identified by their text only: this assumes, as the exact
/// pairing does, that the same text has the same truth value on both sides.
/// A row-count test after a failed statement is named with the dialect, since
/// its value after an error differs between dialects.
#[cfg(test)]
fn feasible(a: &[(String, bool)], b: &[(String, bool)]) -> bool {
    feasible_with(a, &once_flags(a), b, &once_flags(b))
}

/// For each decision of a path: whether its condition is decided only there
/// on the path.
fn once_flags<K: Ord>(p: &[(K, bool)]) -> Vec<bool> {
    let mut keys: Vec<&K> = p.iter().map(|x| &x.0).collect();
    keys.sort_unstable();
    p.iter()
        .map(|x| keys.partition_point(|k| **k <= x.0) - keys.partition_point(|k| **k < x.0) == 1)
        .collect()
}

/// [`feasible`] with the paths' `once_flags` made once, not for every pair.
fn feasible_with<K: Eq>(a: &[(K, bool)], fa: &[bool], b: &[(K, bool)], fb: &[bool]) -> bool {
    for (k, (x, y)) in a.iter().zip(b).enumerate() {
        if x.0 != y.0 || !fa[k] || !fb[k] {
            return true;
        }
        if x.1 != y.1 {
            return false;
        }
    }
    true
}

/// Behavioural findings, the assumptions they rest on, and a structured
/// witness per finding.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Behaviour {
    pub findings: Vec<Finding>,
    pub assumptions: Vec<Assumption>,
    pub witnesses: Vec<Witness>,
    /// Every scenario ran on both sides, nothing in it was left unmodelled,
    /// and every finding is equal (under assumptions): where each side
    /// commits and rolls back was compared here, so statement-level
    /// equivalence need not refuse transaction statements between data
    /// statements.
    pub txn_compared: bool,
    /// Source ranges (byte offsets) of the routines on each side whose
    /// behaviour was compared exactly, as `txn_compared` says for a whole
    /// file: a transaction statement inside one of them need not be refused.
    pub txn_compared_a: Vec<std::ops::Range<usize>>,
    pub txn_compared_b: Vec<std::ops::Range<usize>>,
}

struct Collector<'x, 'a> {
    sa: SideIr<'x, 'a>,
    sb: SideIr<'x, 'a>,
    env: Env,
    splits: Vec<Split>,
    out: Behaviour,
    seen: HashSet<(String, String)>,
    /// Index in `out.findings` of the routine-wide finding of each rule.
    contract_at: HashMap<&'static str, usize>,
    /// Each side's fault-free runs, one per environment (the analysed one
    /// and those a flipped assumption re-runs under), which fault scenarios
    /// resume from.
    xa: RefCell<Vec<Rc<Explorer>>>,
    xb: RefCell<Vec<Rc<Explorer>>>,
    al: Align,
    /// The effect keys of both sides, which both sides' runs number alike.
    keys: KeyUniverse,
    /// Some scenario had no outcome on a side, or an outcome or finding
    /// that is not exact (see [`Behaviour::txn_compared`]).
    incomplete: bool,
    /// Fingerprints of the findings of the routines analysed before this one
    /// in the same files, and this routine's name: two routines whose
    /// statements start with the same words must not share a fingerprint.
    /// Shared with the file loop, which extends it only between pairs, when
    /// no collector holds it: read through `contains` only.
    prior: Rc<HashSet<String>>,
    /// Every unsalted print tested against `prior`, with the answer: a
    /// pair result depends on `prior` only through these.
    prior_tested: RefCell<BTreeMap<String, bool>>,
    salt: String,
    /// Statements where a path of some scenario was cut by a budget, per
    /// side (see `Outcome::cut`).
    cut_a: BTreeSet<NodeId>,
    cut_b: BTreeSet<NodeId>,
    /// The divergences of a scenario with a default flipped (`persists`),
    /// by scenario.
    flipped: RefCell<HashMap<String, Rc<Divergences>>>,
    /// What the work count has been charged for already (see
    /// `Collector::explorer`): the first request for an explorer or a flipped
    /// scenario is counted, repeats are not, so the count does not depend on
    /// whether a cache answered.
    charged: RefCell<BTreeSet<String>>,
    /// Diffs the first computation of each flipped scenario spent (see
    /// `persists`).
    flip_spent: RefCell<BTreeMap<String, u64>>,
    /// While a flipped scenario is recomputed: the diffs it may still make.
    replay: Cell<Option<u64>>,
    /// Keeps the unit open for the statement-level phase check.
    _phase: sqldiff_model::work::UnitPhase,
    /// `diff` calls spent by this routine pair, and whether a budget ran out.
    spent: Cell<u64>,
    out_of_budget: Cell<bool>,
    /// Which work budget ran out first (`out_of_budget` says only that one
    /// did), for the witness of the `equiv.budget` findings.
    cap_hit: RefCell<Option<String>>,
    /// `diff` calls spent by the whole file so far (shared by its routine
    /// pairs), and the budgets in force (routine, file).
    file_spent: Rc<Cell<u64>>,
    budgets: (u64, u64),
    /// The work caps (see `unit_work_cap`): this unit's allowance and where
    /// its count started, and the file's.
    unit_start: u64,
    unit_cap: u64,
    file_work: FileWork,
}

/// The metered-work allowance of one engine comparison's behaviour analysis
/// (one `analyze` call, not the outermost meter: `convert` checks the same
/// file in several comparisons and each gets a full allowance).
#[derive(Clone, Copy)]
struct FileWork {
    /// The meter's total when the analysis began.
    start: u64,
    /// The file cap before the unit's own cap is taken into account.
    cap: u64,
    /// Work the file had already done when this pair began, when the pair
    /// runs alone (`analyze_one_pair`): its meter reads as it would inside
    /// the file. Zero in a whole-file analysis.
    offset: u64,
}

/// Work cap of one unit (a routine pair, or the statements outside any
/// routine): `max(2^24, 2^15 x statements)`. The statement part keeps a long
/// one-transaction script, whose fault replays are quadratic in its
/// statements, below its cap up to the explorer's own step budget (Davy,
/// 2026-11-07). One place, so the formula can change without touching the
/// checks.
pub const UNIT_WORK_FLOOR: u64 = 1 << 24;
pub const UNIT_WORK_PER_STATEMENT: u64 = 1 << 15;
pub fn unit_work_cap(statements: u64) -> u64 {
    UNIT_WORK_FLOOR.max(UNIT_WORK_PER_STATEMENT.saturating_mul(statements))
}

/// Work cap of a file's analysis: `max(2^26, 2^20 x units)`, and never below
/// the largest unit cap (otherwise a unit would be cut by the file before its
/// own allowance).
pub const FILE_WORK_FLOOR: u64 = 1 << 26;
pub const FILE_WORK_PER_UNIT: u64 = 1 << 20;
pub fn file_work_cap(units: u64, largest_unit_cap: u64) -> u64 {
    FILE_WORK_FLOOR
        .max(FILE_WORK_PER_UNIT.saturating_mul(units))
        .max(largest_unit_cap)
}

/// What one check of the cap may overshoot by: an `outcomes` call can make two
/// first-request explorer builds and two `explore` calls, each bounded by
/// `MAX_PATHS` x `MAX_STEPS` plus `JOIN_TICKS` (about 0.7M steps), plus the
/// unmetered `reads` and `pairs`. About 2.8M, 17% of the smallest unit cap.
#[doc(hidden)]
pub const CAP_OVERSHOOT: u64 = 2_800_000;

/// The (rule, anchor) of divergences found.
type Divergences = BTreeSet<(&'static str, String)>;

/// Flipped scenarios kept before the cache starts over (time only).
const FLIPPED_MAX: usize = 4096;

thread_local! {
    /// Test hook: answer every request without the explorer and flipped
    /// caches. It changes no result and no work count (see
    /// `Collector::explorer`); the work-meter tests use it to show that.
    static CACHES_OFF: Cell<bool> = const { Cell::new(false) };
}

fn caches_off() -> bool {
    CACHES_OFF.with(Cell::get)
}

/// Turns the behaviour model's caches off (or on) on this thread. A test
/// hook: results and work counts must be the same either way.
#[doc(hidden)]
pub fn set_caches_off_for_tests(off: bool) {
    CACHES_OFF.with(|c| c.set(off));
}

/// How many outcome-pair comparisons (`diff` calls) one routine pair may
/// spend, and one file's check in all. A count, never a time: the same
/// input stops at the same place on every machine. Past either, the pairs
/// not compared are reported as "not proven: work budget" on every
/// statement of the routine (`budget_findings`).
const ROUTINE_DIFFS: u64 = 1 << 21;
const FILE_DIFFS: u64 = 1 << 23;

impl<'x, 'a> Collector<'x, 'a> {
    fn new(
        a: &'x Analyzed<'a>,
        b: &'x Analyzed<'a>,
        env: Env,
        ra: Option<SyntaxNode>,
        rb: Option<SyntaxNode>,
        file_spent: Rc<Cell<u64>>,
        file_work: FileWork,
    ) -> Self {
        let env_budgets = env.work_budgets.unwrap_or((ROUTINE_DIFFS, FILE_DIFFS));
        let (mut sa, mut sb) = (SideIr::new(a, b, ra), SideIr::new(b, a, rb));
        if let Some(done) = &env.identified {
            mark_unidentified(&mut sa.cfg, &mut sb.cfg, done);
        }
        let keys = KeyUniverse::of(&[&sa.cfg, &sb.cfg]);
        let al = Align::new(&keys, &sa, &sb);
        let unit_cap = env
            .work_caps
            .map_or_else(|| unit_work_cap(statements_of(&sa, &sb)), |c| c.0);
        let mut c = Collector {
            sa,
            sb,
            env,
            splits: Vec::new(),
            out: Behaviour::default(),
            seen: HashSet::new(),
            contract_at: HashMap::new(),
            xa: RefCell::new(Vec::new()),
            xb: RefCell::new(Vec::new()),
            al,
            keys,
            incomplete: false,
            prior: Rc::new(HashSet::new()),
            prior_tested: RefCell::new(BTreeMap::new()),
            salt: String::new(),
            cut_a: BTreeSet::new(),
            cut_b: BTreeSet::new(),
            flipped: RefCell::new(HashMap::new()),
            charged: RefCell::new(BTreeSet::new()),
            flip_spent: RefCell::new(BTreeMap::new()),
            replay: Cell::new(None),
            _phase: sqldiff_model::work::UnitPhase::open(),
            spent: Cell::new(0),
            out_of_budget: Cell::new(false),
            cap_hit: RefCell::new(None),
            file_spent,
            budgets: env_budgets,
            unit_start: sqldiff_model::work::now().total(),
            unit_cap,
            file_work,
        };
        c.splits = splits(&c.sa, &c.sb, true);
        c.splits.extend(splits(&c.sb, &c.sa, false));
        c
    }

    /// Whether an earlier pair already has this print (recorded, see
    /// `prior_tested`).
    fn in_prior(&self, fp: &str) -> bool {
        let hit = self.prior.contains(fp);
        self.prior_tested.borrow_mut().insert(fp.to_string(), hit);
        hit
    }

    /// Marks the routine as cut by a work budget, and remembers which one ran
    /// out first, in a fixed order: the unit's metered work, the file's, the
    /// routine's comparisons, the file's comparisons.
    fn note_cap(&self) {
        self.out_of_budget.set(true);
        let mut hit = self.cap_hit.borrow_mut();
        if hit.is_some() {
            return;
        }
        let (total, file_total) = (self.unit_work(), self.file_work_done());
        *hit = Some(if total >= self.unit_cap {
            format!(
                "unit work budget ({total} of {} units of work)",
                self.unit_cap
            )
        } else if file_total >= self.file_cap() {
            format!(
                "file work budget ({file_total} of {} units of work, behaviour analysis only)",
                self.file_cap()
            )
        } else if self.spent.get() >= self.budgets.0 {
            format!(
                "routine comparison budget ({} of {} outcome comparisons)",
                self.spent.get(),
                self.budgets.0
            )
        } else {
            format!(
                "file comparison budget ({} of {} outcome comparisons)",
                self.file_spent.get(),
                self.budgets.1
            )
        });
    }

    /// Metered work spent by this unit so far.
    fn unit_work(&self) -> u64 {
        sqldiff_model::work::now()
            .total()
            .saturating_sub(self.unit_start)
    }

    /// Metered work spent since this engine comparison's analysis began.
    fn file_work_done(&self) -> u64 {
        sqldiff_model::work::now()
            .total()
            .saturating_sub(self.file_work.start)
            .saturating_add(self.file_work.offset)
    }

    fn file_cap(&self) -> u64 {
        match self.env.work_caps {
            Some((_, f)) => f,
            None => self.file_work.cap.max(self.unit_cap),
        }
    }

    /// Whether a work cap (not a comparison budget) is used up. Never while
    /// muted or recomputing a flipped scenario: that work was counted when it
    /// was first done, and a cut there would depend on a cache.
    fn work_cap_used(&self) -> bool {
        // Defensive: no current replay path reaches this (a replay returns
        // from `spend` first, and a cut run does not replay), so the guard
        // is pinned by `cap_tests` and keeps a future path from letting a
        // cache decide a cut.
        if self.replay.get().is_some() || sqldiff_model::work::is_muted() {
            return false;
        }
        self.unit_work() >= self.unit_cap || self.file_work_done() >= self.file_cap()
    }

    /// Spends one `diff` call; false (and the routine marked as cut by its
    /// budget) when the routine's or the file's allowance is used up.
    fn spend(&self) -> bool {
        // A flipped scenario recomputed (a cache that is off, or was cleared)
        // gets exactly the diffs its first computation spent, from a local
        // allowance: the routine's and the file's budget are not touched, so
        // the budget does not depend on whether a cache answered.
        if let Some(left) = self.replay.get() {
            if left == 0 {
                return false;
            }
            self.replay.set(Some(left - 1));
            return true;
        }
        let file = self.file_spent.get();
        let (routine_max, file_max) = self.budgets;
        if self.spent.get() >= routine_max || file >= file_max || self.work_cap_used() {
            self.note_cap();
            return false;
        }
        self.spent.set(self.spent.get() + 1);
        self.file_spent.set(file + 1);
        sqldiff_model::work::spend(sqldiff_model::work::Site::BehaviourDiffs, 1);
        true
    }

    fn fault_scenario(&self, n: NodeId, r: &[ErrorKind], kind: ErrorKind) -> Scenario {
        Scenario {
            faults: if r.contains(&kind) {
                vec![Fault { node: n, kind }]
            } else {
                Vec::new()
            },
            ..self.env.scenario()
        }
    }

    /// The fault-free run of `cfg` under the environment of `sc` (the
    /// scenario without its faults), made once per environment.
    ///
    /// The cache is a speed-up and is not part of the work count: the first
    /// request for an environment is counted (the run is made and its steps
    /// are spent), a later one counts nothing, whether it hits the cache or,
    /// with the caches off, makes the run again (not counted). So the total
    /// is the same with the cache on or off. `charged` is the ledger of
    /// what was counted.
    fn explorer(
        cache: &RefCell<Vec<Rc<Explorer>>>,
        charged: &RefCell<BTreeSet<String>>,
        side: &str,
        cfg: &Cfg,
        other: &Cfg,
        sc: &Scenario,
        keys: &KeyUniverse,
    ) -> Rc<Explorer> {
        let env = Scenario {
            faults: Vec::new(),
            ..sc.clone()
        };
        let first = charged.borrow_mut().insert(format!("{side}|{env:?}"));
        let on = !caches_off();
        let mut c = cache.borrow_mut();
        if on {
            if let Some(x) = c.iter().find(|x| *x.base_scenario() == env) {
                return x.clone();
            }
        }
        // Paths of this side that reach the same state are joined when the
        // other side makes no decision (see `interp::decision_free`).
        let join = crate::ir::interp::decision_free(other);
        let build = || Rc::new(Explorer::in_universe_joined(cfg, &env, keys, join));
        let x = if first {
            build()
        } else {
            sqldiff_model::work::muted(build)
        };
        if on {
            c.push(x.clone());
        }
        x
    }

    fn outcomes(
        &self,
        sc: &Kind,
        scen_a: &Scenario,
        scen_b: &Scenario,
    ) -> (Vec<Outcome>, Vec<Outcome>) {
        let (ca, cb) = (&self.sa.cfg, &self.sb.cfg);
        // Only paths that reach the failing statement belong to a fault
        // scenario.
        let probe = match sc {
            Kind::Fault { key, .. } => Some(KeyProbe::new(key.as_str())),
            _ => None,
        };
        let reached = |o: &Outcome| match &probe {
            Some(p) => o.fault_hit || o.effects.contains_probe(p),
            None => true,
        };
        let mut pa = Self::explorer(&self.xa, &self.charged, "a", ca, cb, scen_a, &self.keys)
            .explore_where(ca, scen_a, &reached);
        let mut pb = Self::explorer(&self.xb, &self.charged, "b", cb, ca, scen_b, &self.keys)
            .explore_where(cb, scen_b, &reached);
        // The replayed and full runs are filtered here; the others were.
        pa.retain(reached);
        pb.retain(reached);
        (pa, pb)
    }

    /// Whether a divergence still shows with a default setting flipped; if
    /// so, the finding does not rest on the assumption behind that default.
    ///
    /// The divergences of the flipped scenario are worked out once for all
    /// findings that flip it (`flipped`): a scenario with many findings used
    /// to explore and compare every outcome pair again for each of them.
    fn persists(&self, sc: &Kind, scen_a: &Scenario, scen_b: &Scenario, d: &Div) -> bool {
        // Cut by a budget: the conservative answer (the finding rests on its
        // assumption), without computing or caching anything.
        if self.out_of_budget.get() && self.replay.get().is_none() {
            return false;
        }
        let key = format!("{sc:?}|{scen_a:?}|{scen_b:?}");
        // The cache of flipped scenarios is a speed-up like the explorers':
        // a scenario's work is counted the first time it is asked for and not
        // again, hit or recomputed (see `explorer`).
        let first = self.charged.borrow_mut().insert(format!("flip|{key}"));
        let known = if caches_off() {
            None
        } else {
            self.flipped.borrow().get(&key).cloned()
        };
        let set = match known {
            Some(set) => set,
            None => {
                let set = if first {
                    let before = self.spent.get();
                    let set = Rc::new(self.divergences_of(sc, scen_a, scen_b));
                    self.flip_spent
                        .borrow_mut()
                        .insert(key.clone(), self.spent.get() - before);
                    set
                } else {
                    // Same pairs in the same order, the same number of diffs,
                    // none of them from the budget or the work count.
                    let allowed = self.flip_spent.borrow().get(&key).copied().unwrap_or(0);
                    self.replay.set(Some(allowed));
                    let set = sqldiff_model::work::muted(|| {
                        Rc::new(self.divergences_of(sc, scen_a, scen_b))
                    });
                    self.replay.set(None);
                    set
                };
                if !caches_off() {
                    let mut c = self.flipped.borrow_mut();
                    if c.len() >= FLIPPED_MAX {
                        c.clear();
                    }
                    c.insert(key, set.clone());
                }
                set
            }
        };
        set.contains(&(d.rule, d.anchor.clone()))
    }

    /// The (rule, anchor) of every divergence between any pair of outcomes
    /// of the two sides under the scenarios.
    fn divergences_of(&self, sc: &Kind, scen_a: &Scenario, scen_b: &Scenario) -> Divergences {
        let (pa, pb) = self.outcomes(sc, scen_a, scen_b);
        let (ps, _) = pairs(&pa, &pb);
        let ra: Vec<Reads> = pa.iter().map(|o| reads(&self.sa, o)).collect();
        let rb: Vec<Reads> = pb.iter().map(|o| reads(&self.sb, o)).collect();
        let mut out = BTreeSet::new();
        for &(i, j) in &ps {
            if !self.spend() {
                break;
            }
            let (a, b) = ((&pa[i], &ra[i]), (&pb[j], &rb[j]));
            for x in diff(
                &self.sa,
                &self.sb,
                &self.splits,
                sc,
                a,
                b,
                &|_, _| false,
                &self.al,
            ) {
                out.insert((x.rule, x.anchor));
            }
        }
        out
    }

    fn run(
        &mut self,
        sc: Kind,
        scen_a: Scenario,
        scen_b: Scenario,
    ) -> (Vec<Outcome>, Vec<Outcome>) {
        // Nothing left to spend: the scenario is not even explored (that is
        // the other cost of a branchy routine), and the routine is reported
        // as not proven.
        if self.spent.get() >= self.budgets.0
            || self.file_spent.get() >= self.budgets.1
            || self.work_cap_used()
        {
            self.note_cap();
            return (Vec::new(), Vec::new());
        }
        let (pa, pb) = self.outcomes(&sc, &scen_a, &scen_b);
        self.cut_a
            .extend(pa.iter().flat_map(|o| o.cut.iter().copied()));
        self.cut_b
            .extend(pb.iter().flat_map(|o| o.cut.iter().copied()));
        let inexact = |o: &Outcome| {
            !o.imprecise.is_empty() || o.durability_unknown.is_some() || !o.untested.is_empty()
        };
        if pa.is_empty() || pb.is_empty() || pa.iter().chain(&pb).any(inexact) {
            self.incomplete = true;
        }
        if pa.is_empty() || pb.is_empty() {
            return (pa, pb);
        }
        let (ps, exact) = pairs(&pa, &pb);
        let combos = ps.len();
        let ra: Vec<Reads> = pa.iter().map(|o| reads(&self.sa, o)).collect();
        let rb: Vec<Reads> = pb.iter().map(|o| reads(&self.sb, o)).collect();
        let mut found: Vec<(Div, &Outcome, &Outcome, usize)> = Vec::new();
        let mut compared = 0;
        for &(i, j) in &ps {
            if !self.spend() {
                break;
            }
            compared += 1;
            let (oa, ob) = (&pa[i], &pb[j]);
            // Each divergence counts once per outcome pair.
            let mut here: Vec<(&str, String)> = Vec::new();
            // Text only for divergences that may still be emitted.
            let need = |rule: &str, anchor: &str| {
                !found
                    .iter()
                    .any(|x| x.0.rule == rule && x.0.anchor == anchor)
                    && !self.seen.contains(&(rule.to_string(), anchor.to_string()))
            };
            let (a, b) = ((oa, &ra[i]), (ob, &rb[j]));
            let ds = diff(&self.sa, &self.sb, &self.splits, &sc, a, b, &need, &self.al);
            for d in ds {
                if here.contains(&(d.rule, d.anchor.clone())) {
                    continue;
                }
                here.push((d.rule, d.anchor.clone()));
                if let Some(x) = found
                    .iter_mut()
                    .find(|x| x.0.rule == d.rule && x.0.anchor == d.anchor)
                {
                    x.3 += 1;
                } else {
                    found.push((d, oa, ob, 1));
                }
            }
        }
        for (d, oa, ob, hits) in found {
            self.emit(
                &sc,
                d,
                oa,
                ob,
                // Pairs left out may disagree: all agreeing is not known.
                compared == combos && (exact || hits == combos),
                Some((&scen_a, &scen_b)),
            );
        }
        (pa, pb)
    }

    fn emit(
        &mut self,
        sc: &Kind,
        d: Div,
        oa: &Outcome,
        ob: &Outcome,
        all_agree: bool,
        scens: Option<(&Scenario, &Scenario)>,
    ) {
        let key = (d.rule.to_string(), d.anchor.clone());
        if self.seen.contains(&key) {
            return;
        }
        let imprecise = !oa.imprecise.is_empty() || !ob.imprecise.is_empty();
        let durability_unknown =
            d.durability && (oa.durability_unknown.is_some() || ob.durability_unknown.is_some());
        // A caller-transaction divergence is what flipping the default
        // assumption ASM-NO-CALLER-TXN would change: under the assumption
        // both sides agree, so it is reported but does not make the pair
        // different.
        // Milestone M5b: a divergence that rests on a profile row nobody has
        // tested on a real system is capped at unknown.
        let untested: Vec<String> = oa.untested.union(&ob.untested).cloned().collect();
        let verdict = if imprecise || durability_unknown || !untested.is_empty() {
            Verdict::Unknown
        } else if matches!(sc, Kind::CallerTxn) {
            Verdict::EqualUnderAssumptions
        } else if all_agree {
            Verdict::Different
        } else {
            Verdict::Unknown
        };
        // The error contract and the caller's continuation are properties of
        // the routine as a whole: one finding each, the most certain one, so
        // an imprecise path cannot hide a proven difference.
        let replace = if matches!(d.rule, "error.contract" | "txn.caller-continuation") {
            match self.contract_at.get(d.rule) {
                Some(&i) if rank(verdict) <= rank(self.out.findings[i].verdict) => return,
                at => at.copied(),
            }
        } else {
            None
        };
        self.seen.insert(key);
        let mut ids: Vec<String> = d.assumptions.clone();
        for x in oa.assumptions.iter().chain(&ob.assumptions) {
            if !ids.contains(x) {
                ids.push(x.clone());
            }
        }
        if matches!(sc, Kind::CallerTxn) {
            ids.insert(0, "ASM-NO-CALLER-TXN".to_string());
            ids.dedup();
        } else if let Some((scen_a, scen_b)) = scens {
            for id in RERUN_ASSUMPTIONS {
                if !ids.iter().any(|x| x == id) {
                    continue;
                }
                let (mut fa, mut fb) = (scen_a.clone(), scen_b.clone());
                for s in [&mut fa, &mut fb] {
                    if id == "ASM-NO-CALLER-TXN" {
                        s.caller_txn = true;
                    } else {
                        s.xact_abort = true;
                    }
                }
                if self.persists(sc, &fa, &fb, &d) {
                    ids.retain(|x| x != id);
                }
            }
        }
        let mut text = format!(
            "{}, {}.",
            scenario_intro(&self.sa, &self.sb, sc, oa),
            d.text
        );
        let mut caveats: Vec<String> = Vec::new();
        caveats.extend(oa.imprecise().chain(ob.imprecise()).map(str::to_string));
        if durability_unknown {
            caveats.extend(
                oa.durability_unknown
                    .iter()
                    .chain(&ob.durability_unknown)
                    .cloned(),
            );
        }
        if !all_agree {
            caveats.push("the outcome depends on which branch runs".into());
        }
        for u in &untested {
            caveats.push(format!(
                "it relies on {u}, which is not tested on a real system (docs/vendor-facts-verification.md)"
            ));
        }
        caveats.dedup();
        if !caveats.is_empty() {
            text.push_str(&format!(" Not proven: {}.", caveats.join("; ")));
        }
        if matches!(sc, Kind::CallerTxn) {
            text.push_str(
                " Without a caller transaction (the default assumption) both sides agree.",
            );
        }
        for id in &ids {
            if !self.out.assumptions.iter().any(|y| &y.id == id) {
                self.out.assumptions.push(assumption(id));
            }
        }
        let anchor_b = d.anchor_b.clone().unwrap_or_else(|| d.anchor.clone());
        let node_a = |c: &Self| match sc {
            Kind::Fault { key, na, .. } if *key == d.anchor => c.sa.syntax(*na).cloned(),
            _ => c.sa.locate(&d.anchor).and_then(|n| c.sa.syntax(n).cloned()),
        };
        let node_b = |c: &Self| match sc {
            Kind::Fault { key, nb, .. } if *key == anchor_b => c.sb.syntax(*nb).cloned(),
            _ => c.sb.locate(&anchor_b).and_then(|n| c.sb.syntax(n).cloned()),
        };
        let (la, lb) = (node_a(self), node_b(self));
        let anchor_text = |n: &Option<SyntaxNode>| {
            n.as_ref()
                .map(|n| ast::first_words(n, 3).join(" "))
                .unwrap_or_default()
        };
        // Anchored at the first words of the statement on each side, so
        // reformatting keeps the fingerprint; the effect key is added only to
        // split two findings that would otherwise collide.
        let print = |ka: &str, kb: &str| {
            fingerprint(&FingerprintInput {
                object: &self.sa.a.object,
                class: d.class,
                rule: d.rule,
                anchor_a: &format!("{ka}{}", anchor_text(&la)),
                anchor_b: &format!("{kb}{}", anchor_text(&lb)),
            })
        };
        let mut fp = print("", "");
        if self
            .out
            .findings
            .iter()
            .enumerate()
            .any(|(i, f)| f.fingerprint == fp && Some(i) != replace)
            || self.in_prior(&fp)
        {
            fp = print(
                &format!("{}{}|", self.salt, d.anchor),
                &format!("{}{anchor_b}|", self.salt),
            );
        }
        let w = (!self.env.skip_witnesses).then(|| Witness {
            fingerprint: fp.clone(),
            rule: d.rule.to_string(),
            scenario: scenario_info(&self.sa, &self.sb, sc, &self.env, oa),
            a: side_outcome(&self.sa, oa),
            b: side_outcome(&self.sb, ob),
        });
        let f = Finding {
            fingerprint: fp,
            rule: d.rule.to_string(),
            class: d.class,
            severity: d.severity,
            verdict,
            witness: text,
            a: la.as_ref().map(|n| self.sa.a.node_location(n)),
            b: lb.as_ref().map(|n| self.sb.a.node_location(n)),
            assumptions: ids,
        };
        match replace {
            Some(i) => {
                let old = std::mem::replace(&mut self.out.findings[i], f);
                if let Some(w) = w {
                    match self
                        .out
                        .witnesses
                        .iter_mut()
                        .find(|x| x.fingerprint == old.fingerprint)
                    {
                        Some(x) => *x = w,
                        None => self.out.witnesses.push(w),
                    }
                }
            }
            None => {
                if matches!(d.rule, "error.contract" | "txn.caller-continuation") {
                    self.contract_at.insert(d.rule, self.out.findings.len());
                }
                if let Some(w) = w {
                    self.out.witnesses.push(w);
                }
                self.out.findings.push(f);
            }
        }
    }

    /// One unknown finding (`equiv.budget`) on every statement a budget kept
    /// the exploration from running: each statement a cut path could still
    /// have run, in any scenario. Without it a statement past the budget is
    /// absent from every outcome, nothing is compared there, and its unit
    /// reads as "under assumptions".
    fn budget_findings(&mut self) {
        for on_a in [true, false] {
            let (side, cuts) = if on_a {
                (&self.sa, &self.cut_a)
            } else {
                (&self.sb, &self.cut_b)
            };
            if cuts.is_empty() {
                continue;
            }
            let from: Vec<NodeId> = cuts.iter().copied().collect();
            let mut nodes: Vec<&SyntaxNode> = side
                .cfg
                .reachable_from(&from)
                .into_iter()
                .filter(|&i| side.cfg.node(i).stmt().is_some())
                .filter_map(|i| side.cfg.node(i).syntax.as_ref())
                .collect();
            nodes.sort_by_key(|n| n.text_range().start());
            nodes.dedup_by_key(|n| n.text_range());
            let witness = match self.cap_hit.borrow().as_deref() {
                Some(cap) => format!(
                    "Not proven: the behaviour model reached its {cap} before it compared this statement in at least one scenario, so what happens when it fails and how its transaction goes are not compared."
                ),
                None => "Not proven: the behaviour model stopped at its step or path budget before it ran or compared this statement in at least one scenario, so what happens when it fails and how its transaction goes are not compared.".to_string(),
            };
            let mut seen: HashMap<String, usize> = HashMap::new();
            for n in nodes {
                let words = ast::first_words(n, 3).join(" ");
                let nth = seen.entry(words.clone()).or_insert(0);
                *nth += 1;
                let loc = side.a.node_location(n);
                let fp = fingerprint(&FingerprintInput {
                    object: &side.a.object,
                    class: ChangeClass::Transactional,
                    rule: "equiv.budget",
                    anchor_a: &format!("{}{words}#{nth}", self.salt),
                    anchor_b: "",
                });
                self.out.findings.push(Finding {
                    fingerprint: fp,
                    rule: "equiv.budget".to_string(),
                    class: ChangeClass::Transactional,
                    severity: Severity::Medium,
                    verdict: Verdict::Unknown,
                    witness: witness.clone(),
                    a: on_a.then(|| loc.clone()),
                    b: (!on_a).then_some(loc),
                    assumptions: Vec::new(),
                });
            }
        }
    }

    /// Faults at every write both sides perform, in document order of side A.
    fn faults(&mut self) {
        let keys: Vec<String> = self
            .sa
            .cfg
            .effect_nodes()
            .into_iter()
            .map(|(_, e)| e.key.clone())
            .filter(|k| self.sb.effect(k).is_some())
            .collect();
        for key in keys {
            let na = self.sa.effect(&key).expect("effect node").0;
            let nb = self.sb.effect(&key).expect("effect node").0;
            let (ra, rb) = (self.sa.raises(na), self.sb.raises(nb));
            for kind in ErrorKind::ALL {
                if !ra.contains(&kind) && !rb.contains(&kind) {
                    continue;
                }
                let (fa, fb) = (
                    self.fault_scenario(na, &ra, kind),
                    self.fault_scenario(nb, &rb, kind),
                );
                self.run(
                    Kind::Fault {
                        key: key.clone(),
                        kind,
                        na,
                        nb,
                    },
                    fa,
                    fb,
                );
            }
        }
    }

    /// One finding per statement split across sides: the separate
    /// statements read the source at different times.
    fn split_findings(&mut self) {
        for s in self.splits.clone() {
            let (m, p) = if s.merge_on_a {
                (&self.sa, &self.sb)
            } else {
                (&self.sb, &self.sa)
            };
            let Some((_, me)) = m.effect(&s.merge_key) else {
                continue;
            };
            let target = me.target.clone();
            let text = format!(
                "{} changes {} in one statement ({}), which reads its source once; {} splits it into {} and {}, each reading the source at its own time, so a change another session commits between them is seen by one and not the other",
                m.name(),
                target.to_lowercase(),
                m.phrase_at(&s.merge_key),
                p.name(),
                p.phrase_at(&s.part_key),
                p.phrase_at(&s.merge_key),
            );
            let mut d = Div::new(
                "concurrency.split-statement",
                ChangeClass::Concurrency,
                Severity::High,
                &s.merge_key,
                text,
            );
            if s.merge_on_a {
                d.anchor_b = Some(s.part_key.clone());
            } else {
                d.anchor = s.part_key.clone();
                d.anchor_b = Some(s.merge_key.clone());
            }
            d.assumptions = vec!["ASM-CONCURRENT-WRITERS".into(), "ASM-READ-COMMITTED".into()];
            let o = Outcome::empty();
            self.emit(&Kind::Baseline, d, &o, &o, true, None);
        }
    }
}

impl SideIr<'_, '_> {
    fn raises(&self, n: NodeId) -> Vec<ErrorKind> {
        self.cfg
            .node(n)
            .stmt()
            .map(|s| s.raises.clone())
            .unwrap_or_default()
    }

    fn line_no(&self, n: NodeId) -> Option<usize> {
        *self.line_nos[n].get_or_init(|| self.syntax(n).map(|s| self.a.line_of(s)))
    }

    fn stmt_phrase(&self, n: NodeId) -> String {
        match self.syntax(n) {
            Some(s) => format!(
                "the statement `{}` on line {}",
                self.a.snippet(s, 40),
                self.a.line_of(s)
            ),
            None => "the statement".to_string(),
        }
    }

    /// Statement nodes in document order.
    fn stmt_nodes(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = (0..self.cfg.nodes.len())
            .filter(|i| self.cfg.node(*i).stmt().is_some() && self.cfg.node(*i).syntax.is_some())
            .collect();
        v.sort_by_key(|i| self.syntax(*i).map(|s| s.text_range().start()));
        v
    }
}

/// MERGE statements on side `m` whose branch the other side `p` runs as a
/// separate statement on the same table.
fn splits(m: &SideIr, p: &SideIr, merge_on_a: bool) -> Vec<Split> {
    let mut out = Vec::new();
    for (n, e) in m.cfg.effect_nodes() {
        if e.kind != StmtKind::Merge || p.effect(&e.key).is_none() {
            continue;
        }
        let Some(syn) = m.syntax(n) else { continue };
        let words: Vec<String> = ast::sig_tokens(syn)
            .map(|t| t.text().to_ascii_uppercase())
            .collect();
        for (branch, word) in [
            (StmtKind::Delete, "DELETE"),
            (StmtKind::Update, "UPDATE"),
            (StmtKind::Insert, "INSERT"),
        ] {
            let has_branch = words.windows(2).any(|w| w[0] == "THEN" && w[1] == word);
            if !has_branch {
                continue;
            }
            for (_, pe) in p.cfg.effect_nodes() {
                if pe.kind == branch && pe.target == e.target && m.effect(&pe.key).is_none() {
                    out.push(Split {
                        merge_on_a,
                        merge_key: e.key.clone(),
                        part_key: pe.key.clone(),
                    });
                }
            }
        }
    }
    out
}

/// Behavioural findings from running both sides' IR under the same scenarios.
pub fn compare(a: &Analyzed, b: &Analyzed) -> (Vec<Finding>, Vec<Assumption>) {
    let r = analyze(a, b, &Env::default());
    (r.findings, r.assumptions)
}

/// The routines of an artifact the behaviour model analyses: the outermost
/// `ROUTINE` nodes in document order, without PostgreSQL `DO` blocks when the
/// artifact also creates a named routine (a converted script's setup batch,
/// as `equiv::lower` reads it).
fn routines_of(a: &Analyzed) -> Vec<SyntaxNode> {
    let mut top: Vec<SyntaxNode> = Vec::new();
    for n in a.root.descendants() {
        if n.kind() == sqldiff_syntax::SyntaxKind::ROUTINE
            && !top
                .last()
                .is_some_and(|t| t.text_range().contains_range(n.text_range()))
        {
            top.push(n);
        }
    }
    let named: Vec<SyntaxNode> = top
        .iter()
        .filter(|r| !(a.dialect() == Dialect::Postgres && crate::equiv::lower::is_do(r)))
        .cloned()
        .collect();
    if named.is_empty() {
        top
    } else {
        named
    }
}

/// How the routines of two artifacts meet. One routine (or none) on each
/// side meet as they are: the behaviour model has always compared them
/// whatever their names. With more, they meet in order when both sides have
/// the same number and no pair is known to differ in kind or name, and
/// otherwise by kind and name (`equiv::script::routine_key`), as the
/// statement-level comparison meets them. A routine without a partner is
/// listed on its own side.
struct Meeting {
    pairs: Vec<(SyntaxNode, SyntaxNode)>,
    only_a: Vec<SyntaxNode>,
    only_b: Vec<SyntaxNode>,
}

fn meet(a: &Analyzed, b: &Analyzed) -> Option<Meeting> {
    let (ra, rb) = (routines_of(a), routines_of(b));
    // At most one routine each, or a side with none (a script of batches
    // against `DO` blocks): analysed whole, as before.
    if (ra.len() <= 1 && rb.len() <= 1) || ra.is_empty() || rb.is_empty() {
        return None;
    }
    let key = |r: &SyntaxNode, an: &Analyzed| crate::equiv::script::routine_key(r, an.dialect());
    let mut m = Meeting {
        pairs: Vec::new(),
        only_a: Vec::new(),
        only_b: Vec::new(),
    };
    let ka: Vec<Option<_>> = ra.iter().map(|r| key(r, a)).collect();
    let kb: Vec<Option<_>> = rb.iter().map(|r| key(r, b)).collect();
    // In order, when no pair is known to name different objects.
    if ra.len() == rb.len()
        && ka
            .iter()
            .zip(&kb)
            .all(|(x, y)| !matches!((x, y), (Some(x), Some(y)) if x != y))
    {
        m.pairs = ra.iter().cloned().zip(rb.iter().cloned()).collect();
        return Some(m);
    }
    // By kind and name; a name that occurs a different number of times on
    // the two sides (overloads) meets nothing.
    let count = |ks: &[Option<_>], k: &_| ks.iter().filter(|x| x.as_ref() == Some(k)).count();
    let mut used = vec![false; rb.len()];
    for (x, kx) in ra.iter().zip(&ka) {
        let partner = kx.as_ref().and_then(|k| {
            if count(&ka, k) != 1 || count(&kb, k) != 1 {
                return None;
            }
            kb.iter().position(|y| y.as_ref() == Some(k))
        });
        match partner {
            Some(j) => {
                used[j] = true;
                m.pairs.push((x.clone(), rb[j].clone()));
            }
            None => m.only_a.push(x.clone()),
        }
    }
    for (j, y) in rb.iter().enumerate() {
        if !used[j] {
            m.only_b.push(y.clone());
        }
    }
    Some(m)
}

/// [`compare`] under an environment, with structured witnesses.
///
/// A file that creates several routines is analysed routine by routine
/// (`meet`): each pair gets its own control-flow graph, scenarios and
/// budgets. A routine that meets none is not analysed, and a transaction
/// statement inside it stays unproven.
pub fn analyze(a: &Analyzed, b: &Analyzed, env: &Env) -> Behaviour {
    analyze_runs(a, b, env, None)
}

/// What the file's shared budgets looked like around one routine pair of an
/// analysis, and what the pair spent: the facts a stored pair result needs to
/// be replayed (design `w4-4-routine-units.md`, section G).
#[derive(Debug, Clone)]
pub struct PairRun {
    /// `diff` calls the file had spent when the pair began, and when it ended.
    pub file_spent_before: u64,
    pub file_spent_after: u64,
    /// Metered work since the analysis began, when the pair began and ended.
    pub work_before: u64,
    pub work_after: u64,
    /// `diff` calls the pair itself spent, and whether a budget cut it.
    pub spent: u64,
    pub cut: bool,
    /// Fingerprints of the findings of the pairs before it (the collision
    /// salt reads them).
    pub prior: Vec<String>,
    pub behaviour: Behaviour,
}

/// [`analyze`], also listing each routine pair's run in pair order (nothing
/// for a file analysed as one unit).
pub fn analyze_runs(
    a: &Analyzed,
    b: &Analyzed,
    env: &Env,
    mut runs: Option<&mut Vec<PairRun>>,
) -> Behaviour {
    // The work caps read the meter, which counts nothing outside a scope: an
    // entry that opened none would run uncapped.
    debug_assert!(
        sqldiff_model::work::in_scope(),
        "behaviour::analyze needs an open work scope"
    );
    // The `diff` calls all routine pairs of this file share.
    let file_spent = Rc::new(Cell::new(0));
    let start = sqldiff_model::work::now().total();
    let Some(m) = meet(a, b) else {
        // One unit: its own cap is the file's floor (taken in `file_cap`).
        let file_work = FileWork {
            start,
            cap: file_work_cap(1, 0),
            offset: 0,
        };
        // At most one routine on each side: `routines_of` picks it, so a
        // setup `DO` block beside it is not mistaken for it.
        let (ra, rb) = (routines_of(a), routines_of(b));
        let (mut out, _) = analyze_pair(
            a,
            b,
            env,
            ra.first().cloned(),
            rb.first().cloned(),
            "",
            &Rc::new(HashSet::new()),
            &file_spent,
            file_work,
        );
        // Transaction statements are compared only inside a routine both
        // sides hold exactly once.
        let span = |x: &SyntaxNode| {
            let r = x.text_range();
            usize::from(r.start())..usize::from(r.end())
        };
        let single = |an: &Analyzed| {
            let mut rs = an
                .root
                .descendants()
                .filter(|n| n.kind() == sqldiff_syntax::SyntaxKind::ROUTINE);
            match (rs.next(), rs.next()) {
                (Some(r), None) => Some(span(&r)),
                _ => None,
            }
        };
        if let (true, Some(x), Some(y)) = (out.txn_compared, single(a), single(b)) {
            out.txn_compared_a.push(x);
            out.txn_compared_b.push(y);
        }
        return out;
    };
    // The file cap is a function of the file: its units and the largest unit
    // cap, known before the first unit runs.
    let unit_caps: Vec<u64> = if env.work_caps.is_some() {
        Vec::new()
    } else {
        m.pairs
            .iter()
            .map(|(x, y)| unit_work_cap(pair_statements(a, b, Some(x.clone()), Some(y.clone()))))
            .collect()
    };
    let largest = unit_caps.iter().copied().max().unwrap_or(0);
    let file_work = FileWork {
        start,
        cap: file_work_cap(m.pairs.len().max(1) as u64, largest),
        offset: 0,
    };
    let mut out = Behaviour::default();
    let mut prior: Rc<HashSet<String>> = Rc::new(HashSet::new());
    let mut all_compared = m.only_a.is_empty() && m.only_b.is_empty();
    // Stored pair results are consulted only when a cache is attached; the
    // caps they are judged against are the current file's (never stored).
    let caps = env.pair_cache.as_ref().map(|_| PairCaps {
        unit_caps: unit_caps.clone(),
        file_cap: file_work.cap,
    });
    for (i, (x, y)) in m.pairs.iter().enumerate() {
        let name = sqldiff_syntax::ast::routine_name(x).unwrap_or_default();
        let entry_spent = file_spent.get();
        let entry_work = sqldiff_model::work::now().total().saturating_sub(start);
        let before = runs.is_some().then(|| {
            // Sorted at once: the order of the set is never read.
            #[allow(clippy::disallowed_methods)]
            let mut v: Vec<String> = prior.iter().cloned().collect();
            v.sort();
            (entry_spent, entry_work, v)
        });
        let fresh = || {
            analyze_pair(
                a,
                b,
                env,
                Some(x.clone()),
                Some(y.clone()),
                &format!("{name}|"),
                &prior,
                &file_spent,
                file_work,
            )
        };
        let (r, stats) = match (&env.pair_cache, &caps) {
            (Some(cache), Some(caps)) => {
                let key = pair_key(a, b, x, y, env, cache.0.build());
                let bases = PairBases::of(a, b, x, y);
                let taken = cache
                    .0
                    .get(&key)
                    .and_then(|bytes| serde_json::from_slice::<StoredPair>(&bytes).ok())
                    .filter(|sp| sp.reusable(entry_spent, entry_work, &prior, caps, i, env));
                match taken {
                    Some(sp) if bases.shifted(&sp.behaviour, true).is_none() => {
                        // Offsets that do not move back: built, not trusted.
                        let (r, stats) = fresh();
                        cache.0.note(PairEvent::Built, &key);
                        (r, stats)
                    }
                    Some(sp) if !cache.0.recheck(&key) => {
                        // Advance the file's shared state exactly as the
                        // pair would have.
                        file_spent.set(entry_spent + sp.spent);
                        sqldiff_model::work::spend(
                            sqldiff_model::work::Site::BehaviourDiffs,
                            sp.work,
                        );
                        cache.0.note(PairEvent::Reused, &key);
                        (
                            bases.shifted(&sp.behaviour, true).unwrap_or_default(),
                            PairStats {
                                spent: sp.spent,
                                cut: false,
                                tested: sp.tested,
                            },
                        )
                    }
                    Some(sp) => {
                        // Sampled: build it again; the fresh result wins.
                        let (r, stats) = fresh();
                        let rebuilt = StoredPair::of(
                            &r,
                            &stats,
                            entry_work,
                            start,
                            caps.unit_cap(env, i),
                            &bases,
                        );
                        let same = rebuilt.as_ref().is_some_and(|rb| rb.same_as(&sp));
                        cache.0.note(
                            if same {
                                PairEvent::Verified
                            } else {
                                PairEvent::Mismatch
                            },
                            &key,
                        );
                        if let (false, false, Some(rb)) = (same, stats.cut, &rebuilt) {
                            if let Ok(bytes) = serde_json::to_vec(rb) {
                                cache.0.put(&key, &bytes);
                            }
                        }
                        (r, stats)
                    }
                    None => {
                        let (r, stats) = fresh();
                        cache.0.note(PairEvent::Built, &key);
                        // A pair cut by a budget depends on where the file
                        // stood; only a complete one is kept.
                        if !stats.cut {
                            let sp = StoredPair::of(
                                &r,
                                &stats,
                                entry_work,
                                start,
                                caps.unit_cap(env, i),
                                &bases,
                            );
                            if let Some(Ok(bytes)) = sp.map(|sp| serde_json::to_vec(&sp)) {
                                cache.0.put(&key, &bytes);
                            }
                        }
                        (r, stats)
                    }
                }
            }
            _ => fresh(),
        };
        if let (Some(runs), Some((spent_before, work_before, prior_before))) =
            (runs.as_deref_mut(), before)
        {
            runs.push(PairRun {
                file_spent_before: spent_before,
                file_spent_after: file_spent.get(),
                work_before,
                work_after: sqldiff_model::work::now().total().saturating_sub(start),
                spent: stats.spent,
                cut: stats.cut,
                prior: prior_before,
                behaviour: r.clone(),
            });
        }
        // The pair's collector is gone, so `prior` is unique here and
        // `make_mut` does not copy it.
        Rc::make_mut(&mut prior).extend(r.findings.iter().map(|f| f.fingerprint.clone()));
        all_compared &= r.txn_compared;
        if r.txn_compared {
            let span = |n: &SyntaxNode| {
                let t = n.text_range();
                usize::from(t.start())..usize::from(t.end())
            };
            out.txn_compared_a.push(span(x));
            out.txn_compared_b.push(span(y));
        }
        out.findings.extend(r.findings);
        out.witnesses.extend(r.witnesses);
        for x in r.assumptions {
            if !out.assumptions.iter().any(|y| y.id == x.id) {
                out.assumptions.push(x);
            }
        }
    }
    // A routine that meets none is not analysed (as before): the
    // statement-level comparison pairs scripts, triggers and views by
    // other rules than this one, and says what it could not compare.
    all_compared &= m.only_a.is_empty() && m.only_b.is_empty();
    out.txn_compared = all_compared;
    out
}

/// The caps a stored pair is judged against, computed for the file being
/// checked (never read from an entry).
struct PairCaps {
    /// Each pair's unit work cap (empty when `Env::work_caps` fixes them).
    unit_caps: Vec<u64>,
    /// The file cap before a unit's own cap is taken into account.
    file_cap: u64,
}

impl PairCaps {
    fn unit_cap(&self, env: &Env, i: usize) -> u64 {
        env.work_caps.map_or_else(|| self.unit_caps[i], |c| c.0)
    }

    /// `Collector::file_cap` for pair `i`.
    fn file_cap(&self, env: &Env, i: usize) -> u64 {
        match env.work_caps {
            Some((_, f)) => f,
            None => self.file_cap.max(self.unit_caps[i]),
        }
    }
}

/// A routine pair's result as the cache keeps it: the behaviour, what the
/// pair spent of the file's shared budgets, and everything its result read
/// of the file's state. Only a pair no budget cut is kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPair {
    behaviour: Behaviour,
    /// `diff` calls the pair spent, and the work it metered.
    spent: u64,
    work: u64,
    /// The unit work cap it ran under.
    unit_cap: u64,
    /// The unsalted prints it tested against the earlier pairs' findings,
    /// with the answer then.
    tested: Vec<(String, bool)>,
}

impl StoredPair {
    /// The pair's result as it is stored: byte offsets relative to the
    /// start of each routine (`bases`). `None` when a position lies outside
    /// the routine it belongs to or in another file: such a result is not
    /// kept, since it would not move with the routine.
    fn of(
        r: &Behaviour,
        stats: &PairStats,
        entry_work: u64,
        start: u64,
        unit_cap: u64,
        bases: &PairBases,
    ) -> Option<StoredPair> {
        Some(StoredPair {
            behaviour: bases.shifted(r, false)?,
            spent: stats.spent,
            work: sqldiff_model::work::now()
                .total()
                .saturating_sub(start)
                .saturating_sub(entry_work),
            unit_cap,
            tested: stats.tested.clone(),
        })
    }

    fn same_as(&self, other: &StoredPair) -> bool {
        match (serde_json::to_vec(self), serde_json::to_vec(other)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
    }

    /// Whether the stored result is what the pair would return now, at this
    /// place in the file. The strict inequalities leave no room for a cut
    /// the stored run did not see (`spend` and `outcomes` both test `>=`).
    fn reusable(
        &self,
        entry_spent: u64,
        entry_work: u64,
        prior: &HashSet<String>,
        caps: &PairCaps,
        i: usize,
        env: &Env,
    ) -> bool {
        let file_diffs = env.work_budgets.unwrap_or((ROUTINE_DIFFS, FILE_DIFFS)).1;
        self.unit_cap == caps.unit_cap(env, i)
            && entry_spent.saturating_add(self.spent) < file_diffs
            && entry_work.saturating_add(self.work) < caps.file_cap(env, i)
            && self
                .tested
                .iter()
                .all(|(fp, was)| prior.contains(fp) == *was)
    }
}

/// Where the two routines of a pair stand in their files: the byte range of
/// each (side A, side B) and the path of its file.
struct PairBases {
    a: (std::ops::Range<usize>, String),
    b: (std::ops::Range<usize>, String),
}

impl PairBases {
    fn of(a: &Analyzed, b: &Analyzed, x: &SyntaxNode, y: &SyntaxNode) -> PairBases {
        let (la, lb) = (a.node_location(x), b.node_location(y));
        PairBases {
            a: (la.byte_start..la.byte_end, la.path),
            b: (lb.byte_start..lb.byte_end, lb.path),
        }
    }

    /// `r` with every byte offset moved from the file's to the routine's
    /// frame (`to_file` false) or back (`to_file` true). Offsets are in
    /// finding locations and in the compared ranges; line and column
    /// positions are in the key and need no move. `None` when an offset is
    /// not inside its routine, or a location names another file.
    fn shifted(&self, r: &Behaviour, to_file: bool) -> Option<Behaviour> {
        let mut r = r.clone();
        let side = |s: crate::model::Side| {
            if s == crate::model::Side::A {
                &self.a
            } else {
                &self.b
            }
        };
        let mv = |range: &std::ops::Range<usize>, base: &std::ops::Range<usize>| {
            if to_file {
                let (s, e) = (
                    base.start.checked_add(range.start)?,
                    base.start.checked_add(range.end)?,
                );
                (range.start <= range.end).then_some(s..e)
            } else {
                (range.start >= base.start && range.end >= range.start && range.end <= base.end)
                    .then(|| range.start - base.start..range.end - base.start)
            }
        };
        for f in &mut r.findings {
            for loc in [&mut f.a, &mut f.b].into_iter().flatten() {
                let (base, path) = side(loc.side);
                if !to_file && loc.path != *path {
                    return None;
                }
                let m = mv(&(loc.byte_start..loc.byte_end), base)?;
                (loc.byte_start, loc.byte_end) = (m.start, m.end);
            }
        }
        for (ranges, (base, _)) in [
            (&mut r.txn_compared_a, &self.a),
            (&mut r.txn_compared_b, &self.b),
        ] {
            for range in ranges.iter_mut() {
                *range = mv(range, base)?;
            }
        }
        Some(r)
    }
}

/// The key of a routine pair's result: everything the pair reads of the
/// files (design `w4-4-routine-units.md`, section H and the review P1). The
/// line and column where each routine starts and the length of its text are
/// in the key; its byte offset is not: the stored result holds byte offsets
/// relative to the routine and a reuse adds the routine's offset
/// ([`PairBases`]), so a routine moves in its file without losing its result.
fn pair_key(
    a: &Analyzed,
    b: &Analyzed,
    ra: &SyntaxNode,
    rb: &SyntaxNode,
    env: &Env,
    build: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut field = |tag: &str, bytes: &[u8]| {
        h.update((tag.len() as u64).to_le_bytes());
        h.update(tag.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field("format", b"pair-1");
    field("build", build.as_bytes());
    for (tag, an, r, other) in [("a", a, ra, b), ("b", b, rb, a)] {
        field(
            &format!("{tag}.dialect"),
            format!("{:?}", an.dialect()).as_bytes(),
        );
        field(&format!("{tag}.version"), an.artifact.version.as_bytes());
        field(&format!("{tag}.path"), an.artifact.path.as_bytes());
        field(&format!("{tag}.object"), an.object.as_bytes());
        field(&format!("{tag}.side"), format!("{:?}", an.side).as_bytes());
        field(
            &format!("{tag}.excerpt"),
            an.excerpt_lines.to_string().as_bytes(),
        );
        // The tree that is lowered: kind and text of every token.
        let mut tree = Sha256::new();
        for t in r.descendants_with_tokens().filter_map(|e| e.into_token()) {
            tree.update(format!("{:?}", t.kind()).as_bytes());
            tree.update([0]);
            tree.update(t.text().as_bytes());
            tree.update([1]);
        }
        field(&format!("{tag}.tree"), &tree.finalize());
        let loc = an.node_location(r);
        field(
            &format!("{tag}.position"),
            format!(
                "{}:{} len {}",
                loc.start_line,
                loc.start_col,
                loc.byte_end - loc.byte_start
            )
            .as_bytes(),
        );
        let mut temps: Vec<String> = other_temps(an, other).into_iter().collect();
        temps.sort();
        field(
            &format!("{tag}.other_temps"),
            temps.join("\u{1}").as_bytes(),
        );
    }
    // `Env` field by field, so a new field cannot be left out.
    let Env {
        caller_txn,
        xact_abort,
        commit_level,
        skip_witnesses,
        work_budgets,
        work_caps,
        identified,
        pair_cache: _,
    } = env;
    field(
        "env",
        format!("{caller_txn} {xact_abort} {commit_level:?} {skip_witnesses} {work_budgets:?} {work_caps:?}")
            .as_bytes(),
    );
    // The identified condition pairs of this routine pair only, by offset
    // from the start of each routine: the pair reads them as positions in
    // its own routines, so a routine that moves in its file keeps its key.
    let (sa, sb) = (
        usize::from(ra.text_range().start()),
        usize::from(rb.text_range().start()),
    );
    let inside = |r: &SyntaxNode, at: usize| {
        let t = r.text_range();
        usize::from(t.start()) <= at && at < usize::from(t.end())
    };
    let subset: Option<Vec<(usize, usize)>> = identified.as_ref().map(|set| {
        set.iter()
            .copied()
            .filter(|&(x, y)| inside(ra, x) && inside(rb, y))
            .map(|(x, y)| (x - sa, y - sb))
            .collect()
    });
    field("identified", format!("{subset:?}").as_bytes());
    // The salt of the pair's collision fingerprints.
    field(
        "name",
        sqldiff_syntax::ast::routine_name(ra)
            .unwrap_or_default()
            .as_bytes(),
    );
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The analysis of one pair of routines (or, with `None`, of the statements
/// outside any routine). `prior` is shared, not copied: the collector only
/// reads it, and the caller extends it after the pair.
#[allow(clippy::too_many_arguments)]
fn analyze_pair(
    a: &Analyzed,
    b: &Analyzed,
    env: &Env,
    ra: Option<SyntaxNode>,
    rb: Option<SyntaxNode>,
    salt: &str,
    prior: &Rc<HashSet<String>>,
    file_spent: &Rc<Cell<u64>>,
    file_work: FileWork,
) -> (Behaviour, PairStats) {
    let mut c = Collector::new(a, b, env.clone(), ra, rb, file_spent.clone(), file_work);
    c.salt = salt.to_string();
    c.prior = Rc::clone(prior);
    c.faults();
    c.split_findings();
    c.run(Kind::Baseline, env.scenario(), env.scenario());
    if !env.caller_txn {
        let caller = Scenario {
            caller_txn: true,
            ..env.scenario()
        };
        c.run(Kind::CallerTxn, caller.clone(), caller);
    }
    if c.out_of_budget.get() {
        // Some pairs of outcomes were never compared: every statement of
        // the routine is unproven, and nothing here is exact.
        c.cut_a.insert(c.sa.cfg.entry);
        c.cut_b.insert(c.sb.cfg.entry);
        c.incomplete = true;
    }
    c.budget_findings();
    // A replaced routine-wide finding may have brought assumptions that no
    // finding uses any more.
    let used: HashSet<String> = c
        .out
        .findings
        .iter()
        .flat_map(|f| f.assumptions.iter().cloned())
        .collect();
    c.out.assumptions.retain(|x| used.contains(&x.id));
    // Exact, and no divergence the assumptions do not cover: a finding that
    // is different or unknown keeps the transaction statements unproven too.
    c.out.txn_compared = !c.incomplete
        && c.out
            .findings
            .iter()
            .all(|f| matches!(f.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions));
    let stats = PairStats {
        spent: c.spent.get(),
        cut: c.out_of_budget.get(),
        tested: c
            .prior_tested
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
    };
    (c.out, stats)
}

/// What `analyze_pair` reports besides the behaviour.
struct PairStats {
    spent: u64,
    cut: bool,
    /// Unsalted prints tested against the earlier pairs' findings, with the
    /// answer.
    tested: Vec<(String, bool)>,
}

/// Where a routine pair stands in its file's shared budgets when it runs
/// alone (see [`analyze_one_pair`]).
#[derive(Debug, Clone, Default)]
pub struct PairEntry {
    /// `diff` calls the file had spent before the pair.
    pub file_spent: u64,
    /// Metered work since the analysis began, before the pair.
    pub work_before: u64,
    /// Fingerprints of the findings of the pairs before it.
    pub prior: Vec<String>,
}

/// The analysis of the `index`th routine pair of a file that `meet` pairs,
/// run alone: the pair's own collector, with the file's shared state (the
/// `diff` calls and work already spent, the fingerprints already seen) given
/// as `entry` instead of left by the pairs before it. For the entry a
/// whole-file analysis had at that pair this returns exactly the pair's
/// result in it (`pair_tests`). `None` when the file is not analysed pair by
/// pair or `index` is out of range.
pub fn analyze_one_pair(
    a: &Analyzed,
    b: &Analyzed,
    env: &Env,
    index: usize,
    entry: &PairEntry,
) -> Option<PairRun> {
    debug_assert!(
        sqldiff_model::work::in_scope(),
        "behaviour::analyze_one_pair needs an open work scope"
    );
    let start = sqldiff_model::work::now().total();
    let m = meet(a, b)?;
    let (x, y) = m.pairs.get(index)?;
    // The file cap is a function of every pair of the file.
    let largest = if env.work_caps.is_some() {
        0
    } else {
        m.pairs
            .iter()
            .map(|(x, y)| unit_work_cap(pair_statements(a, b, Some(x.clone()), Some(y.clone()))))
            .max()
            .unwrap_or(0)
    };
    let file_work = FileWork {
        start,
        cap: file_work_cap(m.pairs.len().max(1) as u64, largest),
        // Work done before the pair, less what this call spent on the file
        // cap above (the whole-file analysis spent it before its first pair).
        offset: entry
            .work_before
            .saturating_sub(sqldiff_model::work::now().total().saturating_sub(start)),
    };
    let file_spent = Rc::new(Cell::new(entry.file_spent));
    let prior: Rc<HashSet<String>> = Rc::new(entry.prior.iter().cloned().collect());
    let name = sqldiff_syntax::ast::routine_name(x).unwrap_or_default();
    let work_before = sqldiff_model::work::now()
        .total()
        .saturating_sub(start)
        .saturating_add(file_work.offset);
    let (r, stats) = analyze_pair(
        a,
        b,
        env,
        Some(x.clone()),
        Some(y.clone()),
        &format!("{name}|"),
        &prior,
        &file_spent,
        file_work,
    );
    Some(PairRun {
        file_spent_before: entry.file_spent,
        file_spent_after: file_spent.get(),
        work_before,
        work_after: sqldiff_model::work::now()
            .total()
            .saturating_sub(start)
            .saturating_add(file_work.offset),
        spent: stats.spent,
        cut: stats.cut,
        prior: entry.prior.clone(),
        behaviour: r,
    })
}

fn rank(v: Verdict) -> u8 {
    match v {
        Verdict::Different => 3,
        Verdict::Unknown => 2,
        Verdict::EqualUnderAssumptions => 1,
        _ => 0,
    }
}

fn what_if_empty(env: &Env, kind: ErrorKind, text: String) -> WhatIf {
    WhatIf {
        scenario: ScenarioInfo {
            kind: ScenarioKind::Fault,
            statement: None,
            error_kind: Some(kind),
            line_a: None,
            line_b: None,
            caller_transaction: env.caller_txn,
            xact_abort: env.xact_abort,
            path: Vec::new(),
            text,
        },
        raisable_a: false,
        raisable_b: false,
        a: Vec::new(),
        b: Vec::new(),
        findings: Vec::new(),
        witnesses: Vec::new(),
        notes: Vec::new(),
    }
}

/// "What if this statement fails with this error kind?" for the statement
/// starting on `line` of side `side`, aligned to the other side by its write
/// (or, for statements without one, by kind and order).
pub fn what_if(
    a: &Analyzed,
    b: &Analyzed,
    side: crate::model::Side,
    line: usize,
    kind: ErrorKind,
    env: &Env,
) -> WhatIf {
    let on_a = side == crate::model::Side::A;
    // The routine pair that holds the line; a file with at most one routine
    // on each side is analysed whole.
    let (ra, rb) = match meet(a, b) {
        None => (
            routines_of(a).first().cloned(),
            routines_of(b).first().cloned(),
        ),
        Some(m) => {
            let holds = |r: &SyntaxNode| {
                let l = if on_a { a } else { b }.node_location(r);
                l.start_line <= line && line <= l.end_line
            };
            match m
                .pairs
                .iter()
                .find(|(x, y)| holds(if on_a { x } else { y }))
            {
                Some((x, y)) => (Some(x.clone()), Some(y.clone())),
                None => {
                    return WhatIf {
                        notes: vec![format!(
                            "line {line} is in no routine that has a counterpart on the other side"
                        )],
                        ..what_if_empty(env, kind, format!("No statement at line {line}"))
                    }
                }
            }
        }
    };
    let file_work = FileWork {
        start: sqldiff_model::work::now().total(),
        cap: file_work_cap(1, 0),
        offset: 0,
    };
    let mut c = Collector::new(a, b, env.clone(), ra, rb, Rc::new(Cell::new(0)), file_work);
    let (this, other) = if on_a { (&c.sa, &c.sb) } else { (&c.sb, &c.sa) };
    let mut notes = Vec::new();
    let empty = |notes: Vec<String>, text: String| WhatIf {
        notes,
        ..what_if_empty(env, kind, text)
    };
    let nodes = this.stmt_nodes();
    let Some(&n) = nodes
        .iter()
        .find(|n| this.line_no(**n) == Some(line))
        .or_else(|| {
            nodes
                .iter()
                .rfind(|n| this.line_no(**n).is_some_and(|l| l <= line))
        })
    else {
        notes.push(format!("no statement on or before line {line}"));
        return empty(notes, format!("No statement at line {line}"));
    };
    let key = this
        .cfg
        .node(n)
        .stmt()
        .and_then(|s| s.durable_effect())
        .map(|e| e.key.clone());
    let partner = match &key {
        Some(k) => other.cfg.node_of_effect(k),
        None => {
            // n-th statement of the same kind and action on both sides.
            let sig = |s: &SideIr, x: NodeId| {
                s.cfg
                    .node(x)
                    .stmt()
                    .map(|st| format!("{:?}|{:?}", st.kind, st.action))
            };
            let mine = sig(this, n);
            let ord = nodes
                .iter()
                .take_while(|x| **x != n)
                .filter(|x| sig(this, **x) == mine)
                .count();
            other
                .stmt_nodes()
                .into_iter()
                .filter(|x| sig(other, *x) == mine)
                .nth(ord)
        }
    };
    let Some(m) = partner else {
        notes.push(format!(
            "{} has no counterpart on {}",
            this.stmt_phrase(n),
            other.name()
        ));
        return empty(notes, format!("If {} fails", this.stmt_phrase(n)));
    };
    let (na, nb) = if on_a { (n, m) } else { (m, n) };
    let key = key.unwrap_or_else(|| format!("stmt@{na}:{nb}"));
    let (ra, rb) = (c.sa.raises(na), c.sb.raises(nb));
    for (s, r, x) in [(&c.sa, &ra, na), (&c.sb, &rb, nb)] {
        if !r.contains(&kind) {
            notes.push(format!(
                "on {} {} does not raise {} (profile `raises` table), so it runs normally there",
                s.name(),
                s.stmt_phrase(x),
                kind.phrase()
            ));
        }
    }
    let sc = Kind::Fault {
        key: key.clone(),
        kind,
        na,
        nb,
    };
    let (fa, fb) = (
        c.fault_scenario(na, &ra, kind),
        c.fault_scenario(nb, &rb, kind),
    );
    let (pa, pb) = c.run(sc.clone(), fa, fb);
    let scenario = match pa.first() {
        Some(o) => scenario_info(&c.sa, &c.sb, &sc, env, o),
        None => {
            notes.push("no path reaches the statement".into());
            scenario_info(&c.sa, &c.sb, &sc, env, &Outcome::empty())
        }
    };
    WhatIf {
        scenario,
        raisable_a: ra.contains(&kind),
        raisable_b: rb.contains(&kind),
        a: pa.iter().map(|o| side_outcome(&c.sa, o)).collect(),
        b: pb.iter().map(|o| side_outcome(&c.sb, o)).collect(),
        findings: std::mem::take(&mut c.out.findings),
        witnesses: std::mem::take(&mut c.out.witnesses),
        notes,
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;
    use crate::model::Side;

    const SRC: &str =
        "CREATE PROCEDURE dbo.p @p int AS\nBEGIN\nSET @p = @p + 1;\nSELECT @p;\nEND;\n";

    /// The cap guard of `work_cap_used`: a collector whose unit cap is used
    /// up reports it, except while recomputing a flipped scenario or muted.
    #[test]
    fn the_cap_guard_holds_off_in_a_replay_and_when_muted() {
        let art = crate::Artifact::new("a.sql", Dialect::Tsql, SRC);
        let (a, b) = (
            Analyzed::new(&art, Side::A, 3),
            Analyzed::new(&art, Side::B, 3),
        );
        let env = Env {
            work_caps: Some((0, 0)),
            ..Env::from_flips(&BTreeMap::new())
        };
        let file_work = FileWork {
            start: sqldiff_model::work::now().total(),
            cap: 0,
            offset: 0,
        };
        let (ra, rb) = (
            routines_of(&a).first().cloned(),
            routines_of(&b).first().cloned(),
        );
        let c = Collector::new(&a, &b, env, ra, rb, Rc::new(Cell::new(0)), file_work);
        assert!(c.work_cap_used(), "a zero cap is used up");
        c.replay.set(Some(5));
        assert!(!c.work_cap_used(), "not while recomputing a scenario");
        c.replay.set(None);
        assert!(c.work_cap_used());
        sqldiff_model::work::muted(|| {
            assert!(!c.work_cap_used(), "not while muted");
        });
        assert!(c.work_cap_used());
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::{feasible, norm_cond, pairs, Outcome};
    use std::collections::{HashMap, HashSet};

    /// The string-signature implementation `pairs` replaced.
    fn pairs_reference(pa: &[Outcome], pb: &[Outcome]) -> (Vec<(usize, usize)>, bool) {
        if pa.len() == 1 && pb.len() == 1 {
            return (vec![(0, 0)], true);
        }
        let sig = |o: &Outcome| -> Vec<(String, bool)> {
            o.signature
                .iter()
                .map(|(c, b)| (norm_cond(c), *b))
                .collect()
        };
        let (siga, sigb): (Vec<_>, Vec<_>) =
            (pa.iter().map(sig).collect(), pb.iter().map(sig).collect());
        let sb: HashSet<_> = sigb.iter().collect();
        let sa: HashSet<_> = siga.iter().collect();
        if sa == sb {
            let mut first: HashMap<&Vec<(String, bool)>, usize> = HashMap::new();
            for (j, s) in sigb.iter().enumerate() {
                first.entry(s).or_insert(j);
            }
            let v = siga
                .iter()
                .enumerate()
                .filter_map(|(i, s)| first.get(s).map(|&j| (i, j)))
                .collect();
            return (v, true);
        }
        let mut v = Vec::new();
        for (i, x) in siga.iter().enumerate() {
            for (j, y) in sigb.iter().enumerate() {
                if feasible(x, y) {
                    v.push((i, j));
                }
            }
        }
        (v, false)
    }

    fn outcome(x: &[(&str, bool)]) -> Outcome {
        let mut o = Outcome::empty();
        o.signature = p(x).into_iter().map(|(c, b)| (c.into(), b)).collect();
        o
    }

    #[test]
    fn pairs_match_the_string_signature_reference() {
        // Spellings that fold alike ("[X] = 1", "x = 1"), repeated
        // conditions, duplicate paths, equal and unequal path sets.
        let conds = ["x = 1", "[X] = 1", "y > 2", "Y  >  2", "z", "w"];
        let seed = std::cell::Cell::new(12345u64);
        let next = |n: usize| {
            seed.set(
                seed.get()
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407),
            );
            ((seed.get() >> 33) as usize) % n
        };
        for _ in 0..400 {
            let side = |len: usize| -> Vec<Outcome> {
                (0..len)
                    .map(|_| {
                        let n = next(4);
                        let path: Vec<(&str, bool)> =
                            (0..n).map(|_| (conds[next(6)], next(2) == 0)).collect();
                        outcome(&path)
                    })
                    .collect()
            };
            let (la, lb) = (1 + next(5), 1 + next(5));
            let (pa, mut pb) = (side(la), side(lb));
            assert_eq!(pairs(&pa, &pb), pairs_reference(&pa, &pb));
            // Same set of paths, spelled differently and reordered.
            pb = pa
                .iter()
                .rev()
                .map(|o| {
                    let mut o = o.clone();
                    for (c, _) in &mut o.signature {
                        *c = c.to_uppercase().into();
                    }
                    o
                })
                .collect();
            assert_eq!(pairs(&pa, &pb), pairs_reference(&pa, &pb));
        }
    }

    fn p(x: &[(&str, bool)]) -> Vec<(String, bool)> {
        x.iter().map(|(c, v)| (c.to_string(), *v)).collect()
    }

    #[test]
    fn the_same_condition_with_two_values_is_impossible() {
        assert!(!feasible(&p(&[("c", true)]), &p(&[("c", false)])));
        assert!(!feasible(
            &p(&[("c", true), ("d", true)]),
            &p(&[("c", true), ("d", false)])
        ));
    }

    #[test]
    fn a_prefix_and_its_extensions_pair() {
        assert!(feasible(
            &p(&[("c", true)]),
            &p(&[("c", true), ("d", false)])
        ));
        assert!(feasible(&p(&[]), &p(&[("c", true)])));
    }

    #[test]
    fn different_conditions_say_nothing() {
        // Another order, or another spelling of the same condition.
        assert!(feasible(&p(&[("c", true)]), &p(&[("d", false)])));
        assert!(feasible(
            &p(&[("c", true), ("d", true)]),
            &p(&[("d", false), ("c", true)])
        ));
    }

    #[test]
    fn a_condition_decided_twice_is_not_compared() {
        // A loop test has a value per iteration.
        assert!(feasible(
            &p(&[("w", true), ("w", false)]),
            &p(&[("w", true), ("w", true), ("w", false)])
        ));
        assert!(feasible(
            &p(&[("w", true)]),
            &p(&[("w", false), ("w", true)])
        ));
    }
}

/// One routine pair's pair-state measurement (see `interp::pairs`).
#[derive(Debug, Clone)]
pub struct PairRow {
    pub routine: String,
    pub lines_a: usize,
    pub lines_b: usize,
    pub nodes_a: usize,
    pub nodes_b: usize,
    /// "baseline" or "caller-txn" (the environment of the run).
    pub env: &'static str,
    /// Unknown branch tests (decisions) in the two CFGs.
    pub decisions_a: usize,
    pub decisions_b: usize,
    /// Decisions whose condition the other side also decides, under the
    /// folding `pairs` uses and the lenient one of the last variant.
    pub matched_a: [usize; 2],
    pub matched_b: [usize; 2],
    /// What the production explorer finds under the same environment: paths
    /// per side (at most 128 each) and whether a budget cut any of them.
    pub prod_paths_a: usize,
    pub prod_paths_b: usize,
    pub prod_cut: bool,
    /// The same with path joining switched on for the side (whatever the
    /// other side is): paths left, paths joined, whether a budget still cuts.
    pub join_paths_a: usize,
    pub join_paths_b: usize,
    pub joined_a: usize,
    pub joined_b: usize,
    pub join_cut: bool,
    /// The other side makes no decision, so joining is used in production.
    pub gate_a: bool,
    pub gate_b: bool,
    /// Perfect-alignment bound: each side against a copy of itself, with the
    /// `agree` key (every decision matches, nothing is solo): what pair-state
    /// merging needs when the conditions pair up completely.
    pub self_a: super::interp::pairs::Measure,
    pub self_b: super::interp::pairs::Measure,
    /// The key variants, in the order of [`PAIR_VARIANTS`].
    pub measures: Vec<super::interp::pairs::Measure>,
}

/// strict+steps, strict, agree+steps, agree (no steps, dead loops dropped),
/// agree with local-variable prefixes also ignored when matching conditions,
/// and that with the uncommitted-log abstraction.
pub const PAIR_VARIANTS: [super::interp::pairs::Variant; 6] = {
    use super::interp::pairs::Variant as V;
    const fn v(agree: bool, steps: bool, deadloop: bool, lenient: bool, abstract_log: bool) -> V {
        V {
            agree,
            steps,
            deadloop,
            lenient,
            abstract_log,
        }
    }
    [
        v(false, true, false, false, false),
        v(false, false, true, false, false),
        v(true, true, false, false, false),
        v(true, false, true, false, false),
        v(true, false, true, true, false),
        v(true, false, true, true, true),
    ]
};

/// The conditions the explorer treats as one decision because their folded
/// text is equal (`norm_cond`; `pairs()` and `feasible()`), per routine pair:
/// the start offsets of the `Test::Unknown` nodes on each side that carry it.
/// Report only (perf plan W6c, E2): nothing in `analyze` calls it.
#[derive(Debug, Clone)]
pub struct SharedText {
    pub routine: String,
    pub text: String,
    pub a: Vec<usize>,
    pub b: Vec<usize>,
}

pub fn shared_conditions(a: &Analyzed, b: &Analyzed) -> Vec<SharedText> {
    let pairs: Vec<(Option<SyntaxNode>, Option<SyntaxNode>, String)> = match meet(a, b) {
        None => vec![(
            routines_of(a).first().cloned(),
            routines_of(b).first().cloned(),
            String::new(),
        )],
        Some(m) => m
            .pairs
            .iter()
            .map(|(x, y)| {
                (
                    Some(x.clone()),
                    Some(y.clone()),
                    sqldiff_syntax::ast::routine_name(x).unwrap_or_default(),
                )
            })
            .collect(),
    };
    let tests = |c: &Cfg| -> BTreeMap<String, Vec<usize>> {
        let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for n in &c.nodes {
            if let super::cfg::Op::Test {
                test: super::cfg::Test::Unknown(t),
                ..
            } = &n.op
            {
                if let Some(s) = &n.syntax {
                    m.entry(norm_cond(t))
                        .or_default()
                        .push(sqldiff_syntax::ast::sig_range(s).0);
                }
            }
        }
        m
    };
    let mut out = Vec::new();
    for (ra, rb, name) in pairs {
        let (sa, sb) = (SideIr::new(a, b, ra), SideIr::new(b, a, rb));
        let (ta, tb) = (tests(&sa.cfg), tests(&sb.cfg));
        for (text, xs) in ta {
            if let Some(ys) = tb.get(&text) {
                out.push(SharedText {
                    routine: name.clone(),
                    text,
                    a: xs,
                    b: ys.clone(),
                });
            }
        }
    }
    out
}

/// Measures the lockstep pair-state counts of every routine pair of `a` and
/// `b` (the pairs `analyze` would analyse). On demand only: nothing in
/// `analyze` calls it, so verdicts cannot depend on it.
pub fn pair_states(a: &Analyzed, b: &Analyzed, env: &Env, max_states: usize) -> Vec<PairRow> {
    let pairs: Vec<(Option<SyntaxNode>, Option<SyntaxNode>, String)> = match meet(a, b) {
        None => vec![(
            routines_of(a).first().cloned(),
            routines_of(b).first().cloned(),
            String::new(),
        )],
        Some(m) => m
            .pairs
            .iter()
            .map(|(x, y)| {
                (
                    Some(x.clone()),
                    Some(y.clone()),
                    sqldiff_syntax::ast::routine_name(x).unwrap_or_default(),
                )
            })
            .collect(),
    };
    let mut rows = Vec::new();
    for (ra, rb, name) in pairs {
        let (sa, sb) = (SideIr::new(a, b, ra.clone()), SideIr::new(b, a, rb.clone()));
        let keys = KeyUniverse::of(&[&sa.cfg, &sb.cfg]);
        let dec = |c: &Cfg| {
            c.nodes
                .iter()
                .filter(|n| {
                    matches!(
                        &n.op,
                        super::cfg::Op::Test {
                            test: super::cfg::Test::Unknown(_) | super::cfg::Test::NoRows(_),
                            ..
                        }
                    )
                })
                .count()
        };
        let lines = |r: &Option<SyntaxNode>, an: &Analyzed| {
            r.as_ref().map_or(0, |r| {
                let l = an.node_location(r);
                l.end_line + 1 - l.start_line
            })
        };
        let mut envs = vec![("baseline", env.scenario())];
        if !env.caller_txn {
            envs.push((
                "caller-txn",
                Scenario {
                    caller_txn: true,
                    ..env.scenario()
                },
            ));
        }
        let tests = |c: &Cfg, lenient: bool| -> Vec<String> {
            c.nodes
                .iter()
                .filter_map(|n| match &n.op {
                    super::cfg::Op::Test {
                        test: super::cfg::Test::Unknown(t),
                        ..
                    } => Some(super::interp::pairs::canon(t, lenient)),
                    _ => None,
                })
                .collect()
        };
        let matched = |x: &Cfg, y: &Cfg, lenient: bool| {
            let ys: HashSet<String> = tests(y, lenient).into_iter().collect();
            tests(x, lenient).iter().filter(|t| ys.contains(*t)).count()
        };
        for (label, sc) in envs {
            let (xa, xb) = (
                Explorer::in_universe(&sa.cfg, &sc, &keys),
                Explorer::in_universe(&sb.cfg, &sc, &keys),
            );
            let cut = xa.base().iter().chain(xb.base()).any(|o| !o.cut.is_empty());
            let (ja, jb) = (
                Explorer::in_universe_joined(&sa.cfg, &sc, &keys, true),
                Explorer::in_universe_joined(&sb.cfg, &sc, &keys, true),
            );
            let join_cut = ja.base().iter().chain(jb.base()).any(|o| !o.cut.is_empty());
            rows.push(PairRow {
                join_paths_a: ja.base().len(),
                join_paths_b: jb.base().len(),
                joined_a: ja.joined(),
                joined_b: jb.joined(),
                join_cut,
                gate_a: super::interp::decision_free(&sb.cfg),
                gate_b: super::interp::decision_free(&sa.cfg),
                matched_a: [
                    matched(&sa.cfg, &sb.cfg, false),
                    matched(&sa.cfg, &sb.cfg, true),
                ],
                matched_b: [
                    matched(&sb.cfg, &sa.cfg, false),
                    matched(&sb.cfg, &sa.cfg, true),
                ],
                prod_paths_a: xa.base().len(),
                prod_paths_b: xb.base().len(),
                prod_cut: cut,
                self_a: super::interp::pairs::measure(
                    &sa.cfg,
                    &sa.cfg,
                    &keys,
                    &sc,
                    &sc,
                    PAIR_VARIANTS[3],
                    max_states,
                ),
                self_b: super::interp::pairs::measure(
                    &sb.cfg,
                    &sb.cfg,
                    &keys,
                    &sc,
                    &sc,
                    PAIR_VARIANTS[3],
                    max_states,
                ),
                routine: name.clone(),
                lines_a: lines(&ra, a),
                lines_b: lines(&rb, b),
                nodes_a: sa.cfg.nodes.len(),
                nodes_b: sb.cfg.nodes.len(),
                env: label,
                decisions_a: dec(&sa.cfg),
                decisions_b: dec(&sb.cfg),
                measures: PAIR_VARIANTS
                    .iter()
                    .map(|v| {
                        super::interp::pairs::measure(
                            &sa.cfg, &sb.cfg, &keys, &sc, &sc, *v, max_states,
                        )
                    })
                    .collect(),
            });
        }
    }
    rows
}

/// The proof test of W4.4 step 1 (design `w4-4-routine-units.md`, sections E
/// to G): a routine pair run alone with the file's shared state injected
/// returns exactly what it returned inside the whole-file analysis, for the
/// budgets that bind (the diff-count budgets and the work caps) at every
/// position of the cut.
#[cfg(test)]
mod pair_tests {
    use super::*;
    use crate::model::Side;

    fn tsql(n: usize, branches: usize) -> String {
        (0..n)
            .map(|i| {
                let ifs: String = (0..branches + i % 3)
                    .map(|k| {
                        format!(
                            "IF @x > {k} UPDATE dbo.t SET c = c + {i} WHERE id = @x; ELSE INSERT INTO dbo.t (id, c) VALUES (@x, {k});\n"
                        )
                    })
                    .collect();
                format!("CREATE PROCEDURE dbo.p{i} @x int AS\nBEGIN\n{ifs}END;\nGO\n")
            })
            .collect()
    }

    /// The PostgreSQL side; routine `skew` has one branch changed, so the
    /// files carry differences as well as equalities.
    fn pg(n: usize, branches: usize, skew: Option<usize>) -> String {
        (0..n)
            .map(|i| {
                let ifs: String = (0..branches + i % 3)
                    .map(|k| {
                        let add = if skew == Some(i) && k == 0 { i + 1 } else { i };
                        format!(
                            "IF x > {k} THEN UPDATE t SET c = c + {add} WHERE id = x; ELSE INSERT INTO t (id, c) VALUES (x, {k}); END IF;\n"
                        )
                    })
                    .collect();
                format!(
                    "CREATE PROCEDURE p{i}(x int) LANGUAGE plpgsql AS $$\nBEGIN\n{ifs}END;\n$$;\n"
                )
            })
            .collect()
    }

    fn analysed(a: &str, b: &str, env: &Env) -> (Behaviour, Vec<PairRun>) {
        let (ta, tb) = (
            crate::Artifact::new("a.sql", Dialect::Tsql, a),
            crate::Artifact::new("b.sql", Dialect::Postgres, b),
        );
        let (x, y) = (
            Analyzed::new(&ta, Side::A, 3),
            Analyzed::new(&tb, Side::B, 3),
        );
        let mut runs = Vec::new();
        let (out, _) =
            sqldiff_model::work::scope("pair test", || analyze_runs(&x, &y, env, Some(&mut runs)));
        (out, runs)
    }

    fn alone(a: &str, b: &str, env: &Env, index: usize, entry: &PairEntry) -> Option<PairRun> {
        let (ta, tb) = (
            crate::Artifact::new("a.sql", Dialect::Tsql, a),
            crate::Artifact::new("b.sql", Dialect::Postgres, b),
        );
        let (x, y) = (
            Analyzed::new(&ta, Side::A, 3),
            Analyzed::new(&tb, Side::B, 3),
        );
        sqldiff_model::work::scope("pair test", || analyze_one_pair(&x, &y, env, index, entry)).0
    }

    /// Every pair of `runs` run alone with its recorded entry equals its run.
    fn check(a: &str, b: &str, env: &Env, label: &str) -> (usize, usize) {
        let (_, runs) = analysed(a, b, env);
        assert!(
            !runs.is_empty(),
            "{label}: the file is not analysed per pair"
        );
        let cut = runs.iter().filter(|r| r.cut).count();
        for (i, r) in runs.iter().enumerate() {
            let entry = PairEntry {
                file_spent: r.file_spent_before,
                work_before: r.work_before,
                prior: r.prior.clone(),
            };
            let one = alone(a, b, env, i, &entry)
                .unwrap_or_else(|| panic!("{label}: pair {i} did not run alone"));
            let show = |x: &PairRun| {
                format!(
                    "{:?} {} {} {} {} {} {}",
                    x.behaviour,
                    x.file_spent_before,
                    x.file_spent_after,
                    x.work_before,
                    x.work_after,
                    x.spent,
                    x.cut
                )
            };
            assert_eq!(show(&one), show(r), "{label}: pair {i} alone differs");
        }
        (runs.len(), cut)
    }

    #[test]
    fn a_pair_alone_equals_the_pair_in_its_file() {
        let (a, b) = (tsql(6, 4), pg(6, 4, Some(2)));
        let (n, cut) = check(&a, &b, &Env::default(), "no budget binds");
        assert_eq!((n, cut), (6, 0), "nothing is cut with the default budgets");
    }

    #[test]
    fn a_pair_alone_equals_the_pair_in_its_file_when_the_budgets_bind() {
        let (a, b) = (tsql(6, 3), pg(6, 3, Some(3)));
        // Learn what the pairs spend, then place each budget so that the cut
        // falls before, exactly at, inside and after the pairs.
        let (_, runs) = analysed(&a, &b, &Env::default());
        let total_spent = runs.last().unwrap().file_spent_after;
        let total_work = runs.last().unwrap().work_after;
        assert!(
            total_spent > 8 && total_work > 8,
            "the pairs spend something"
        );
        let mut cuts = BTreeSet::new();
        for r in &runs {
            // Exactly at a pair's boundary, one before it and one after.
            for d in [
                r.file_spent_before,
                r.file_spent_before + 1,
                r.file_spent_after,
            ] {
                for e in [d.saturating_sub(1), d, d + 1] {
                    let env = Env {
                        work_budgets: Some((1 << 21, e)),
                        ..Env::default()
                    };
                    let (n, cut) = check(&a, &b, &env, &format!("diff budget {e}"));
                    assert_eq!(n, 6);
                    cuts.insert(cut);
                }
            }
            for d in [r.work_before, r.work_after] {
                for e in [d.saturating_sub(1), d, d + 1] {
                    let env = Env {
                        work_caps: Some((u64::MAX / 2, e)),
                        ..Env::default()
                    };
                    let (_, cut) = check(&a, &b, &env, &format!("work cap {e}"));
                    cuts.insert(cut);
                }
            }
        }
        assert!(
            cuts.len() > 3,
            "the budgets cut different numbers of pairs: {cuts:?}"
        );
    }

    /// A routine of the perturbation files: it uses a temp table, calls the
    /// routine `callee`, branches, and writes.
    fn t_routine(name: &str, temp: &str, callee: &str, lit: u32) -> String {
        format!(
            "CREATE PROCEDURE dbo.{name} @x int AS\nBEGIN\nSET NOCOUNT ON;\nCREATE TABLE #{temp} (id int);\nINSERT INTO #{temp} VALUES (@x);\nEXEC dbo.{callee} @x;\nIF @x > {lit} UPDATE dbo.t SET c = c + 1 WHERE id = @x; ELSE INSERT INTO dbo.t (id, c) VALUES (@x, {lit});\nEND;\nGO\n"
        )
    }

    fn p_routine(name: &str, temp: &str, callee: &str, lit: u32) -> String {
        format!(
            "CREATE PROCEDURE {name}(x int) LANGUAGE plpgsql AS $$\nBEGIN\nCREATE TEMP TABLE {temp} (id int);\nINSERT INTO {temp} VALUES (x);\nCALL {callee}(x);\nIF x > {lit} THEN UPDATE t SET c = c + 1 WHERE id = x; ELSE INSERT INTO t (id, c) VALUES (x, {lit}); END IF;\nEND;\n$$;\n"
        )
    }

    type Routines = Vec<(String, String)>;

    /// Routines `r0..r6` (each calling its successor, the last one calling
    /// `late`, which is not defined), under `edit`.
    fn files(edit: &dyn Fn(&mut Routines)) -> (String, String) {
        let mut v: Routines = (0..7u32)
            .map(|i| {
                let callee = if i < 6 {
                    format!("r{}", i + 1)
                } else {
                    "late".into()
                };
                (
                    t_routine(&format!("r{i}"), &format!("tmp{i}"), &callee, 10 + i),
                    p_routine(&format!("r{i}"), &format!("tmp{i}"), &callee, 10 + i),
                )
            })
            .collect();
        edit(&mut v);
        (
            v.iter().map(|x| x.0.as_str()).collect(),
            v.iter().map(|x| x.1.as_str()).collect(),
        )
    }

    /// The soundness of the key: a cache primed on one file serves every
    /// edit of it that leaves a pair's key alone, and what it serves is what
    /// a fresh analysis gives. The edits are the ones that could reach a pair
    /// from outside its routine: text before and after it, a table another
    /// routine writes, a routine defined later, and a temporary table that a
    /// later routine creates or that a pair reads without creating.
    #[test]
    fn a_cache_primed_on_one_file_serves_any_edit_of_it_as_a_fresh_run_would() {
        type Edit = Box<dyn Fn(&mut Routines)>;
        let reads = |v: &mut Routines| {
            v[3].0 = v[3]
                .0
                .replace("EXEC dbo.r4 @x;", "SELECT id FROM #tmp9; EXEC dbo.r4 @x;");
            v[3].1 = v[3]
                .1
                .replace("CALL r4(x);", "PERFORM id FROM tmp9; CALL r4(x);");
        };
        let append = |name: &'static str, temp: &'static str, callee: &'static str, lit: u32| {
            Box::new(move |v: &mut Routines| {
                v.push((
                    t_routine(name, temp, callee, lit),
                    p_routine(name, temp, callee, lit),
                ));
            }) as Edit
        };
        let cases: Vec<(&str, Edit)> = vec![
            (
                "literal in r1 (before)",
                Box::new(|v| {
                    v[1].0 = v[1].0.replace("11", "12");
                    v[1].1 = v[1].1.replace("11", "12");
                }),
            ),
            (
                "longer literal in r1 (before; later routines move)",
                Box::new(|v| {
                    v[1].0 = v[1].0.replace("11", "1100");
                    v[1].1 = v[1].1.replace("11", "1100");
                }),
            ),
            (
                "literal in r5 (after)",
                Box::new(|v| {
                    v[5].0 = v[5].0.replace("15", "16");
                    v[5].1 = v[5].1.replace("15", "16");
                }),
            ),
            (
                "r5 writes another table",
                Box::new(|v| {
                    v[5].0 = v[5].0.replace("dbo.t ", "dbo.u ");
                    v[5].1 = v[5].1.replace(" t ", " u ");
                }),
            ),
            (
                "late routine defined at the end",
                append("late", "tmpz", "r0", 99),
            ),
            (
                "a later routine uses a temp table named like pair 3's",
                append("z1", "tmp3", "r0", 98),
            ),
            (
                "a later routine uses a temp table pair 3 does not",
                append("z1", "tmp9", "r0", 98),
            ),
            ("unrelated routine appended", append("z2", "tmpy", "r0", 97)),
            ("pair 3 reads a temp table nobody creates", Box::new(reads)),
            (
                "pair 3 reads a temp table a later routine creates",
                Box::new(move |v| {
                    reads(v);
                    v.push((
                        t_routine("z1", "tmp9", "r0", 98),
                        p_routine("z1", "tmp9", "r0", 98),
                    ));
                }),
            ),
        ];
        let (a0, b0) = files(&|_| {});
        let mut served = 0;
        for (name, edit) in &cases {
            let cache = std::sync::Arc::new(MemCache::default());
            let env = with_cache(&Env::default(), &cache);
            let _ = analysed(&a0, &b0, &env);
            cache.reset_events();
            let (a, b) = files(&**edit);
            let (cached, cached_runs) = analysed(&a, &b, &env);
            let (fresh, fresh_runs) = analysed(&a, &b, &Env::default());
            assert_eq!(
                shown(&cached_runs, &cached),
                shown(&fresh_runs, &fresh),
                "{name}"
            );
            served += cache.count(PairEvent::Reused);
        }
        assert!(served > 0, "no case reused a pair: the test proves nothing");
    }

    /// A cache in memory, counting what happened.
    #[derive(Default)]
    struct MemCache {
        map: std::sync::Mutex<BTreeMap<String, Vec<u8>>>,
        events: std::sync::Mutex<Vec<PairEvent>>,
        recheck: bool,
    }

    impl PairCache for MemCache {
        fn build(&self) -> &str {
            "test-build"
        }
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.map.lock().unwrap().get(key).cloned()
        }
        fn put(&self, key: &str, bytes: &[u8]) {
            self.map.lock().unwrap().insert(key.into(), bytes.to_vec());
        }
        fn recheck(&self, _: &str) -> bool {
            self.recheck
        }
        fn note(&self, event: PairEvent, _: &str) {
            self.events.lock().unwrap().push(event);
        }
    }

    impl MemCache {
        fn count(&self, e: PairEvent) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|x| **x == e)
                .count()
        }
        fn reset_events(&self) {
            self.events.lock().unwrap().clear();
        }
    }

    fn with_cache(env: &Env, cache: &std::sync::Arc<MemCache>) -> Env {
        Env {
            pair_cache: Some(PairCacheHandle(cache.clone())),
            ..env.clone()
        }
    }

    fn shown(runs: &[PairRun], out: &Behaviour) -> String {
        let mut s = format!("{out:?}\n");
        for r in runs {
            s += &format!(
                "{} {} {} {} {} {}\n",
                r.file_spent_before,
                r.file_spent_after,
                r.work_before,
                r.work_after,
                r.spent,
                r.cut
            );
        }
        s
    }

    /// Cold and warm runs with a cache equal the run without one, in the
    /// whole behaviour and in every pair's place in the file's budgets.
    /// Returns how many pairs the warm run took from the cache.
    fn cached_equals_fresh(a: &str, b: &str, env: &Env, label: &str) -> usize {
        let (fresh, fresh_runs) = analysed(a, b, env);
        let cache = std::sync::Arc::new(MemCache::default());
        let cached_env = with_cache(env, &cache);
        let (cold, cold_runs) = analysed(a, b, &cached_env);
        assert_eq!(
            shown(&cold_runs, &cold),
            shown(&fresh_runs, &fresh),
            "{label}: cold"
        );
        assert_eq!(
            cache.count(PairEvent::Reused),
            0,
            "{label}: cold run reused"
        );
        cache.reset_events();
        let (warm, warm_runs) = analysed(a, b, &cached_env);
        assert_eq!(
            shown(&warm_runs, &warm),
            shown(&fresh_runs, &fresh),
            "{label}: warm"
        );
        cache.count(PairEvent::Reused)
    }

    #[test]
    fn a_cached_pair_equals_the_fresh_one_under_every_budget() {
        let (a, b) = (tsql(6, 3), pg(6, 3, Some(3)));
        let hits = cached_equals_fresh(&a, &b, &Env::default(), "default budgets");
        assert_eq!(hits, 6, "every pair is taken when no budget binds");
        let (_, runs) = analysed(&a, &b, &Env::default());
        let mut taken = 0;
        for r in &runs {
            for d in [r.file_spent_before, r.file_spent_after] {
                for e in [d.saturating_sub(1), d, d + 1] {
                    let env = Env {
                        work_budgets: Some((1 << 21, e)),
                        ..Env::default()
                    };
                    taken += cached_equals_fresh(&a, &b, &env, &format!("diff budget {e}"));
                }
            }
            for d in [r.work_before, r.work_after] {
                for e in [d.saturating_sub(1), d, d + 1] {
                    let env = Env {
                        work_caps: Some((u64::MAX / 2, e)),
                        ..Env::default()
                    };
                    taken += cached_equals_fresh(&a, &b, &env, &format!("work cap {e}"));
                }
            }
        }
        assert!(taken > 0, "the budget runs took something from the cache");
    }

    #[test]
    fn an_edit_rebuilds_only_the_pair_it_changes() {
        let (a, b) = (tsql(6, 3), pg(6, 3, Some(3)));
        let cache = std::sync::Arc::new(MemCache::default());
        let env = with_cache(&Env::default(), &cache);
        let _ = analysed(&a, &b, &env);
        cache.reset_events();
        // The same length: one literal of the last routine, on both sides.
        let a2 = a.replacen("c + 5 WHERE", "c + 6 WHERE", 1);
        let b2 = b.replacen("c + 5 WHERE", "c + 6 WHERE", 1);
        assert_ne!(a, a2);
        let (cached, cached_runs) = analysed(&a2, &b2, &env);
        let (fresh, fresh_runs) = analysed(&a2, &b2, &Env::default());
        assert_eq!(shown(&cached_runs, &cached), shown(&fresh_runs, &fresh));
        assert_eq!(cache.count(PairEvent::Reused), 5);
        assert_eq!(cache.count(PairEvent::Built), 1);
    }

    /// A routine that moves in its file (an edit of an earlier routine made
    /// the text longer, on the same lines) keeps its stored result, and the
    /// result's byte offsets follow it.
    #[test]
    fn a_pair_that_moves_in_its_file_keeps_its_result_with_the_new_offsets() {
        let (a, b) = (tsql(6, 3), pg(6, 3, Some(3)));
        let cache = std::sync::Arc::new(MemCache::default());
        let env = with_cache(&Env::default(), &cache);
        let _ = analysed(&a, &b, &env);
        cache.reset_events();
        // Longer text on one line of the first routine only.
        let a2 = a.replacen("c + 0 WHERE", "c + 1000 WHERE", 1);
        let b2 = b.replacen("c + 0 WHERE", "c + 1000 WHERE", 1);
        assert_ne!(a, a2);
        let (cached, cached_runs) = analysed(&a2, &b2, &env);
        let (fresh, fresh_runs) = analysed(&a2, &b2, &Env::default());
        assert_eq!(shown(&cached_runs, &cached), shown(&fresh_runs, &fresh));
        // The offsets are observed: some finding has a location, and they
        // are the fresh ones (the comparison above is of the whole result).
        assert!(
            fresh
                .findings
                .iter()
                .any(|f| f.a.is_some() || f.b.is_some()),
            "the shapes below prove nothing without a located finding"
        );
        assert_eq!(cache.count(PairEvent::Reused), 5);
        assert_eq!(cache.count(PairEvent::Built), 1);
    }

    /// The identified condition pairs are positions in the file, and a
    /// routine that moves keeps its key: the key holds them by offset from
    /// the routine's start. (A set that does not move with its routine is
    /// another key, so a pair is built again, never taken wrongly.)
    #[test]
    fn identified_pairs_move_with_their_routines() {
        let (a, b) = (tsql(6, 3), pg(6, 3, Some(3)));
        let a2 = a.replacen("c + 0 WHERE", "c + 1000 WHERE", 1);
        let b2 = b.replacen("c + 0 WHERE", "c + 1000 WHERE", 1);
        assert_ne!(a, a2);
        // One identified pair inside every routine, as the file stands.
        let set = |a: &str, b: &str| {
            let mut s = BTreeSet::new();
            for (x, y) in a
                .match_indices("CREATE PROCEDURE")
                .zip(b.match_indices("CREATE PROCEDURE"))
            {
                s.insert((x.0 + 20, y.0 + 20));
            }
            std::sync::Arc::new(s)
        };
        let cache = std::sync::Arc::new(MemCache::default());
        let env = |a: &str, b: &str| Env {
            identified: Some(set(a, b)),
            ..with_cache(&Env::default(), &cache)
        };
        let _ = analysed(&a, &b, &env(&a, &b));
        cache.reset_events();
        let (cached, cached_runs) = analysed(&a2, &b2, &env(&a2, &b2));
        let plain = Env {
            identified: Some(set(&a2, &b2)),
            ..Env::default()
        };
        let (fresh, fresh_runs) = analysed(&a2, &b2, &plain);
        assert_eq!(shown(&cached_runs, &cached), shown(&fresh_runs, &fresh));
        assert_eq!(cache.count(PairEvent::Reused), 5);
        assert_eq!(cache.count(PairEvent::Built), 1);
    }

    #[test]
    fn a_sampled_pair_is_built_again_and_compared() {
        let (a, b) = (tsql(4, 3), pg(4, 3, None));
        let cache = std::sync::Arc::new(MemCache {
            recheck: true,
            ..MemCache::default()
        });
        let env = with_cache(&Env::default(), &cache);
        let (first, _) = analysed(&a, &b, &env);
        cache.reset_events();
        let (second, _) = analysed(&a, &b, &env);
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(cache.count(PairEvent::Verified), 4);
        assert_eq!(cache.count(PairEvent::Reused), 0);
        assert_eq!(cache.count(PairEvent::Mismatch), 0);
    }

    #[test]
    fn the_key_follows_what_the_pair_reads() {
        let (a, b) = (tsql(3, 2), pg(3, 2, None));
        let cache = std::sync::Arc::new(MemCache::default());
        let env = with_cache(&Env::default(), &cache);
        let _ = analysed(&a, &b, &env);
        // Another environment, other budgets: nothing is taken.
        cache.reset_events();
        let caller = Env {
            caller_txn: true,
            ..env.clone()
        };
        let _ = analysed(&a, &b, &caller);
        assert_eq!(cache.count(PairEvent::Reused), 0, "another Env");
        cache.reset_events();
        let _ = analysed(
            &a,
            &b,
            &with_cache(
                &Env {
                    skip_witnesses: true,
                    ..Env::default()
                },
                &cache,
            ),
        );
        assert_eq!(cache.count(PairEvent::Reused), 0, "skip_witnesses");
        // A tree-neutral change to the draft (a trailing comment in a routine
        // moves nothing else): the edited routine alone is built.
        cache.reset_events();
        let b2 = b.replacen("END;\n$$;", "-- c\nEND;\n$$;", 1);
        let _ = analysed(&a, &b2, &env);
        assert_eq!(cache.count(PairEvent::Built), 3, "the draft's lines shifted: pairs below and the edited one rebuild, positions are in the key");
    }

    #[test]
    fn a_collision_that_disappears_rebuilds_the_pair_that_had_it() {
        let (a, b) = (tsql(3, 2), pg(3, 2, None));
        let cache = std::sync::Arc::new(MemCache::default());
        let env = with_cache(&Env::default(), &cache);
        let _ = analysed(&a, &b, &env);
        // Routine p0 writes another table (the same length): the findings of
        // p1 no longer meet p0's prints, so p1 salts nothing now, and what
        // was stored for it (a collision) is not what it returns.
        let head = |s: &str, from: &str| s.find(from).unwrap();
        let (ia, ib) = (
            head(&a, "CREATE PROCEDURE dbo.p1"),
            head(&b, "CREATE PROCEDURE p1("),
        );
        let a2 = format!("{}{}", a[..ia].replace("dbo.t ", "dbo.u "), &a[ia..]);
        let b2 = format!("{}{}", b[..ib].replace(" t ", " u "), &b[ib..]);
        assert_ne!(a, a2);
        assert_ne!(b, b2);
        cache.reset_events();
        let (cached, cached_runs) = analysed(&a2, &b2, &env);
        let (fresh, fresh_runs) = analysed(&a2, &b2, &Env::default());
        assert_eq!(shown(&cached_runs, &cached), shown(&fresh_runs, &fresh));
        assert_eq!(cache.count(PairEvent::Built), 2, "p0 and p1");
        assert_eq!(cache.count(PairEvent::Reused), 1, "p2 keeps its collision");
    }
}
