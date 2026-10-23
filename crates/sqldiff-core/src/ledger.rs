//! Assumption ledger (design §9): where each assumption's status comes
//! from, which ones decide the verdict, and contradictions between sources.
//!
//! Sources, strongest first: a session flip (a reviewer's what-if), the
//! object's own source (`SET XACT_ABORT ON`, `SET OPTION COMMIT`), composed
//! output, the catalog, an author's declaration, pack or project config, and
//! the tool's default. Flipping a scenario assumption re-runs the
//! behavioural analysis (`ir::behaviour::RERUN_ASSUMPTIONS`); flipping any
//! other assumption relabels the findings that rely on it. Two sources that
//! disagree on the same fact produce an `assumption.contradiction` finding.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::artifact::{Analyzed, Artifact};
use crate::ir::behaviour::RERUN_ASSUMPTIONS;
use crate::ir::cfg::{lower, Action};
use crate::model::{
    fingerprint, AssumptionSource, ChangeClass, Finding, FingerprintInput, Report, Severity, Side,
    Verdict,
};
use crate::{engine, facts, Options};

/// Session-local assumption flips: id → holds.
pub type Flips = BTreeMap<String, bool>;

/// Where an assumption's status comes from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Session,
    /// Proven from the object's own source.
    Source,
    /// Proven from composed output.
    Composition,
    /// Proven from the catalog.
    Catalog,
    /// Declared by the author in source.
    Author,
    /// Declared in a pack or project config.
    Config,
    /// A tool default no source states.
    Default,
}

/// A fact the object's own source states, so the matching default is not
/// an assumption for that side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discharge {
    pub id: String,
    pub side: Side,
    pub line: usize,
    /// The statement that states it, e.g. `SET XACT_ABORT ON`.
    pub cite: String,
    /// The stated value, where the fact has one (a commit level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Byte range of the statement.
    #[serde(skip)]
    pub range: (usize, usize),
}

/// Facts one side's source states about its own environment.
pub fn discharges(a: &Analyzed) -> Vec<Discharge> {
    let mut out = Vec::new();
    let cfg = lower(a);
    for n in &cfg.nodes {
        let Some(s) = n.stmt() else { continue };
        if s.action == Action::XactAbort(true) {
            if let Some(syn) = &n.syntax {
                out.push(Discharge {
                    id: "ASM-TSQL-CALLER-XACT-ABORT-OFF".into(),
                    side: a.side,
                    line: a.line_of(syn),
                    cite: "SET XACT_ABORT ON".into(),
                    value: None,
                    range: sqldiff_syntax::ast::sig_range(syn),
                });
                break;
            }
        }
    }
    if let Some((level, offset)) = facts::find_commit_level(a) {
        let line = a.location(offset, offset).start_line;
        out.push(Discharge {
            id: "ASM-DB2-COMMIT-LEVEL".into(),
            side: a.side,
            line,
            cite: format!("SET OPTION COMMIT = {level}"),
            value: Some(level),
            range: (offset, offset + "COMMIT".len()),
        });
    }
    out
}

/// The Db2 for i commit level a declaration's text names (`*NONE`, `*CHG`, ...).
pub fn declared_commit_level(text: &str) -> Option<String> {
    let up = text.to_ascii_uppercase();
    ["*NONE", "*CHG", "*CS", "*ALL", "*RR", "*NC", "*UR", "*RS"]
        .into_iter()
        .filter_map(|l| {
            up.find(l)
                .filter(|i| {
                    // Whole word: `*CS` must not match inside `*CSX`.
                    !up[i + l.len()..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                })
                .map(|i| (i, l))
        })
        .min()
        .map(|(_, l)| l.to_string())
}

/// The canonical name of a Db2 for i commit level: `*UR` is `*CHG`, `*RS`
/// is `*ALL`, `*NC` is `*NONE` (`*CS` and `*RR` have no synonym in use).
pub fn canonical_commit_level(level: &str) -> String {
    let up = level.trim().to_ascii_uppercase();
    match up.as_str() {
        "*UR" => "*CHG".into(),
        "*RS" => "*ALL".into(),
        "*NC" => "*NONE".into(),
        _ => up,
    }
}

/// Findings for declarations that contradict what a side's source states.
pub fn contradictions(sides: &[&Analyzed], opts: &Options) -> Vec<Finding> {
    let mut out = Vec::new();
    for a in sides {
        for d in discharges(a) {
            let Some(text) = opts.declared.get(&d.id) else {
                continue;
            };
            let contradicts = match (&d.value, declared_commit_level(text)) {
                (Some(v), Some(w)) => canonical_commit_level(v) != canonical_commit_level(&w),
                _ => false,
            };
            if !contradicts {
                continue;
            }
            let loc = a.location(d.range.0, d.range.1);
            out.push(Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class: ChangeClass::Transactional,
                    rule: "assumption.contradiction",
                    // One finding per side: the side is part of the anchor.
                    anchor_a: &format!("{}|{:?}", d.id, d.side),
                    anchor_b: text,
                }),
                rule: "assumption.contradiction".into(),
                class: ChangeClass::Transactional,
                severity: Severity::High,
                verdict: Verdict::Unknown,
                witness: format!(
                    "Configuration declares {} as \"{text}\", but side {:?} line {} says {}; the routine's own setting is used, so the declaration is wrong for this object or the object is not created from this source.",
                    d.id, d.side, d.line, d.cite
                ),
                a: (a.side == Side::A).then(|| loc.clone()),
                b: (a.side == Side::B).then_some(loc),
                assumptions: vec![d.id.clone()],
            });
        }
    }
    out
}

pub use crate::session::derive_verdict;

/// Applies flips that do not change scenarios: a denied assumption drops
/// every finding that relies on it to unknown (its witness was only shown
/// under it); a confirmed one shows as declared. Without a denial that
/// hits, the report and its verdict are left as they are; with one, the
/// verdict is derived again, never better than "equal under assumptions"
/// when review accepted findings.
pub fn relabel(r: &mut Report, flips: &Flips) {
    let ids = |holds: bool| -> Vec<String> {
        flips
            .iter()
            .filter(|(_, h)| **h == holds)
            .map(|(k, _)| k.clone())
            .collect()
    };
    crate::session::confirm_assumptions(r, &ids(true));
    let before = r.verdict;
    crate::session::deny_assumptions(r, &ids(false));
    if r.verdict != before && !r.suppressed.is_empty() {
        r.verdict = r.open_verdict();
    }
}

/// Diff under flips: scenario flips re-run the analysis, the rest relabel.
pub fn evaluate(a: &Artifact, b: &Artifact, opts: &Options) -> Report {
    let mut r = engine::diff(a, b, opts);
    relabel(&mut r, &opts.flips);
    r
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Declared,
    Confirmed,
    Denied,
    /// Stated by the object's own source: not an assumption.
    Discharged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub text: String,
    pub status: Status,
    pub source: Source,
    /// Config line, source statement or session flip behind the status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cite: Option<String>,
    /// Findings that rely on it.
    pub findings: Vec<String>,
    /// The pair's verdict if this assumption alone flipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_if_flipped: Option<Verdict>,
    pub flips_verdict: bool,
    /// Flipping it re-runs the behavioural analysis.
    pub reruns: bool,
}

/// Only the flips that re-run the behavioural analysis.
pub fn scenario_flips(flips: &Flips) -> Flips {
    flips
        .iter()
        .filter(|(k, _)| RERUN_ASSUMPTIONS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

/// The pair's report under `opts.flips` and its ledger: verdict-deciding
/// assumptions first, then open ones, then by source.
pub fn ledger(a: &Artifact, b: &Artifact, opts: &Options) -> (Report, Vec<Entry>) {
    ledger_by(&opts.flips, &opts.declared, &mut |f| {
        engine::diff(
            a,
            b,
            &Options {
                flips: f.clone(),
                ..opts.clone()
            },
        )
    })
}

/// The ledger over a caller's own pipeline: `run` diffs the pair under the
/// given scenario flips with everything the caller shows applied (pack
/// policy, markers, review suppression), so "verdict if flipped" compares
/// against the verdict the caller displays. Only scenario flips re-run; the
/// others relabel a copy of that report.
pub fn ledger_by(
    flips: &Flips,
    declared: &BTreeMap<String, String>,
    run: &mut dyn FnMut(&Flips) -> Report,
) -> (Report, Vec<Entry>) {
    let scen = scenario_flips(flips);
    let raw = run(&scen);
    let mut report = raw.clone();
    relabel(&mut report, flips);
    let mut ids: Vec<String> = report.assumptions.iter().map(|x| x.id.clone()).collect();
    for id in RERUN_ASSUMPTIONS {
        if flips.contains_key(id) && !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }
    let mut out = Vec::new();
    for id in ids {
        let asm = report.assumptions.iter().find(|x| x.id == id);
        let flipped = flips.get(&id).copied();
        let (status, source, cite) = match (flipped, asm.map(|x| x.source)) {
            (Some(true), _) => (
                Status::Confirmed,
                Source::Session,
                Some("confirmed in this session".into()),
            ),
            (Some(false), _) => (
                Status::Denied,
                Source::Session,
                Some("denied in this session".into()),
            ),
            (None, Some(AssumptionSource::ComposedOutput)) => (
                Status::Discharged,
                Source::Composition,
                Some("proven from composed output".into()),
            ),
            (None, _) if declared.contains_key(&id) => (
                Status::Declared,
                Source::Config,
                declared.get(&id).map(|t| format!("declared: {t}")),
            ),
            (None, Some(AssumptionSource::Declared)) => (Status::Declared, Source::Config, None),
            (None, Some(AssumptionSource::Author)) => (
                Status::Open,
                Source::Author,
                Some("stated by the author in source".into()),
            ),
            _ => (Status::Open, Source::Default, None),
        };
        let mut other = flips.clone();
        // Flipping a denied one restores it; anything else is denied.
        other.insert(id.clone(), status == Status::Denied);
        let reruns = RERUN_ASSUMPTIONS.contains(&id.as_str());
        let mut r = if reruns {
            run(&scenario_flips(&other))
        } else {
            raw.clone()
        };
        relabel(&mut r, &other);
        let v = r.verdict;
        out.push(Entry {
            text: asm
                .map(|x| x.text.clone())
                .unwrap_or_else(|| crate::ir::behaviour::assumption(&id).text),
            findings: report
                .findings
                .iter()
                .filter(|f| f.assumptions.contains(&id))
                .map(|f| f.fingerprint.clone())
                .collect(),
            reruns,
            verdict_if_flipped: Some(v),
            flips_verdict: v != report.verdict,
            id,
            status,
            source,
            cite,
        });
    }
    for d in &report.discharged {
        out.push(Entry {
            id: d.id.clone(),
            text: crate::ir::behaviour::assumption(&d.id).text,
            status: Status::Discharged,
            source: Source::Source,
            cite: Some(format!("side {:?} line {}: {}", d.side, d.line, d.cite)),
            findings: Vec::new(),
            verdict_if_flipped: None,
            flips_verdict: false,
            reruns: false,
        });
    }
    out.sort_by_key(|e| {
        (
            !e.flips_verdict,
            e.status != Status::Open,
            e.source,
            e.id.clone(),
        )
    });
    (report, out)
}
