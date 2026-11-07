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

use sqldiff_syntax::{ast, Dialect, SyntaxNode};

use super::cfg::{Cfg, Effect, NodeId};
use super::delta::scan_differing;
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
}

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
                c.to_lowercase(),
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
                condition: c.to_lowercase(),
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
    so.not_proven = o.imprecise.clone();
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

/// T-SQL compared against PostgreSQL or Db2 for i: the other side's
/// temporary tables, which a table variable may have been converted to.
fn other_temps(x: &Analyzed, other: &Analyzed) -> HashSet<String> {
    if x.dialect() == Dialect::Tsql && other.dialect() != Dialect::Tsql {
        crate::classify::temp_table_names(&other.root)
    } else {
        HashSet::new()
    }
}

/// Pairs the outcomes of both sides: by path signature when both sides
/// branch on the same conditions, otherwise every combination (and the
/// verdict is capped at unknown unless all combinations agree).
fn pairs(pa: &[Outcome], pb: &[Outcome]) -> (Vec<(usize, usize)>, bool) {
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
        // The first outcome of B with each signature.
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
    for i in 0..pa.len() {
        for j in 0..pb.len() {
            v.push((i, j));
        }
    }
    (v, false)
}

/// Behavioural findings, the assumptions they rest on, and a structured
/// witness per finding.
#[derive(Debug, Clone, Default)]
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
    prior: HashSet<String>,
    salt: String,
    /// Statements where a path of some scenario was cut by a budget, per
    /// side (see `Outcome::cut`).
    cut_a: BTreeSet<NodeId>,
    cut_b: BTreeSet<NodeId>,
    /// The divergences of a scenario with a default flipped (`persists`),
    /// by scenario.
    flipped: RefCell<HashMap<String, Rc<Divergences>>>,
    /// `diff` calls spent by this routine pair, and whether a budget ran out.
    spent: Cell<u64>,
    out_of_budget: Cell<bool>,
    /// `diff` calls spent by the whole file so far (shared by its routine
    /// pairs), and the budgets in force (routine, file).
    file_spent: Rc<Cell<u64>>,
    budgets: (u64, u64),
}

/// The (rule, anchor) of divergences found.
type Divergences = BTreeSet<(&'static str, String)>;

/// Flipped scenarios kept before the cache starts over (time only).
const FLIPPED_MAX: usize = 4096;

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
    ) -> Self {
        let env_budgets = env.work_budgets.unwrap_or((ROUTINE_DIFFS, FILE_DIFFS));
        let (sa, sb) = (SideIr::new(a, b, ra), SideIr::new(b, a, rb));
        let keys = KeyUniverse::of(&[&sa.cfg, &sb.cfg]);
        let al = Align::new(&keys, &sa, &sb);
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
            prior: HashSet::new(),
            salt: String::new(),
            cut_a: BTreeSet::new(),
            cut_b: BTreeSet::new(),
            flipped: RefCell::new(HashMap::new()),
            spent: Cell::new(0),
            out_of_budget: Cell::new(false),
            file_spent,
            budgets: env_budgets,
        };
        c.splits = splits(&c.sa, &c.sb, true);
        c.splits.extend(splits(&c.sb, &c.sa, false));
        c
    }

    /// Spends one `diff` call; false (and the routine marked as cut by its
    /// budget) when the routine's or the file's allowance is used up.
    fn spend(&self) -> bool {
        let file = self.file_spent.get();
        let (routine_max, file_max) = self.budgets;
        if self.spent.get() >= routine_max || file >= file_max {
            self.out_of_budget.set(true);
            return false;
        }
        self.spent.set(self.spent.get() + 1);
        self.file_spent.set(file + 1);
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
    fn explorer(
        cache: &RefCell<Vec<Rc<Explorer>>>,
        cfg: &Cfg,
        sc: &Scenario,
        keys: &KeyUniverse,
    ) -> Rc<Explorer> {
        let env = Scenario {
            faults: Vec::new(),
            ..sc.clone()
        };
        let mut c = cache.borrow_mut();
        if let Some(x) = c.iter().find(|x| *x.base_scenario() == env) {
            return x.clone();
        }
        let x = Rc::new(Explorer::in_universe(cfg, &env, keys));
        c.push(x.clone());
        x
    }

    fn outcomes(
        &self,
        sc: &Kind,
        scen_a: &Scenario,
        scen_b: &Scenario,
    ) -> (Vec<Outcome>, Vec<Outcome>) {
        let (ca, cb) = (&self.sa.cfg, &self.sb.cfg);
        let mut pa = Self::explorer(&self.xa, ca, scen_a, &self.keys).explore(ca, scen_a);
        let mut pb = Self::explorer(&self.xb, cb, scen_b, &self.keys).explore(cb, scen_b);
        if let Kind::Fault { key, .. } = sc {
            // Only paths that reach the failing statement belong to the scenario.
            let reached = |o: &Outcome| o.fault_hit || o.effects.contains_key(key.as_str());
            pa.retain(reached);
            pb.retain(reached);
        }
        (pa, pb)
    }

    /// Whether a divergence still shows with a default setting flipped; if
    /// so, the finding does not rest on the assumption behind that default.
    ///
    /// The divergences of the flipped scenario are worked out once for all
    /// findings that flip it (`flipped`): a scenario with many findings used
    /// to explore and compare every outcome pair again for each of them.
    fn persists(&self, sc: &Kind, scen_a: &Scenario, scen_b: &Scenario, d: &Div) -> bool {
        let key = format!("{sc:?}|{scen_a:?}|{scen_b:?}");
        let known = self.flipped.borrow().get(&key).cloned();
        let set = match known {
            Some(set) => set,
            None => {
                let set = Rc::new(self.divergences_of(sc, scen_a, scen_b));
                let mut c = self.flipped.borrow_mut();
                if c.len() >= FLIPPED_MAX {
                    c.clear();
                }
                c.insert(key, set.clone());
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
        if self.spent.get() >= self.budgets.0 || self.file_spent.get() >= self.budgets.1 {
            self.out_of_budget.set(true);
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
        caveats.extend(oa.imprecise.iter().chain(&ob.imprecise).cloned());
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
            || self.prior.contains(&fp)
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
                    witness: "Not proven: the behaviour model stopped at its step, path or work budget before it ran or compared this statement in at least one scenario, so what happens when it fails and how its transaction goes are not compared.".to_string(),
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
    // The `diff` calls all routine pairs of this file share.
    let file_spent = Rc::new(Cell::new(0));
    let Some(m) = meet(a, b) else {
        // At most one routine on each side: `routines_of` picks it, so a
        // setup `DO` block beside it is not mistaken for it.
        let (ra, rb) = (routines_of(a), routines_of(b));
        let mut out = analyze_pair(
            a,
            b,
            env,
            ra.first().cloned(),
            rb.first().cloned(),
            "",
            &HashSet::new(),
            &file_spent,
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
    let mut out = Behaviour::default();
    let mut prior: HashSet<String> = HashSet::new();
    let mut all_compared = m.only_a.is_empty() && m.only_b.is_empty();
    for (x, y) in &m.pairs {
        let name = sqldiff_syntax::ast::routine_name(x).unwrap_or_default();
        let r = analyze_pair(
            a,
            b,
            env,
            Some(x.clone()),
            Some(y.clone()),
            &format!("{name}|"),
            &prior,
            &file_spent,
        );
        prior.extend(r.findings.iter().map(|f| f.fingerprint.clone()));
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

/// The analysis of one pair of routines (or, with `None`, of the statements
/// outside any routine).
#[allow(clippy::too_many_arguments)]
fn analyze_pair(
    a: &Analyzed,
    b: &Analyzed,
    env: &Env,
    ra: Option<SyntaxNode>,
    rb: Option<SyntaxNode>,
    salt: &str,
    prior: &HashSet<String>,
    file_spent: &Rc<Cell<u64>>,
) -> Behaviour {
    let mut c = Collector::new(a, b, env.clone(), ra, rb, file_spent.clone());
    c.salt = salt.to_string();
    c.prior = prior.clone();
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
    c.out
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
    let mut c = Collector::new(a, b, env.clone(), ra, rb, Rc::new(Cell::new(0)));
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
            rows.push(PairRow {
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
