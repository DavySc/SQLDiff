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

use std::collections::{BTreeMap, BTreeSet, HashSet};

use sqldiff_syntax::{ast, SyntaxNode};

use super::cfg::{lower, Cfg, Effect, NodeId};
use super::interp::{explore, Exit, Fate, Fault, Outcome, Routed, Scenario, TempState};
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
        self.cfg
            .effect_nodes()
            .into_iter()
            .find(|(_, e)| e.key == key)
    }

    fn syntax(&self, n: NodeId) -> Option<&SyntaxNode> {
        self.cfg.node(n).syntax.as_ref()
    }

    fn line(&self, n: NodeId) -> String {
        self.syntax(n)
            .map(|s| format!("{:?} line {}", self.a.side, self.a.line_of(s)))
            .unwrap_or_default()
    }

    /// Effect phrase, with an ordinal when the same write occurs more than once.
    fn phrase(&self, key: &str) -> String {
        let Some((_, e)) = self.effect(key) else {
            return key.to_string();
        };
        let p = effect_phrase(e);
        let (base, n) = key.rsplit_once('#').unwrap_or((key, "1"));
        let repeated = self
            .cfg
            .effect_nodes()
            .iter()
            .filter(|(_, x)| x.key.rsplit_once('#').map(|y| y.0) == Some(base))
            .count()
            > 1;
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
        match self.effect(key) {
            Some((n, _)) => format!("{} ({})", self.phrase(key), self.line(n)),
            None => key.to_string(),
        }
    }

    /// Position of an effect in document order (for ordering divergences).
    fn order(&self, key: &str) -> usize {
        self.cfg
            .effect_nodes()
            .iter()
            .position(|(_, e)| e.key == key)
            .unwrap_or(usize::MAX)
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
        }
    }

    /// The environment of a diff: session flips plus declared facts.
    pub fn from_options(opts: &crate::Options) -> Env {
        Env {
            commit_level: opts
                .declared
                .get("ASM-DB2-COMMIT-LEVEL")
                .and_then(|t| crate::ledger::declared_commit_level(t)),
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

/// Differences between one outcome of each side under one scenario.
fn diff(
    sa: &SideIr,
    sb: &SideIr,
    splits: &[Split],
    sc: &Kind,
    oa: &Outcome,
    ob: &Outcome,
) -> Vec<Div> {
    let mut out = Vec::new();
    let fault_key = match sc {
        Kind::Fault { key, .. } => Some(key.as_str()),
        _ => None,
    };
    let both = |k: &String| sa.effect(k).is_some() && sb.effect(k).is_some();
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
                format!(
                    "{} rolls the statement back; {} keeps the rows it changed before the error, because statements are atomic only under commitment control",
                    q.name(),
                    p.name()
                ),
            );
            d.assumptions = vec!["ASM-MULTI-ROW".into()];
            d.durability = true;
            out.push(d);
        }
    }
    // 2. Effects that ran on both sides with different fates.
    let mut keys: Vec<&String> = oa.effects.keys().collect();
    for k in ob.effects.keys() {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys.sort_by_key(|k| (sa.order(k), sb.order(k)));
    let mut fate_diffs = Vec::new();
    let mut control_diffs = Vec::new();
    for k in keys {
        if Some(k.as_str()) == fault_key || !both(k) {
            continue;
        }
        let (fa, fb) = (oa.effects.get(k).copied(), ob.effects.get(k).copied());
        if fa == fb {
            continue;
        }
        let ran_before = oa.before_fault.contains(k) || ob.before_fault.contains(k);
        match (fa, fb) {
            (Some(_), Some(_)) => fate_diffs.push((k.clone(), fa, fb, ran_before)),
            _ => control_diffs.push((k.clone(), fa, fb)),
        }
    }
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
            format!(
                "{} {}; {} {}",
                sa.name(),
                fate_phrase(sa, k, *fa),
                sb.name(),
                fate_phrase(sb, k, *fb)
            ),
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
            let text = format!(
                "{} {}; {} {}",
                sa.name(),
                fate_phrase(sa, k, *fa),
                sb.name(),
                fate_phrase(sb, k, *fb)
            );
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
            format!(
                "{} {}; {} {}",
                sa.name(),
                exit_phrase(oa),
                sb.name(),
                exit_phrase(ob)
            ),
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
            let next = after_fault(oc)
                .map(|n| c.line(n))
                .filter(|l| !l.is_empty())
                .map(|l| format!(" ({l} runs next)"))
                .unwrap_or_default();
            let ends = match &oc.exit {
                Exit::Code(code) => format!("returns code {code}"),
                _ => "returns normally".to_string(),
            };
            out.push(Div::new(
                "txn.caller-continuation",
                ChangeClass::Transactional,
                Severity::High,
                fk,
                format!(
                    "{} reports the error and carries on{next}; the call {ends}, so the caller's batch goes on and can commit its work; {} raises the error, so the calling statement fails and a caller's transaction block is aborted",
                    c.name(),
                    x.name()
                ),
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
                op.effects.get(&s.part_key),
                Some(Fate::Durable | Fate::Partial)
            );
            let merge_kept = matches!(
                om.effects.get(&s.merge_key),
                Some(Fate::Durable | Fate::Partial)
            );
            if kept && !merge_kept && op.before_fault.contains(&s.part_key) {
                let mut d = Div::new(
                    "txn.atomicity",
                    ChangeClass::Transactional,
                    Severity::Critical,
                    &s.merge_key,
                    format!(
                        "{} does it in one statement, which fails as a whole and changes nothing; {} has already run {}, which stays durable",
                        m.name(),
                        p.name(),
                        p.phrase_at(&s.part_key)
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
    // 7. Temporary state seen by aligned statements.
    let reads = |s: &SideIr, o: &Outcome| -> BTreeSet<(String, String, TempState)> {
        o.temp_reads
            .iter()
            .filter_map(|(n, t, st)| {
                let key = s.cfg.node(*n).stmt()?.durable_effect()?.key.clone();
                Some((key, t.clone(), *st))
            })
            .collect()
    };
    let (ra, rb) = (reads(sa, oa), reads(sb, ob));
    if let Some((k, t, _)) = ra.symmetric_difference(&rb).next() {
        if both(k) {
            let state = |r: &BTreeSet<(String, String, TempState)>| {
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
                format!(
                    "{} reads the temporary table {}, which on {} {} and on {} {}",
                    sa.phrase(k),
                    t.to_lowercase(),
                    sa.name(),
                    state(&ra),
                    sb.name(),
                    state(&rb)
                ),
            ));
        }
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
    for (k, f) in &o.effects {
        let p = if k == super::interp::CALLER {
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
        .filter(|(k, f)| **f == Fate::Undone && o.before_fault.contains(*k))
        .map(|(k, _)| {
            if k == super::interp::CALLER {
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
fn norm_cond(c: &str) -> String {
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

/// Pairs the outcomes of both sides: by path signature when both sides
/// branch on the same conditions, otherwise every combination (and the
/// verdict is capped at unknown unless all combinations agree).
fn pairs<'o>(pa: &'o [Outcome], pb: &'o [Outcome]) -> (Vec<(&'o Outcome, &'o Outcome)>, bool) {
    if pa.len() == 1 && pb.len() == 1 {
        return (vec![(&pa[0], &pb[0])], true);
    }
    let sig = |o: &Outcome| -> Vec<(String, bool)> {
        o.signature
            .iter()
            .map(|(c, b)| (norm_cond(c), *b))
            .collect()
    };
    let sb: HashSet<_> = pb.iter().map(sig).collect();
    let sa: HashSet<_> = pa.iter().map(sig).collect();
    if sa == sb {
        let v = pa
            .iter()
            .filter_map(|a| pb.iter().find(|b| sig(b) == sig(a)).map(|b| (a, b)))
            .collect();
        return (v, true);
    }
    let mut v = Vec::new();
    for a in pa {
        for b in pb {
            v.push((a, b));
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
}

struct Collector<'x, 'a> {
    sa: SideIr<'x, 'a>,
    sb: SideIr<'x, 'a>,
    env: Env,
    splits: Vec<Split>,
    out: Behaviour,
    seen: HashSet<(String, String)>,
    contract_seen: HashSet<&'static str>,
}

impl<'x, 'a> Collector<'x, 'a> {
    fn new(a: &'x Analyzed<'a>, b: &'x Analyzed<'a>, env: Env) -> Self {
        let mut c = Collector {
            sa: SideIr { a, cfg: lower(a) },
            sb: SideIr {
                a: b,
                cfg: lower(b),
            },
            env,
            splits: Vec::new(),
            out: Behaviour::default(),
            seen: HashSet::new(),
            contract_seen: HashSet::new(),
        };
        c.splits = splits(&c.sa, &c.sb, true);
        c.splits.extend(splits(&c.sb, &c.sa, false));
        c
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

    fn run(
        &mut self,
        sc: Kind,
        scen_a: Scenario,
        scen_b: Scenario,
    ) -> (Vec<Outcome>, Vec<Outcome>) {
        let mut pa = explore(&self.sa.cfg, &scen_a);
        let mut pb = explore(&self.sb.cfg, &scen_b);
        if let Kind::Fault { key, .. } = &sc {
            // Only paths that reach the failing statement belong to the scenario.
            let reached = |o: &Outcome| o.fault_hit || o.effects.contains_key(key);
            pa.retain(reached);
            pb.retain(reached);
            if pa.is_empty() || pb.is_empty() {
                return (pa, pb);
            }
        }
        let (ps, exact) = pairs(&pa, &pb);
        let combos = ps.len();
        let mut found: Vec<(Div, &Outcome, &Outcome, usize)> = Vec::new();
        for (oa, ob) in &ps {
            for d in diff(&self.sa, &self.sb, &self.splits, &sc, oa, ob) {
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
            self.emit(&sc, d, oa, ob, exact || hits == combos);
        }
        (pa, pb)
    }

    fn emit(&mut self, sc: &Kind, d: Div, oa: &Outcome, ob: &Outcome, all_agree: bool) {
        let key = (d.rule.to_string(), d.anchor.clone());
        // The error contract and the caller's continuation are properties of
        // the routine as a whole: the first failing statement shows them.
        if matches!(d.rule, "error.contract" | "txn.caller-continuation")
            && !self.contract_seen.insert(d.rule)
        {
            return;
        }
        if !self.seen.insert(key) {
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
        let mut ids: Vec<String> = d.assumptions.clone();
        for x in oa.assumptions.iter().chain(&ob.assumptions) {
            if !ids.contains(x) {
                ids.push(x.clone());
            }
        }
        if matches!(sc, Kind::CallerTxn) {
            ids.insert(0, "ASM-NO-CALLER-TXN".to_string());
            ids.dedup();
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
            _ => {
                c.sa.effect(&d.anchor)
                    .and_then(|(n, _)| c.sa.syntax(n).cloned())
            }
        };
        let node_b = |c: &Self| match sc {
            Kind::Fault { key, nb, .. } if *key == anchor_b => c.sb.syntax(*nb).cloned(),
            _ => {
                c.sb.effect(&anchor_b)
                    .and_then(|(n, _)| c.sb.syntax(n).cloned())
            }
        };
        let (la, lb) = (node_a(self), node_b(self));
        let anchor_text = |n: &Option<SyntaxNode>| {
            n.as_ref()
                .map(|n| ast::first_words(n, 3).join(" "))
                .unwrap_or_default()
        };
        let fp = fingerprint(&FingerprintInput {
            object: &self.sa.a.object,
            class: d.class,
            rule: d.rule,
            anchor_a: &format!("{}|{}", d.anchor, anchor_text(&la)),
            anchor_b: &format!("{}|{}", anchor_b, anchor_text(&lb)),
        });
        self.out.witnesses.push(Witness {
            fingerprint: fp.clone(),
            rule: d.rule.to_string(),
            scenario: scenario_info(&self.sa, &self.sb, sc, &self.env, oa),
            a: side_outcome(&self.sa, oa),
            b: side_outcome(&self.sb, ob),
        });
        self.out.findings.push(Finding {
            fingerprint: fp,
            rule: d.rule.to_string(),
            class: d.class,
            severity: d.severity,
            verdict,
            witness: text,
            a: la.as_ref().map(|n| self.sa.a.node_location(n)),
            b: lb.as_ref().map(|n| self.sb.a.node_location(n)),
            assumptions: ids,
        });
    }

    /// Faults at every write both sides perform, in document order of side A.
    fn faults(&mut self) {
        let keys: Vec<String> = self
            .sa
            .cfg
            .effect_nodes()
            .into_iter()
            .map(|(_, e)| e.key.clone())
            .filter(|k| self.sb.cfg.node_of_effect(k).is_some())
            .collect();
        for key in keys {
            let na = self.sa.cfg.node_of_effect(&key).expect("effect node");
            let nb = self.sb.cfg.node_of_effect(&key).expect("effect node");
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
            self.emit(&Kind::Baseline, d, &o, &o, true);
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
        self.syntax(n).map(|s| self.a.line_of(s))
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

/// [`compare`] under an environment, with structured witnesses.
pub fn analyze(a: &Analyzed, b: &Analyzed, env: &Env) -> Behaviour {
    let mut c = Collector::new(a, b, env.clone());
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
    c.out
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
    let mut c = Collector::new(a, b, env.clone());
    let on_a = side == crate::model::Side::A;
    let (this, other) = if on_a { (&c.sa, &c.sb) } else { (&c.sb, &c.sa) };
    let mut notes = Vec::new();
    let empty = |notes: Vec<String>, text: String| WhatIf {
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
        notes,
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
