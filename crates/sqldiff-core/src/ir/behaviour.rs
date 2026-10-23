//! Behavioural comparison of two artifacts on the semantic IR: both sides
//! are lowered and run under the same scenarios (no fault; each aligned
//! write failing with each error kind it can raise; the caller holding an
//! open transaction), and the outcomes are compared. This replaces the
//! summary-level checker that used to live in `facts.rs` and keeps its rule
//! ids: `txn.atomicity`, `txn.statement-atomicity`, `txn.caller-transaction`
//! and `error.contract`, plus `error.control`, `txn.temp-state` and
//! `txn.durability`.
//!
//! Paired exploration with statement alignment and full witnesses is WP9;
//! here effects are aligned by key (kind, folded target, occurrence).

use std::collections::{BTreeSet, HashSet};

use sqldiff_syntax::{ast, SyntaxNode};

use super::cfg::{lower, Cfg, Effect, NodeId};
use super::interp::{explore, Exit, Fate, Fault, Outcome, Scenario, TempState};
use super::profile::ErrorKind;
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

#[derive(Debug, Clone)]
enum Kind {
    Fault { key: String, kind: ErrorKind },
    Baseline,
    CallerTxn,
}

struct Div {
    rule: &'static str,
    class: ChangeClass,
    severity: Severity,
    anchor: String,
    text: String,
    assumptions: Vec<String>,
    /// Depends on the fate of effects (capped by `durability_unknown`).
    durability: bool,
}

/// Differences between one outcome of each side under one scenario.
fn diff(sa: &SideIr, sb: &SideIr, sc: &Kind, oa: &Outcome, ob: &Outcome) -> Vec<Div> {
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
            out.push(Div {
                rule: "txn.statement-atomicity",
                class: ChangeClass::Transactional,
                severity: Severity::High,
                anchor: fk.to_string(),
                text: format!(
                    "{} rolls the statement back; {} keeps the rows it changed before the error, because statements are atomic only under commitment control",
                    q.name(),
                    p.name()
                ),
                assumptions: vec!["ASM-MULTI-ROW".into()],
                durability: true,
            });
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
        let rule = scenario_rule(*before);
        out.push(Div {
            rule,
            class: ChangeClass::Transactional,
            severity: Severity::Critical,
            anchor: k.clone(),
            text: format!(
                "{} {}; {} {}",
                sa.name(),
                fate_phrase(sa, k, *fa),
                sb.name(),
                fate_phrase(sb, k, *fb)
            ),
            assumptions: asm,
            durability: true,
        });
    }
    // 3. Effects that run on one side only (after the fault).
    if fault_key.is_some() {
        if let Some((k, fa, fb)) = control_diffs.first() {
            let persisted = matches!(fa, Some(Fate::Durable | Fate::Partial))
                || matches!(fb, Some(Fate::Durable | Fate::Partial));
            out.push(Div {
                rule: "error.control",
                class: if persisted {
                    ChangeClass::Transactional
                } else {
                    ChangeClass::Semantic
                },
                severity: Severity::High,
                anchor: k.clone(),
                text: format!(
                    "{} {}; {} {}",
                    sa.name(),
                    fate_phrase(sa, k, *fa),
                    sb.name(),
                    fate_phrase(sb, k, *fb)
                ),
                assumptions: Vec::new(),
                durability: false,
            });
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
        out.push(Div {
            rule,
            class: if rule == "error.contract" {
                ChangeClass::Semantic
            } else {
                ChangeClass::Transactional
            },
            severity: Severity::High,
            anchor: fault_key.unwrap_or("exit").to_string(),
            text: format!(
                "{} {}; {} {}",
                sa.name(),
                exit_phrase(oa),
                sb.name(),
                exit_phrase(ob)
            ),
            assumptions: asm,
            durability: false,
        });
    }
    // 5. Temporary state seen by aligned statements.
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
            out.push(Div {
                rule: "txn.temp-state",
                class: ChangeClass::Transactional,
                severity: Severity::High,
                anchor: k.clone(),
                text: format!(
                    "{} reads the temporary table {}, which on {} {} and on {} {}",
                    sa.phrase(k),
                    t.to_lowercase(),
                    sa.name(),
                    state(&ra),
                    sb.name(),
                    state(&rb)
                ),
                assumptions: Vec::new(),
                durability: false,
            });
        }
    }
    out
}

fn scenario_intro(sa: &SideIr, sb: &SideIr, sc: &Kind, oa: &Outcome) -> String {
    let path = if oa.signature.is_empty() {
        String::new()
    } else {
        let conds: Vec<String> = oa
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
    };
    match sc {
        Kind::Fault { key, kind } => {
            let la = sa.effect(key).map(|(n, _)| sa.line(n)).unwrap_or_default();
            let lb = sb.effect(key).map(|(n, _)| sb.line(n)).unwrap_or_default();
            format!(
                "If {} fails with {} ({la}; {lb}){path}",
                sa.phrase(key),
                kind.phrase()
            )
        }
        Kind::Baseline if path.is_empty() => "When every statement succeeds".to_string(),
        Kind::Baseline => format!("When no statement fails{path}"),
        Kind::CallerTxn => format!("If the caller already holds an open transaction{path}"),
    }
}

/// Pairs the outcomes of both sides: by path signature when both sides
/// branch on the same conditions, otherwise every combination (and the
/// verdict is capped at unknown unless all combinations agree).
fn pairs<'o>(pa: &'o [Outcome], pb: &'o [Outcome]) -> (Vec<(&'o Outcome, &'o Outcome)>, bool) {
    if pa.len() == 1 && pb.len() == 1 {
        return (vec![(&pa[0], &pb[0])], true);
    }
    let sig = |o: &Outcome| o.signature.clone();
    let sb: HashSet<_> = pb.iter().map(sig).collect();
    let sa: HashSet<_> = pa.iter().map(sig).collect();
    if sa == sb {
        let v = pa
            .iter()
            .filter_map(|a| {
                pb.iter()
                    .find(|b| b.signature == a.signature)
                    .map(|b| (a, b))
            })
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

struct Collector<'x, 'a> {
    sa: SideIr<'x, 'a>,
    sb: SideIr<'x, 'a>,
    out: Vec<Finding>,
    assumptions: Vec<Assumption>,
    seen: HashSet<(String, String)>,
    contract_seen: bool,
}

impl Collector<'_, '_> {
    fn outcomes(
        &self,
        sc: &Kind,
        scen_a: &Scenario,
        scen_b: &Scenario,
    ) -> (Vec<Outcome>, Vec<Outcome>) {
        let mut pa = explore(&self.sa.cfg, scen_a);
        let mut pb = explore(&self.sb.cfg, scen_b);
        if let Kind::Fault { key, .. } = sc {
            // Only paths that reach the failing statement belong to the scenario.
            let reached = |o: &Outcome| o.fault_hit || o.effects.contains_key(key);
            pa.retain(reached);
            pb.retain(reached);
        }
        (pa, pb)
    }

    /// Whether a divergence still shows with a default setting flipped; if
    /// so, the finding does not rest on the assumption behind that default.
    fn persists(&self, sc: &Kind, scen_a: &Scenario, scen_b: &Scenario, d: &Div) -> bool {
        let (pa, pb) = self.outcomes(sc, scen_a, scen_b);
        let (ps, _) = pairs(&pa, &pb);
        ps.iter().any(|(oa, ob)| {
            diff(&self.sa, &self.sb, sc, oa, ob)
                .iter()
                .any(|x| x.rule == d.rule && x.anchor == d.anchor)
        })
    }

    fn run(&mut self, sc: Kind, scen_a: Scenario, scen_b: Scenario) {
        let (pa, pb) = self.outcomes(&sc, &scen_a, &scen_b);
        if pa.is_empty() || pb.is_empty() {
            return;
        }
        let (ps, exact) = pairs(&pa, &pb);
        let combos = ps.len();
        let mut found: Vec<(Div, &Outcome, &Outcome, usize)> = Vec::new();
        for (oa, ob) in &ps {
            for d in diff(&self.sa, &self.sb, &sc, oa, ob) {
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
            let key = (d.rule.to_string(), d.anchor.clone());
            if d.rule == "error.contract" {
                if self.contract_seen {
                    continue;
                }
                self.contract_seen = true;
            }
            if !self.seen.insert(key) {
                continue;
            }
            let all_agree = exact || hits == combos;
            let imprecise = !oa.imprecise.is_empty() || !ob.imprecise.is_empty();
            let durability_unknown = d.durability
                && (oa.durability_unknown.is_some() || ob.durability_unknown.is_some());
            // A caller-transaction divergence is what flipping the default
            // assumption ASM-NO-CALLER-TXN would change: under the assumption
            // both sides agree, so it is reported but does not make the pair
            // different.
            let verdict = if imprecise || durability_unknown {
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
            } else {
                for id in ["ASM-NO-CALLER-TXN", "ASM-TSQL-CALLER-XACT-ABORT-OFF"] {
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
                    if self.persists(&sc, &fa, &fb, &d) {
                        ids.retain(|x| x != id);
                    }
                }
            }
            let mut text = format!(
                "{}, {}.",
                scenario_intro(&self.sa, &self.sb, &sc, oa),
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
                if !self.assumptions.iter().any(|y| &y.id == id) {
                    self.assumptions.push(assumption(id));
                }
            }
            let la = self
                .sa
                .effect(&d.anchor)
                .and_then(|(n, _)| self.sa.syntax(n).cloned());
            let lb = self
                .sb
                .effect(&d.anchor)
                .and_then(|(n, _)| self.sb.syntax(n).cloned());
            let anchor_text = |n: &Option<SyntaxNode>| {
                n.as_ref()
                    .map(|n| ast::first_words(n, 3).join(" "))
                    .unwrap_or_default()
            };
            let class = d.class;
            // Same anchors as the syntactic checks had, so stored review
            // decisions keep matching; the effect key only splits a collision.
            let print = |key: &str| {
                fingerprint(&FingerprintInput {
                    object: &self.sa.a.object,
                    class,
                    rule: d.rule,
                    anchor_a: &format!("{key}{}", anchor_text(&la)),
                    anchor_b: &format!("{key}{}", anchor_text(&lb)),
                })
            };
            let mut fp = print("");
            if self.out.iter().any(|f| f.fingerprint == fp) {
                fp = print(&format!("{}|", d.anchor));
            }
            self.out.push(Finding {
                fingerprint: fp,
                rule: d.rule.to_string(),
                class,
                severity: d.severity,
                verdict,
                witness: text,
                a: la.as_ref().map(|n| self.sa.a.node_location(n)),
                b: lb.as_ref().map(|n| self.sb.a.node_location(n)),
                assumptions: ids,
            });
        }
    }
}

/// Behavioural findings from running both sides' IR under the same scenarios.
pub fn compare(a: &Analyzed, b: &Analyzed) -> (Vec<Finding>, Vec<Assumption>) {
    let mut c = Collector {
        sa: SideIr { a, cfg: lower(a) },
        sb: SideIr {
            a: b,
            cfg: lower(b),
        },
        out: Vec::new(),
        assumptions: Vec::new(),
        seen: HashSet::new(),
        contract_seen: false,
    };
    // Faults at every write both sides perform, in document order of side A.
    let keys: Vec<String> =
        c.sa.cfg
            .effect_nodes()
            .into_iter()
            .map(|(_, e)| e.key.clone())
            .filter(|k| c.sb.cfg.node_of_effect(k).is_some())
            .collect();
    for key in keys {
        let na = c.sa.cfg.node_of_effect(&key).expect("effect node");
        let nb = c.sb.cfg.node_of_effect(&key).expect("effect node");
        let ra =
            c.sa.cfg
                .node(na)
                .stmt()
                .map(|s| s.raises.clone())
                .unwrap_or_default();
        let rb =
            c.sb.cfg
                .node(nb)
                .stmt()
                .map(|s| s.raises.clone())
                .unwrap_or_default();
        for kind in ErrorKind::ALL {
            if !ra.contains(&kind) && !rb.contains(&kind) {
                continue;
            }
            let fault = |n: NodeId, r: &[ErrorKind]| Scenario {
                faults: if r.contains(&kind) {
                    vec![Fault { node: n, kind }]
                } else {
                    Vec::new()
                },
                ..Scenario::default()
            };
            c.run(
                Kind::Fault {
                    key: key.clone(),
                    kind,
                },
                fault(na, &ra),
                fault(nb, &rb),
            );
        }
    }
    c.run(Kind::Baseline, Scenario::default(), Scenario::default());
    let caller = Scenario {
        caller_txn: true,
        ..Scenario::default()
    };
    c.run(Kind::CallerTxn, caller.clone(), caller);
    (c.out, c.assumptions)
}
