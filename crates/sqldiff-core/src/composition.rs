//! Composition of generated code (design section 3, layer 4, and section 9).
//!
//! Generated output is made of parts, each with a fragment, its template,
//! its parameters and its span in the output. Behaviour belongs to the
//! composed output, so the engine analyses that text; this module adds
//! what only the composition can tell:
//!
//! - **Read mode**: split the output into parts from provenance markers
//!   ([`read`], marker syntax from a pack) or from an adapter's persisted
//!   composition (built by the caller), with exact offsets.
//! - **Check mode** ([`check`]): substitute each part's parameters into its
//!   template and compare with the generated part. A mismatch is an input
//!   inconsistency and becomes a finding, never hidden.
//! - **Part-level diff**: a part present on one side only, or built from a
//!   different template or parameters, is reported against the composition.
//! - **Wrapper facts**: session options, transaction and error-handling
//!   facts that come from a part other than the routine body are recorded
//!   as *proven from composed output* in the assumption ledger.
//! - **Staleness**: output older than its inputs caps the verdict at unknown.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use sqldiff_syntax::template::{stand_in, Compiled, Derived, SegmentKind, Template};
use sqldiff_syntax::{ast, SyntaxNode};

use crate::artifact::{content_hash, Analyzed, Artifact};
use crate::classify::{stmt_kind, StmtKind};
use crate::facts;
use crate::model::{
    fingerprint, Assumption, AssumptionSource, ChangeClass, Finding, FingerprintInput, Report,
    Severity, Side, Verdict,
};
use crate::normalize::lcs_pairs;
use crate::Options;

/// One part of a composed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub template: Option<String>,
    pub parameters: BTreeMap<String, String>,
    /// Byte span of the whole part, markers included.
    pub start: usize,
    pub end: usize,
    /// Byte span of the fragment the template produced.
    pub body_start: usize,
    pub body_end: usize,
    /// Content hash of the template the part was generated from, if the
    /// source reports it; a different current template means stale output.
    pub template_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Provenance markers in the text, by marker syntax name.
    Markers(String),
    /// Persisted by the generator and reported by a source adapter.
    Adapter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub offset: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composition {
    pub parts: Vec<Part>,
    pub origin: Origin,
    /// The source flags the output as older than its inputs.
    pub stale: bool,
    /// Unbalanced or nested markers, out-of-range spans.
    pub problems: Vec<Problem>,
}

impl Composition {
    pub fn part_at(&self, offset: usize) -> Option<&Part> {
        self.parts
            .iter()
            .find(|p| offset >= p.start && offset < p.end.max(p.start + 1))
    }

    /// Builds a composition from spans an adapter reported, checking them
    /// against the text.
    pub fn from_spans(text: &str, mut parts: Vec<Part>, stale: bool) -> Composition {
        let mut problems = Vec::new();
        parts.sort_by_key(|p| p.start);
        let mut prev_end = 0;
        parts.retain(|p| {
            let ok = p.start <= p.end
                && p.end <= text.len()
                && text.is_char_boundary(p.start)
                && text.is_char_boundary(p.end);
            if !ok {
                problems.push(Problem {
                    offset: p.start.min(text.len()),
                    message: format!(
                        "part '{}' span {}..{} is outside the text ({} bytes)",
                        p.name,
                        p.start,
                        p.end,
                        text.len()
                    ),
                });
            } else if p.start < prev_end {
                problems.push(Problem {
                    offset: p.start,
                    message: format!("part '{}' overlaps the part before it", p.name),
                });
            }
            if ok {
                prev_end = prev_end.max(p.end);
            }
            ok
        });
        Composition {
            parts,
            origin: Origin::Adapter,
            stale,
            problems,
        }
    }
}

/// Provenance marker syntax: a begin and an end line. Named groups of
/// `begin`: `part` names the part, `template` its template, and any other
/// named group is a parameter.
#[derive(Debug, Clone)]
pub struct Markers {
    pub name: String,
    begin: Regex,
    end: Regex,
}

impl Markers {
    pub fn new(name: &str, begin: &str, end: &str) -> Result<Markers, String> {
        let re = |p: &str| Regex::new(p).map_err(|e| format!("composition `{name}`: {e}"));
        Ok(Markers {
            name: name.to_string(),
            begin: re(begin)?,
            end: re(end)?,
        })
    }
}

/// Read mode: splits `text` by the first marker syntax that finds a part.
/// `None` when no syntax matches anything.
pub fn read(text: &str, markers: &[Markers]) -> Option<Composition> {
    markers.iter().find_map(|m| read_one(text, m))
}

fn read_one(text: &str, m: &Markers) -> Option<Composition> {
    let mut parts = Vec::new();
    let mut problems = Vec::new();
    let mut open: Option<Part> = None;
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let (start, end) = (at, at + raw.len());
        at = end;
        if open.is_some() && m.end.is_match(line) {
            let mut p = open.take().expect("open part");
            p.body_end = start;
            p.end = end;
            parts.push(p);
            continue;
        }
        let Some(c) = m.begin.captures(line) else {
            if m.end.is_match(line) {
                problems.push(Problem {
                    offset: start,
                    message: "end marker without an open part".to_string(),
                });
            }
            continue;
        };
        if let Some(p) = open.take() {
            problems.push(Problem {
                offset: start,
                message: format!(
                    "part '{}' is not closed before the next part starts (nested parts are not supported)",
                    p.name
                ),
            });
        }
        let mut parameters = BTreeMap::new();
        let mut name = None;
        let mut template = None;
        for g in m.begin.capture_names().flatten() {
            let Some(v) = c.name(g) else { continue };
            match g {
                "part" => name = Some(v.as_str().to_string()),
                "template" => template = Some(v.as_str().to_string()),
                _ => {
                    parameters.insert(g.to_string(), v.as_str().to_string());
                }
            }
        }
        open = Some(Part {
            name: name.unwrap_or_else(|| format!("part{}", parts.len() + 1)),
            template,
            parameters,
            start,
            end,
            body_start: end,
            body_end: end,
            template_hash: None,
        });
    }
    if let Some(p) = open {
        problems.push(Problem {
            offset: p.start,
            message: format!("part '{}' has no end marker", p.name),
        });
    }
    if parts.is_empty() && problems.is_empty() {
        return None;
    }
    Some(Composition {
        parts,
        origin: Origin::Markers(m.name.clone()),
        stale: false,
        problems,
    })
}

/// Result of checking one part against its template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Match,
    /// The substituted template differs from the generated part from
    /// `line` (1-based, in the output) on.
    Mismatch {
        line: usize,
        expected: String,
        actual: String,
    },
    /// The part names no template, or the template was not found.
    NoTemplate,
    /// The template could not be expanded (missing parameters, special forms).
    NotExpandable(Vec<String>),
    /// The template changed since the part was generated.
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartCheck {
    /// Index into [`Composition::parts`].
    pub part: usize,
    pub outcome: Outcome,
    /// Placeholder candidates in the template that name no parameter of the part.
    pub unresolved: Vec<String>,
}

/// Check mode: substitutes each part's parameters into its template and
/// compares the result with the part. Lines compare without trailing
/// whitespace or line-ending differences.
pub fn check(
    text: &str,
    comp: &Composition,
    syntax: &Compiled,
    templates: &dyn Fn(&str) -> Option<String>,
) -> Vec<PartCheck> {
    let lines = sqldiff_syntax::LineIndex::new(text);
    comp.parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let tpl = p.template.as_deref().and_then(templates);
            let Some(tpl) = tpl else {
                return PartCheck {
                    part: i,
                    outcome: Outcome::NoTemplate,
                    unresolved: Vec::new(),
                };
            };
            let known: BTreeSet<String> = p.parameters.keys().cloned().collect();
            let t = Template::parse(&tpl, syntax, Some(&known));
            let unresolved = t.unresolved().map(|s| t.slice(s).to_string()).collect();
            if p.template_hash
                .as_ref()
                .is_some_and(|h| *h != content_hash(&tpl))
            {
                return PartCheck {
                    part: i,
                    outcome: Outcome::Stale,
                    unresolved,
                };
            }
            let outcome = match t.substitute(&p.parameters) {
                Err(e) => Outcome::NotExpandable(e.iter().map(|x| x.to_string()).collect()),
                Ok(out) => {
                    let norm = |s: &str| -> Vec<String> {
                        let mut v: Vec<String> =
                            s.lines().map(|l| l.trim_end().to_string()).collect();
                        while v.last().is_some_and(|l| l.is_empty()) {
                            v.pop();
                        }
                        v
                    };
                    let want = norm(&out.text);
                    let got = norm(&text[p.body_start..p.body_end]);
                    match (0..want.len().max(got.len())).find(|&k| want.get(k) != got.get(k)) {
                        None => Outcome::Match,
                        Some(k) => Outcome::Mismatch {
                            line: lines.line_col(p.body_start).0 + k,
                            expected: want.get(k).cloned().unwrap_or_default(),
                            actual: got.get(k).cloned().unwrap_or_default(),
                        },
                    }
                }
            };
            PartCheck {
                part: i,
                outcome,
                unresolved,
            }
        })
        .collect()
}

/// One side's composition input to [`annotate`].
#[derive(Debug, Clone, Copy)]
pub struct Composed<'a> {
    pub artifact: &'a Artifact,
    pub composition: Option<&'a Composition>,
    /// Check-mode results; empty when check mode did not run.
    pub checks: &'a [PartCheck],
}

impl<'a> Composed<'a> {
    pub fn new(artifact: &'a Artifact, composition: Option<&'a Composition>) -> Self {
        Composed {
            artifact,
            composition,
            checks: &[],
        }
    }
}

/// Adds composition findings, wrapper facts and unknown reasons to a report
/// from [`crate::diff`] (with `b`) or [`crate::check`] (without), and
/// recomputes the verdict.
pub fn annotate(report: &mut Report, a: Composed, b: Option<Composed>, opts: &Options) {
    let aa = Analyzed::new(a.artifact, Side::A, opts.excerpt_lines);
    let ab = b.map(|b| Analyzed::new(b.artifact, Side::B, opts.excerpt_lines));
    let object = aa.object.clone();
    let mut sides = vec![(&aa, a)];
    if let (Some(ab), Some(b)) = (&ab, b) {
        sides.push((ab, b));
    }
    for (an, c) in &sides {
        let Some(comp) = c.composition else { continue };
        let label = format!("side {:?}", an.side);
        for p in &comp.problems {
            report.unknown_reasons.push(format!(
                "{label} composition, line {}: {}",
                an.lines.line_col(p.offset).0,
                p.message
            ));
        }
        if comp.stale {
            report.unknown_reasons.push(format!(
                "{label} is flagged stale by its source (generated output older than its inputs)"
            ));
        }
        check_findings(report, an, comp, c.checks, &object);
        wrapper_facts(report, an, comp);
    }
    if let (Some(ab), Some(b)) = (&ab, b) {
        if let (Some(ca), Some(cb)) = (a.composition, b.composition) {
            compare_parts(report, &aa, ca, ab, cb, &object);
        }
    }
    *report = crate::engine::finish(report.clone());
}

fn part_label(p: &Part) -> String {
    match &p.template {
        Some(t) => format!("part '{}' (template {t})", p.name),
        None => format!("part '{}'", p.name),
    }
}

#[allow(clippy::too_many_arguments)]
fn part_finding(
    an: &Analyzed,
    object: &str,
    rule: &str,
    class: ChangeClass,
    severity: Severity,
    verdict: Verdict,
    witness: String,
    p: &Part,
) -> Finding {
    let anchor = format!("{}|{}", p.name, p.template.as_deref().unwrap_or(""));
    let loc = an.location(p.body_start, p.body_end.max(p.body_start));
    Finding {
        fingerprint: fingerprint(&FingerprintInput {
            object,
            class,
            rule,
            anchor_a: if an.side == Side::A { &anchor } else { "" },
            anchor_b: if an.side == Side::B { &anchor } else { "" },
        }),
        rule: rule.to_string(),
        class,
        severity,
        verdict,
        witness,
        a: (an.side == Side::A).then(|| loc.clone()),
        b: (an.side == Side::B).then_some(loc),
        assumptions: Vec::new(),
    }
}

fn check_findings(
    report: &mut Report,
    an: &Analyzed,
    comp: &Composition,
    checks: &[PartCheck],
    object: &str,
) {
    let side = format!("side {:?}", an.side);
    for c in checks {
        let p = &comp.parts[c.part];
        for u in &c.unresolved {
            report.findings.push(part_finding(
                an,
                object,
                "template.unresolved",
                ChangeClass::Structural,
                Severity::Medium,
                Verdict::Unknown,
                format!(
                    "Template of {} on {side}: `{u}` looks like a placeholder but names no parameter of the part; it was kept as SQL.",
                    part_label(p)
                ),
                p,
            ));
        }
        match &c.outcome {
            Outcome::Match => {}
            Outcome::Mismatch {
                line,
                expected,
                actual,
            } => report.findings.push(part_finding(
                an,
                object,
                "composition.substitution-mismatch",
                ChangeClass::Structural,
                Severity::High,
                Verdict::Unknown,
                format!(
                    "Check mode: {} on {side} does not match its template with the part's parameters substituted; line {line} is `{}`, the template gives `{}`. The output was edited after generation or the template changed.",
                    part_label(p),
                    actual.trim(),
                    expected.trim()
                ),
                p,
            )),
            Outcome::NoTemplate => report.unknown_reasons.push(format!(
                "{side} {}: no template to check it against",
                part_label(p)
            )),
            Outcome::NotExpandable(why) => report.unknown_reasons.push(format!(
                "{side} {}: template not checked ({})",
                part_label(p),
                why.join("; ")
            )),
            Outcome::Stale => report.unknown_reasons.push(format!(
                "{side} {} is stale: its template changed since the output was generated",
                part_label(p)
            )),
        }
    }
}

/// Statements whose significant text starts inside a part's body.
fn statements_in<'n>(p: &Part, units: &'n [SyntaxNode]) -> Vec<&'n SyntaxNode> {
    units
        .iter()
        .filter(|n| {
            let (s, _) = ast::sig_range(n);
            s >= p.body_start && s < p.body_end
        })
        .filter(|n| {
            // Outermost only: skip a unit whose parent unit is also in the part.
            !n.ancestors().skip(1).any(|x| {
                x.kind().is_unit() && {
                    let (s, _) = ast::sig_range(&x);
                    s >= p.body_start && s < p.body_end
                }
            })
        })
        .collect()
}

fn compare_parts(
    report: &mut Report,
    aa: &Analyzed,
    ca: &Composition,
    ab: &Analyzed,
    cb: &Composition,
    object: &str,
) {
    let na: Vec<&str> = ca.parts.iter().map(|p| p.name.as_str()).collect();
    let nb: Vec<&str> = cb.parts.iter().map(|p| p.name.as_str()).collect();
    let pairs = lcs_pairs(&na, &nb);
    let matched_a: BTreeSet<usize> = pairs.iter().map(|x| x.0).collect();
    let matched_b: BTreeSet<usize> = pairs.iter().map(|x| x.1).collect();
    for &(i, j) in &pairs {
        let (pa, pb) = (&ca.parts[i], &cb.parts[j]);
        if pa.template != pb.template {
            report.findings.push(part_finding(
                ab,
                object,
                "composition.template-changed",
                ChangeClass::Structural,
                Severity::Low,
                Verdict::Equal,
                format!(
                    "Composition: part '{}' is generated from template {} on side A and {} on side B; statement differences inside it are listed separately.",
                    pa.name,
                    pa.template.as_deref().unwrap_or("(none)"),
                    pb.template.as_deref().unwrap_or("(none)")
                ),
                pb,
            ));
        }
        let keys: BTreeSet<&String> = pa.parameters.keys().chain(pb.parameters.keys()).collect();
        let changed: Vec<String> = keys
            .into_iter()
            .filter(|k| pa.parameters.get(*k) != pb.parameters.get(*k))
            .map(|k| {
                let v = |m: &BTreeMap<String, String>| {
                    m.get(k).map_or("(unset)".to_string(), |v| format!("`{v}`"))
                };
                format!("{k}: {} → {}", v(&pa.parameters), v(&pb.parameters))
            })
            .collect();
        if !changed.is_empty() {
            report.findings.push(part_finding(
                ab,
                object,
                "composition.parameters-changed",
                ChangeClass::Structural,
                Severity::Info,
                Verdict::Equal,
                format!(
                    "Composition: {} has different parameters ({}).",
                    part_label(pb),
                    changed.join(", ")
                ),
                pb,
            ));
        }
    }
    for (an, comp, matched, removed) in [(aa, ca, &matched_a, true), (ab, cb, &matched_b, false)] {
        let units: Vec<SyntaxNode> = an
            .root
            .descendants()
            .filter(|n| n.kind().is_unit())
            .collect();
        for (i, p) in comp.parts.iter().enumerate() {
            if matched.contains(&i) {
                continue;
            }
            let stmts = statements_in(p, &units);
            let kinds: Vec<StmtKind> = stmts.iter().map(|n| stmt_kind(n, an.dialect())).collect();
            let effectful: Vec<ChangeClass> = kinds
                .iter()
                .filter(|k| k.is_effectful())
                .map(|k| k.class())
                .collect();
            let (class, verdict) = if let Some(c) = effectful
                .iter()
                .copied()
                .max_by_key(|c| c.default_severity())
            {
                (c, Verdict::Different)
            } else if !stmts.is_empty() {
                (ChangeClass::Structural, Verdict::Unknown)
            } else {
                (ChangeClass::Cosmetic, Verdict::Equal)
            };
            let held: Vec<String> = stmts
                .iter()
                .zip(&kinds)
                .map(|(n, k)| {
                    format!(
                        "{} `{}` (line {})",
                        crate::treediff::article(k.label()),
                        an.snippet(n, 60),
                        an.line_of(n)
                    )
                })
                .collect();
            let (this, other, verb) = if removed {
                ("A", "B", "missing from")
            } else {
                ("B", "A", "not in")
            };
            let (l0, l1) = (
                an.lines.line_col(p.start).0,
                an.lines.line_col(p.end.saturating_sub(1).max(p.start)).0,
            );
            let holds = if held.is_empty() {
                "it holds no statements".to_string()
            } else {
                format!("it holds {}", held.join(", "))
            };
            let witness = format!(
                "Composition: {} is present in the side {this} output (lines {l0}-{l1}) and {verb} the side {other} output; {holds}.",
                part_label(p)
            );
            // The part explains these statement findings; fold them into it.
            let fold_rule = if removed {
                "stmt.removed"
            } else {
                "stmt.added"
            };
            report.findings.retain(|f| {
                let loc = if removed { &f.a } else { &f.b };
                !(f.rule == fold_rule
                    && loc
                        .as_ref()
                        .is_some_and(|l| l.byte_start >= p.body_start && l.byte_end <= p.body_end))
            });
            let rule = if removed {
                "composition.part-removed"
            } else {
                "composition.part-added"
            };
            report.findings.push(part_finding(
                an,
                object,
                rule,
                class,
                class.default_severity(),
                verdict,
                witness,
                p,
            ));
        }
    }
}

/// A wrapper fact: transaction, error-handling or session behaviour stated
/// by a part other than the one holding the routine's writes.
fn wrapper_facts(report: &mut Report, an: &Analyzed, comp: &Composition) {
    let f = facts::extract(an);
    let body = f
        .writes
        .first()
        .and_then(|w| comp.part_at(ast::sig_range(&w.node).0))
        .map(|p| p.name.clone());
    let mut found: Vec<(usize, String, String)> = Vec::new();
    for n in an.root.descendants().filter(|n| n.kind().is_unit()) {
        let kind = stmt_kind(&n, an.dialect());
        let key = match kind {
            StmtKind::TxnSetting | StmtKind::SessionSetting => {
                format!(
                    "setting-{}",
                    ast::first_words(&n, 2).join("-").to_lowercase()
                )
            }
            StmtKind::TxnBegin => "transaction".to_string(),
            StmtKind::Handler | StmtKind::TryCatch | StmtKind::ExceptionSection => {
                format!(
                    "handler-{}",
                    kind.label().replace(['/', ' '], "-").to_lowercase()
                )
            }
            StmtKind::Block if f.atomic_blocks.contains(&n) => "atomic".to_string(),
            _ => continue,
        };
        found.push((ast::sig_range(&n).0, key, an.snippet(&n, 60)));
    }
    if let Some((level, off)) = &f.commit_level {
        found.push((
            *off,
            "commit-level".to_string(),
            format!("COMMIT = {level}"),
        ));
    }
    for (off, key, what) in found {
        let Some(p) = comp.part_at(off) else { continue };
        if body.as_deref() == Some(p.name.as_str()) {
            continue;
        }
        // Handlers nested in another wrapper fact are covered by it.
        if key.starts_with("handler-")
            && report.assumptions.iter().any(|x| {
                x.id.starts_with(&format!("COMPOSED-{:?}:{}:handler-", an.side, p.name))
            })
        {
            continue;
        }
        let id = format!("COMPOSED-{:?}:{}:{key}", an.side, p.name);
        if report.assumptions.iter().any(|x| x.id == id) {
            continue;
        }
        report.assumptions.push(Assumption {
            id,
            text: format!(
                "`{what}` (side {:?} line {}) comes from {} of the composed output",
                an.side,
                an.lines.line_col(off).0,
                part_label(p)
            ),
            source: AssumptionSource::ComposedOutput,
        });
    }
}

/// Template level: maps the locations and diagnostics of one side of a
/// report on a template's SQL fragment ([`Template::fragment`]) back to the
/// template, so findings point at the template the author edits.
pub fn remap_to_template(
    report: &mut Report,
    side: Side,
    template: &Template,
    fragment: &Derived,
    dialect: sqldiff_syntax::Dialect,
    excerpt_lines: usize,
) {
    let text = &template.text;
    let lines = sqldiff_syntax::LineIndex::new(text);
    // Witnesses quote fragment text; show placeholders as the template spells them.
    let mut spelled: Vec<(String, String)> = template
        .segments
        .iter()
        .filter_map(|s| match &s.kind {
            SegmentKind::Placeholder { name } => {
                Some((stand_in(name), template.slice(s).to_string()))
            }
            _ => None,
        })
        .collect();
    spelled.sort_by(|x, y| y.0.len().cmp(&x.0.len()).then(x.0.cmp(&y.0)));
    spelled.dedup_by(|x, y| x.0 == y.0);
    let respell = |s: &str| {
        spelled.iter().fold(s.to_string(), |acc, (from, to)| {
            acc.replace(from.as_str(), to)
        })
    };
    let mine = |f: &Finding| [&f.a, &f.b].into_iter().flatten().any(|l| l.side == side);
    for f in report.findings.iter_mut().filter(|f| mine(f)) {
        f.witness = respell(&f.witness);
    }
    let info = if side == Side::A {
        Some(&mut report.a)
    } else {
        report.b.as_mut()
    };
    if let Some(info) = info {
        info.object = respell(&info.object);
        info.content_hash = content_hash(text);
    }
    for f in &mut report.findings {
        for loc in [&mut f.a, &mut f.b].into_iter().flatten() {
            if loc.side != side {
                continue;
            }
            let (s, e) = fragment.map.src_range(loc.byte_start, loc.byte_end);
            let (sl, sc) = lines.line_col(s);
            let (el, ec) = lines.line_col(e);
            loc.byte_start = s;
            loc.byte_end = e;
            loc.start_line = sl;
            loc.start_col = sc;
            loc.end_line = el;
            loc.end_col = ec;
            loc.excerpt = text[s..e]
                .lines()
                .take(excerpt_lines.max(1))
                .map(|l| l.trim_end().to_string())
                .collect();
        }
    }
    report.diagnostics.retain(|d| d.side != side);
    let (_, diags) = fragment.parse(dialect);
    for d in diags {
        let (line, col) = lines.line_col(d.offset);
        report.diagnostics.push(crate::model::DiagnosticInfo {
            side,
            line,
            col,
            message: d.message,
        });
    }
    report.normalizations.push(format!(
        "side {side:?} is a template: placeholders read as identifiers, special forms as comments"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers() -> Vec<Markers> {
        vec![Markers::new(
            "test",
            r"^-- \[part:(?P<part>[a-z0-9-]+) template=(?P<template>[a-z0-9-]+)(?: table=(?P<TABLE>\S+))?\]$",
            r"^-- \[/part\]$",
        )
        .unwrap()]
    }

    #[test]
    fn read_mode_offsets() {
        let text = "-- [part:head template=h]\nSELECT 1;\n-- [/part]\nfree text\n-- [part:body template=b table=dbo.t]\nDELETE FROM dbo.t;\n-- [/part]\n";
        let c = read(text, &markers()).unwrap();
        assert!(c.problems.is_empty(), "{:?}", c.problems);
        assert_eq!(c.parts.len(), 2);
        assert_eq!(
            &text[c.parts[0].body_start..c.parts[0].body_end],
            "SELECT 1;\n"
        );
        assert_eq!(
            &text[c.parts[1].body_start..c.parts[1].body_end],
            "DELETE FROM dbo.t;\n"
        );
        assert_eq!(c.parts[1].parameters["TABLE"], "dbo.t");
        assert_eq!(c.parts[1].template.as_deref(), Some("b"));
        assert_eq!(
            c.part_at(text.find("DELETE").unwrap()).unwrap().name,
            "body"
        );
        assert!(c.part_at(text.find("free").unwrap()).is_none());
        assert!(read("SELECT 1;\n", &markers()).is_none());
    }

    #[test]
    fn unbalanced_markers_are_problems() {
        let c = read(
            "-- [part:a template=x]\nSELECT 1;\n-- [part:b template=y]\nSELECT 2;\n",
            &markers(),
        )
        .unwrap();
        assert_eq!(c.problems.len(), 2, "{:?}", c.problems);
        let c = read("SELECT 1;\n-- [/part]\n", &markers()).unwrap();
        assert_eq!(c.problems.len(), 1);
    }

    #[test]
    fn check_mode() {
        use sqldiff_syntax::template::TemplateSyntax;
        let syntax = Compiled::new(&TemplateSyntax {
            open: "{{".into(),
            close: "}}".into(),
            name_pattern: "[A-Z][A-Z0-9_]*".into(),
            special_forms: vec![],
        })
        .unwrap();
        let text = "-- [part:body template=b table=dbo.t]\nDELETE FROM dbo.t;  \r\n-- [/part]\n";
        let c = read(text, &markers()).unwrap();
        let ok = check(text, &c, &syntax, &|_| {
            Some("DELETE FROM {{TABLE}};\n".into())
        });
        assert_eq!(ok[0].outcome, Outcome::Match);
        let bad = check(text, &c, &syntax, &|_| {
            Some("DELETE FROM {{TABLE}} WHERE 1 = 0;\n".into())
        });
        assert!(
            matches!(&bad[0].outcome, Outcome::Mismatch { line: 2, .. }),
            "{bad:?}"
        );
        // KEY is not a parameter of the part: kept as SQL and listed, never guessed.
        let unknown = check(text, &c, &syntax, &|_| {
            Some("DELETE FROM {{TABLE}} WHERE k = {{KEY}};".into())
        });
        assert_eq!(unknown[0].unresolved, vec!["{{KEY}}".to_string()]);
        assert!(matches!(&unknown[0].outcome, Outcome::Mismatch { .. }));
        let none = check(text, &c, &syntax, &|_| None);
        assert_eq!(none[0].outcome, Outcome::NoTemplate);
        let mut stale = c.clone();
        stale.parts[0].template_hash = Some("old".into());
        let s = check(text, &stale, &syntax, &|_| {
            Some("DELETE FROM {{TABLE}};\n".into())
        });
        assert_eq!(s[0].outcome, Outcome::Stale);
    }

    #[test]
    fn adapter_spans_are_validated() {
        let p = |name: &str, s, e| Part {
            name: name.into(),
            template: None,
            parameters: BTreeMap::new(),
            start: s,
            end: e,
            body_start: s,
            body_end: e,
            template_hash: None,
        };
        let c = Composition::from_spans(
            "0123456789",
            vec![p("a", 0, 5), p("b", 4, 8), p("c", 8, 20)],
            false,
        );
        assert_eq!(c.parts.len(), 2);
        assert_eq!(c.problems.len(), 2);
    }
}
