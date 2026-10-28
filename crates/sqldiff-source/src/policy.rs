//! Pack policy applied to engine reports: severity and class overrides,
//! declared assumptions, limit rules, staleness, author markers and
//! template placeholders.

use regex::Regex;
use serde::{Deserialize, Serialize};
use sqldiff_core::model::{fingerprint, AssumptionSource, FingerprintInput, Location, Side};
use sqldiff_core::{Artifact, ChangeClass, Finding, Report, Severity, Verdict};
use sqldiff_syntax::LineIndex;

use crate::pack::{MarkerKind, TemplateSyntax};
use crate::Resolved;

/// Applies overrides and declared assumptions to a report.
pub fn apply(report: &mut Report, config: &Resolved) {
    for f in &mut report.findings {
        if let Some(o) = config.override_for(&f.rule) {
            if let Some(c) = o.class {
                f.class = c;
            }
            if let Some(s) = o.severity {
                f.severity = s;
            }
        }
    }
    for a in &mut report.assumptions {
        if let Some(text) = config.policy.assumptions.get(&a.id) {
            a.source = AssumptionSource::Declared;
            a.text = text.clone();
        }
    }
}

/// Caps the verdict at unknown when a side is stale generated output.
pub fn cap_stale(report: &mut Report, stale_a: bool, stale_b: bool) {
    if !(stale_a || stale_b) {
        return;
    }
    let side = if stale_a { "A" } else { "B" };
    report.unknown_reasons.push(format!(
        "side {side} is flagged stale by its source (generated output older than its inputs)"
    ));
    report.settle();
}

/// Findings of class `limit` for one artifact.
pub fn limit_findings(art: &Artifact, side: Side, config: &Resolved) -> Vec<Finding> {
    let mut out = Vec::new();
    let lines = LineIndex::new(&art.text);
    let loc = |start: usize, end: usize| {
        let (sl, sc) = lines.line_col(start);
        let (el, ec) = lines.line_col(end);
        Location {
            side,
            path: art.path.clone(),
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
            byte_start: start,
            byte_end: end,
            excerpt: art.text[start..end]
                .lines()
                .take(1)
                .map(|l| l.trim_end().to_string())
                .collect(),
        }
    };
    let object = art.path.as_str();
    let mut push =
        |rule: &str, key: &str, severity: Option<Severity>, witness: String, l: Location| {
            let f = Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object,
                    class: ChangeClass::Limit,
                    rule,
                    anchor_a: if side == Side::A { key } else { "" },
                    anchor_b: if side == Side::B { key } else { "" },
                }),
                rule: rule.to_string(),
                class: ChangeClass::Limit,
                severity: severity.unwrap_or(ChangeClass::Limit.default_severity()),
                verdict: Verdict::Unknown,
                witness,
                a: (side == Side::A).then(|| l.clone()),
                b: (side == Side::B).then_some(l),
                assumptions: Vec::new(),
            };
            out.push(f);
        };
    for rule in &config.policy.limits {
        let applies = rule.dialects.is_empty()
            || rule
                .dialects
                .iter()
                .any(|d| d.parse().ok() == Some(art.dialect));
        if !applies {
            continue;
        }
        let id = format!("limit.{}", rule.id);
        if let Some(max) = rule.max_bytes {
            if art.text.len() > max {
                push(
                    &id,
                    "bytes",
                    rule.severity,
                    format!("{} ({} bytes, limit {max})", rule.message, art.text.len()),
                    loc(0, 0),
                );
            }
        }
        if let Some(max) = rule.max_lines {
            let n = art.text.lines().count();
            if n > max {
                push(
                    &id,
                    "lines",
                    rule.severity,
                    format!("{} ({n} lines, limit {max})", rule.message),
                    loc(0, 0),
                );
            }
        }
        if let Some(p) = &rule.pattern {
            let Ok(re) = Regex::new(p) else { continue };
            for m in re.find_iter(&art.text) {
                let (line, _) = lines.line_col(m.start());
                push(
                    &id,
                    &m.as_str().to_lowercase(),
                    rule.severity,
                    format!("Side {side:?} line {line}: {}", rule.message),
                    loc(m.start(), m.end()),
                );
            }
        }
    }
    out
}

/// Adds limit findings for both sides of a report.
/// Limit findings are unknown, so a report that gains one can no longer be
/// equal; its verdict is derived again.
pub fn add_limits(report: &mut Report, a: &Artifact, b: Option<&Artifact>, config: &Resolved) {
    let before = report.findings.len();
    report.findings.extend(limit_findings(a, Side::A, config));
    if let Some(b) = b {
        report.findings.extend(limit_findings(b, Side::B, config));
    }
    if report.findings.len() > before {
        report.settle();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub kind: MarkerKind,
    pub value: String,
    pub line: usize,
}

/// Author markers (review notes, intentional differences, sync partners) in a text.
pub fn markers(text: &str, config: &Resolved) -> Vec<Marker> {
    let lines = LineIndex::new(text);
    let mut out = Vec::new();
    for m in &config.policy.author_markers {
        let Ok(re) = Regex::new(&m.pattern) else {
            continue;
        };
        for c in re.captures_iter(text) {
            let all = c.get(0).expect("group 0");
            let value = c.get(1).unwrap_or(all);
            out.push(Marker {
                kind: m.kind,
                value: value.as_str().trim().to_string(),
                line: lines.line_col(all.start()).0,
            });
        }
    }
    out.sort_by_key(|m| m.line);
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placeholder {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

/// Placeholders of a template syntax definition in a text.
pub fn placeholders(text: &str, t: &TemplateSyntax) -> Vec<Placeholder> {
    let pattern = format!(
        "{}\\s*({})\\s*{}",
        regex::escape(&t.open),
        t.name_pattern,
        regex::escape(&t.close)
    );
    let Ok(re) = Regex::new(&pattern) else {
        return Vec::new();
    };
    re.captures_iter(text)
        .map(|c| {
            let all = c.get(0).expect("group 0");
            Placeholder {
                name: c[1].to_string(),
                start: all.start(),
                end: all.end(),
            }
        })
        .collect()
}
