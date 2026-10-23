//! Entry points: diff two artifacts, or check one.

use std::collections::{BTreeMap, HashSet};

use crate::artifact::{Analyzed, Artifact};
use crate::classify::pairing_key;
use crate::equiv;
use crate::facts;
use crate::lint;
use crate::model::{
    fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, NoiseCounts, Report, Severity,
    Side, Verdict,
};
use crate::normalize::lcs_pairs;
use crate::treediff;

#[derive(Debug, Clone)]
pub struct Options {
    /// Maximum lines of excerpt per location.
    pub excerpt_lines: usize,
    /// DDL artifacts (CREATE TABLE / CREATE UNIQUE INDEX) describing the
    /// catalog side A runs against; used to discharge key, nullability and
    /// type side conditions of statement equivalence.
    pub catalog_a: Vec<Artifact>,
    /// The same for side B.
    pub catalog_b: Vec<Artifact>,
    /// Session assumption flips (id → holds). Flips of scenario assumptions
    /// (`ir::behaviour::RERUN_ASSUMPTIONS`) re-run the behavioural analysis
    /// under the flipped environment; the rest are applied to verdicts by
    /// the caller (see `sqldiff_source::queue::apply`).
    pub flips: BTreeMap<String, bool>,
    /// Facts declared in packs or project config, by assumption id (the
    /// declaration's text). A declared Db2 for i commit level is used for
    /// routines that do not state one.
    pub declared: BTreeMap<String, String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            excerpt_lines: 3,
            catalog_a: Vec::new(),
            catalog_b: Vec::new(),
            flips: BTreeMap::new(),
            declared: BTreeMap::new(),
        }
    }
}

pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compare two artifacts. Same dialect: version review (UC2). Different
/// dialects: drift and conversion review (UC1, UC3).
pub fn diff(a: &Artifact, b: &Artifact, opts: &Options) -> Report {
    let aa = Analyzed::new(a, Side::A, opts.excerpt_lines);
    let ab = Analyzed::new(b, Side::B, opts.excerpt_lines);
    let same = a.dialect == b.dialect;

    let mut findings = Vec::new();
    let mut assumptions: Vec<Assumption> = Vec::new();
    let mut normalizations = vec![
        "whitespace, line breaks and comments".to_string(),
        "keyword case".to_string(),
        "statement terminators".to_string(),
    ];
    let mut unknown_reasons = Vec::new();
    let mut noise = NoiseCounts::default();
    let schema_a = equiv::schema::Schema::load(&opts.catalog_a);
    let schema_b = equiv::schema::Schema::load(&opts.catalog_b);
    let mut eq = equiv::Outcome::default();

    if same {
        let mut td = treediff::diff(&aa, &ab);
        equiv::refine_modified(&aa, &ab, &schema_a, &schema_b, &mut td.findings, &mut eq);
        findings.extend(td.findings);
        noise.cosmetic = td.cosmetic;
        noise.structural = td.structural + eq.structural;
        match a.dialect.lexical().folding {
            sqldiff_syntax::IdentFolding::Lower => {
                normalizations.push("unquoted identifiers folded to lower case".to_string())
            }
            sqldiff_syntax::IdentFolding::Upper => {
                normalizations.push("unquoted identifiers folded to upper case".to_string())
            }
            sqldiff_syntax::IdentFolding::Collation => {
                normalizations.push("identifier quoting ([x], \"x\", x)".to_string());
                if td.used_case_insensitive_idents {
                    normalizations.push("identifier and variable case".to_string());
                    assumptions.push(Assumption::default(
                        "ASM-TSQL-CI-COLLATION",
                        "Identifiers and variables compare case-insensitively (case-insensitive catalog collation)",
                    ));
                }
            }
        }
        // Lints that are new in B.
        let old: HashSet<String> = lint::all_hits(&aa)
            .into_iter()
            .map(|h| format!("{}|{}", h.rule, h.key))
            .collect();
        for h in lint::all_hits(&ab) {
            if !old.contains(&format!("{}|{}", h.rule, h.key)) {
                findings.push(lint::to_finding(&ab, &h));
            }
        }
    } else {
        findings.extend(pair_writes(&aa, &ab));
        for side in [&aa, &ab] {
            for h in lint::all_hits(side) {
                findings.push(lint::to_finding(side, &h));
            }
        }
        eq = equiv::compare(&aa, &ab, &schema_a, &schema_b);
        findings.append(&mut eq.findings);
        noise.structural += eq.structural;
        unknown_reasons.append(&mut eq.unknown_reasons);
        normalizations.push(
            "statement lowering: parameters by position, aliases resolved, dialect built-ins and idioms mapped to one form".to_string(),
        );
    }
    for x in eq.normalizations.drain(..) {
        if !normalizations.contains(&x) {
            normalizations.push(x);
        }
    }
    for x in eq.assumptions.drain(..) {
        if !assumptions.iter().any(|y| y.id == x.id) {
            assumptions.push(x);
        }
    }

    let env = crate::ir::behaviour::Env::from_options(opts);
    let beh = crate::ir::behaviour::analyze(&aa, &ab, &env);
    let witnesses = beh.witnesses;
    findings.extend(beh.findings);
    for x in beh.assumptions {
        if !assumptions.iter().any(|y| y.id == x.id) {
            assumptions.push(x);
        }
    }

    findings.extend(crate::ledger::contradictions(&[&aa, &ab], opts));
    let mut discharged = crate::ledger::discharges(&aa);
    discharged.extend(crate::ledger::discharges(&ab));

    let mut diagnostics = aa.diagnostics();
    diagnostics.extend(ab.diagnostics());
    if !diagnostics.is_empty() {
        unknown_reasons.push(format!(
            "{} parse problem(s); affected statements may be misaligned",
            diagnostics.len()
        ));
    }

    finish(Report {
        tool_version: TOOL_VERSION.to_string(),
        verdict: Verdict::Equal,
        a: aa.info(),
        b: Some(ab.info()),
        findings,
        noise,
        normalizations,
        assumptions,
        unknown_reasons,
        diagnostics,
        witnesses,
        discharged,
    })
}

/// "What if this statement fails?" (design §6.1 `what_if`): the statement
/// on or before `line` of side `side`, failing with `kind`, under the
/// environment `opts.flips` and `opts.declared` describe.
pub fn what_if(
    a: &Artifact,
    b: &Artifact,
    opts: &Options,
    side: Side,
    line: usize,
    kind: crate::ir::ErrorKind,
) -> crate::ir::witness::WhatIf {
    let aa = Analyzed::new(a, Side::A, opts.excerpt_lines);
    let ab = Analyzed::new(b, Side::B, opts.excerpt_lines);
    let env = crate::ir::behaviour::Env::from_options(opts);
    crate::ir::behaviour::what_if(&aa, &ab, side, line, kind, &env)
}

/// Transaction overlay brackets of both sides (design §6.2).
pub fn overlay(a: &Artifact, b: &Artifact) -> crate::ir::overlay::Overlay {
    let aa = Analyzed::new(a, Side::A, 1);
    let ab = Analyzed::new(b, Side::B, 1);
    crate::ir::overlay::overlay(&aa, &ab)
}

/// Lint one artifact on its own.
pub fn check(a: &Artifact, opts: &Options) -> Report {
    let aa = Analyzed::new(a, Side::A, opts.excerpt_lines);
    let findings = lint::all_hits(&aa)
        .iter()
        .map(|h| lint::to_finding(&aa, h))
        .collect();
    let diagnostics = aa.diagnostics();
    let mut unknown_reasons = Vec::new();
    if !diagnostics.is_empty() {
        unknown_reasons.push(format!("{} parse problem(s)", diagnostics.len()));
    }
    finish(Report {
        tool_version: TOOL_VERSION.to_string(),
        verdict: Verdict::Equal,
        a: aa.info(),
        b: None,
        findings,
        noise: NoiseCounts::default(),
        normalizations: Vec::new(),
        assumptions: Vec::new(),
        unknown_reasons,
        diagnostics,
        witnesses: Vec::new(),
        discharged: Vec::new(),
    })
}

fn finish(mut r: Report) -> Report {
    // Same fingerprint twice (e.g. the same residue on two lines): keep both, they have different locations.
    r.findings.sort_by(|x, y| {
        y.severity
            .cmp(&x.severity)
            .then(y.verdict.cmp(&x.verdict))
            .then(first_line(x).cmp(&first_line(y)))
    });
    r.verdict = if r.findings.iter().any(|f| f.verdict == Verdict::Different) {
        Verdict::Different
    } else if r.findings.iter().any(|f| f.verdict == Verdict::Unknown)
        || !r.unknown_reasons.is_empty()
    {
        Verdict::Unknown
    } else if !r.assumptions.is_empty() {
        Verdict::EqualUnderAssumptions
    } else {
        Verdict::Equal
    };
    r
}

fn first_line(f: &Finding) -> (usize, usize) {
    (
        f.a.as_ref().map(|l| l.start_line).unwrap_or(usize::MAX),
        f.b.as_ref().map(|l| l.start_line).unwrap_or(usize::MAX),
    )
}

/// Cross-dialect: every durable write on one side should have a counterpart
/// (same kind, same target object after case and `_` folding) on the other.
fn pair_writes(a: &Analyzed, b: &Analyzed) -> Vec<Finding> {
    let wa = facts::extract(a).writes;
    let wb = facts::extract(b).writes;
    // MERGE and INSERT ... ON CONFLICT are both upserts.
    let key = |w: &facts::Write| {
        let kind = if w.upsert {
            "Upsert".to_string()
        } else {
            format!("{:?}", w.kind)
        };
        format!("{kind}:{}", pairing_key(std::slice::from_ref(&w.target)))
    };
    let target_of = |k: &String| {
        k.split_once(':')
            .map(|x| x.1.to_string())
            .unwrap_or_default()
    };
    let ka: Vec<String> = wa.iter().map(key).collect();
    let kb: Vec<String> = wb.iter().map(key).collect();
    let pairs = lcs_pairs(&ka, &kb);
    let mut out = Vec::new();
    let matched_a: HashSet<usize> = pairs.iter().map(|p| p.0).collect();
    let matched_b: HashSet<usize> = pairs.iter().map(|p| p.1).collect();
    for (side, other, ws, keys, matched, is_a) in [
        (a, b, &wa, &ka, &matched_a, true),
        (b, a, &wb, &kb, &matched_b, false),
    ] {
        for (i, w) in ws.iter().enumerate() {
            if matched.contains(&i) {
                continue;
            }
            let other_keys = if is_a { &kb } else { &ka };
            let elsewhere = other_keys.contains(&keys[i]);
            let same_target = other_keys
                .iter()
                .any(|k| target_of(k) == target_of(&keys[i]));
            let (verdict, tail) = if elsewhere {
                (
                    Verdict::Unknown,
                    "it appears there in a different order".to_string(),
                )
            } else if same_target {
                (
                    Verdict::Unknown,
                    "the same table is written by a different kind of statement".to_string(),
                )
            } else {
                (
                    Verdict::Different,
                    "it has no corresponding write".to_string(),
                )
            };
            let witness = format!(
                "Side {:?} ({}) runs {} {} (line {}); on side {:?} ({}) {tail}.",
                side.side,
                side.dialect().display_name(),
                crate::treediff::article(crate::classify::StmtKind::label(w.kind)),
                match w.kind {
                    crate::classify::StmtKind::Insert | crate::classify::StmtKind::Merge =>
                        format!("into {}", w.target),
                    crate::classify::StmtKind::Delete => format!("from {}", w.target),
                    _ => format!("of {}", w.target),
                },
                side.line_of(&w.node),
                other.side,
                other.dialect().display_name(),
            );
            let loc = side.node_location(&w.node);
            let class = ChangeClass::Semantic;
            out.push(Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class,
                    rule: "write.unpaired",
                    anchor_a: if is_a { &keys[i] } else { "" },
                    anchor_b: if is_a { "" } else { &keys[i] },
                }),
                rule: "write.unpaired".to_string(),
                class,
                severity: Severity::High,
                verdict,
                witness,
                a: is_a.then(|| loc.clone()),
                b: (!is_a).then_some(loc),
                assumptions: Vec::new(),
            });
        }
    }
    out
}
