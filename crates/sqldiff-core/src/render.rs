//! Output formats: human text, JSON (serde) and SARIF 2.1.0.

use std::fmt::Write;

use serde_json::{json, Value};

use crate::model::{AssumptionSource, Finding, Location, Report, Severity, SuppressionKind};

pub fn json(report: &Report) -> String {
    serde_json::to_string_pretty(report).expect("report serializes")
}

pub fn text(report: &Report, show_assumption_details: bool) -> String {
    let mut out = String::new();
    let a = &report.a;
    match &report.b {
        Some(b) => {
            let _ = writeln!(
                out,
                "A: {} ({} {}, {})",
                a.path, a.dialect, a.dialect_version, a.object
            );
            let _ = writeln!(
                out,
                "B: {} ({} {}, {})",
                b.path, b.dialect, b.dialect_version, b.object
            );
        }
        None => {
            let _ = writeln!(
                out,
                "{} ({} {}, {})",
                a.path, a.dialect, a.dialect_version, a.object
            );
        }
    }
    match report.verdict_before_suppression {
        Some(before) if before != report.verdict => {
            let _ = writeln!(
                out,
                "Verdict: {} ({} before {} suppressed)",
                report.verdict.label().to_uppercase(),
                before.label(),
                report.suppressed.len()
            );
        }
        _ => {
            let _ = writeln!(out, "Verdict: {}", report.verdict.label().to_uppercase());
        }
    }

    let mut by_class: Vec<(&str, usize)> = Vec::new();
    for f in &report.findings {
        match by_class.iter_mut().find(|(c, _)| *c == f.class.label()) {
            Some((_, n)) => *n += 1,
            None => by_class.push((f.class.label(), 1)),
        }
    }
    let classes: Vec<String> = by_class.iter().map(|(c, n)| format!("{n} {c}")).collect();
    let mut summary = format!(
        "{} finding{}",
        report.findings.len(),
        if report.findings.len() == 1 { "" } else { "s" }
    );
    if !classes.is_empty() {
        let _ = write!(summary, " ({})", classes.join(", "));
    }
    if report.noise.cosmetic > 0 {
        let _ = write!(
            summary,
            " · {} cosmetic change(s) hidden",
            report.noise.cosmetic
        );
    }
    if report.noise.structural > 0 {
        let _ = write!(
            summary,
            " · {} structural change(s) hidden",
            report.noise.structural
        );
    }
    if !report.suppressed.is_empty() {
        let _ = write!(summary, " · {} suppressed", report.suppressed.len());
    }
    let _ = writeln!(out, "{summary}");

    for (i, f) in report.findings.iter().enumerate() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "[{}] {} · {} · {}   {} {}",
            i + 1,
            f.class.label().to_uppercase(),
            f.severity.label(),
            f.verdict.label(),
            f.rule,
            f.fingerprint
        );
        let _ = writeln!(out, "    {}", f.witness);
        for loc in [&f.a, &f.b].into_iter().flatten() {
            write_location(&mut out, loc);
        }
        if !f.assumptions.is_empty() {
            let _ = writeln!(out, "    assumes: {}", f.assumptions.join(", "));
        }
    }

    if !report.suppressed.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Suppressed:");
        for s in &report.suppressed {
            let _ = writeln!(
                out,
                "  {} {} {}  {}",
                s.finding.fingerprint,
                s.finding.class.label(),
                s.finding.rule,
                s.justification
            );
        }
    }

    let (proven, open): (Vec<_>, Vec<_>) = report
        .assumptions
        .iter()
        .partition(|x| x.source == AssumptionSource::ComposedOutput);
    if !open.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Assumptions:");
        for x in open {
            if show_assumption_details {
                let _ = writeln!(out, "  {:<32} {:?}  {}", x.id, x.source, x.text);
            } else {
                let _ = writeln!(out, "  {:<32} {}", x.id, x.text);
            }
            if !x.accepted_by.is_empty() {
                let by: Vec<String> = x.accepted_by.iter().map(|s| s.label()).collect();
                let _ = writeln!(
                    out,
                    "  {:<32} accepted by the deployment profile: {}",
                    "",
                    by.join("; ")
                );
            }
        }
    }
    if !proven.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Proven from composed output:");
        for x in proven {
            let _ = writeln!(out, "  {}", x.text);
        }
    }
    if !report.normalizations.is_empty() && report.findings.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Normalizations: {}", report.normalizations.join("; "));
    }
    if report.equivalent == Some(false) {
        let _ = writeln!(
            out,
            "Equivalent: no, B only refines A (the reference): {}",
            report.refinements.join("; ")
        );
    }
    if !report.unknown_reasons.is_empty() {
        let _ = writeln!(out);
        for r in &report.unknown_reasons {
            let _ = writeln!(out, "Not proven: {r}");
        }
    }
    for d in &report.diagnostics {
        let _ = writeln!(
            out,
            "parse {:?}:{}:{}: {}",
            d.side, d.line, d.col, d.message
        );
    }
    out
}

fn write_location(out: &mut String, loc: &Location) {
    let first = loc.excerpt.first().map(|s| s.trim()).unwrap_or("");
    let _ = writeln!(
        out,
        "    {:?}: {}:{}  {}",
        loc.side, loc.path, loc.start_line, first
    );
    for l in loc.excerpt.iter().skip(1) {
        let _ = writeln!(out, "       {}", l.trim_end());
    }
}

fn sarif_level(s: Severity) -> &'static str {
    match s {
        Severity::Critical | Severity::High => "error",
        Severity::Medium => "warning",
        Severity::Low | Severity::Info => "note",
    }
}

fn sarif_location(loc: &Location) -> Value {
    // Column 0 means a whole-line or whole-file anchor: no columns.
    let region = if loc.start_col == 0 {
        json!({ "startLine": loc.start_line.max(1), "endLine": loc.end_line.max(loc.start_line).max(1) })
    } else {
        json!({
            "startLine": loc.start_line,
            "startColumn": loc.start_col,
            "endLine": loc.end_line,
            "endColumn": loc.end_col,
        })
    };
    json!({
        "physicalLocation": {
            "artifactLocation": { "uri": loc.path },
            "region": region,
        },
        "message": { "text": format!("side {:?}", loc.side) }
    })
}

/// Where a SARIF result points.
#[derive(Debug, Clone)]
pub struct Placement {
    pub primary: Location,
    /// Put before the witness, e.g. to say where a removed line was.
    pub note: Option<String>,
}

/// A whole-file anchor at line `line` (column 0: no columns in SARIF).
pub fn line_anchor(side: crate::model::Side, path: &str, line: usize) -> Location {
    Location {
        side,
        path: path.to_string(),
        start_line: line.max(1),
        start_col: 0,
        end_line: line.max(1),
        end_col: 0,
        byte_start: 0,
        byte_end: 0,
        excerpt: Vec::new(),
    }
}

/// The default placement: the side under review (B), else A, else the
/// first line of the report's B file (A for a check), so every result has
/// a location; code scanning drops results without one.
pub fn default_placement(report: &Report, f: &Finding) -> Placement {
    use crate::model::Side;
    let primary = match (&f.b, &f.a) {
        (Some(b), _) => b.clone(),
        (None, Some(a)) => a.clone(),
        (None, None) => match &report.b {
            Some(b) => line_anchor(Side::B, &b.path, 1),
            None => line_anchor(Side::A, &report.a.path, 1),
        },
    };
    Placement {
        primary,
        note: None,
    }
}

fn same_place(x: &Location, y: &Location) -> bool {
    x.path == y.path && x.start_line == y.start_line && x.end_line == y.end_line && x.side == y.side
}

fn sarif_result(f: &Finding, p: &Placement) -> Value {
    let related: Vec<Value> = [&f.a, &f.b]
        .into_iter()
        .flatten()
        .filter(|l| !same_place(l, &p.primary))
        .map(sarif_location)
        .collect();
    let text = match &p.note {
        Some(n) => format!("{n} {}", f.witness),
        None => f.witness.clone(),
    };
    json!({
        "ruleId": f.rule,
        "level": sarif_level(f.severity),
        "message": { "text": text },
        "locations": [sarif_location(&p.primary)],
        "relatedLocations": related,
        "partialFingerprints": { "sqldiff/v1": f.fingerprint },
        "properties": {
            "class": f.class.label(),
            "severity": f.severity.label(),
            "verdict": f.verdict.label(),
            "assumptions": f.assumptions,
        }
    })
}

/// SARIF 2.1.0 for one or more reports, placed by [`default_placement`].
pub fn sarif(reports: &[Report]) -> String {
    sarif_placed(reports, &|i, f| default_placement(&reports[i], f))
}

/// SARIF 2.1.0 where `place` gives each result's primary location, from the
/// index of its report and the finding. Every result has one location.
pub fn sarif_placed(reports: &[Report], place: &dyn Fn(usize, &Finding) -> Placement) -> String {
    let mut rules: Vec<String> = reports
        .iter()
        .flat_map(|r| {
            r.findings
                .iter()
                .chain(r.suppressed.iter().map(|s| &s.finding))
                .map(|f| f.rule.clone())
        })
        .collect();
    rules.sort();
    rules.dedup();
    let mut results: Vec<Value> = reports
        .iter()
        .enumerate()
        .flat_map(|(i, r)| {
            r.findings
                .iter()
                .map(move |f| sarif_result(f, &place(i, f)))
        })
        .collect();
    // Suppressed findings stay in the log, marked as such (SARIF 3.35).
    for (i, r) in reports.iter().enumerate() {
        for s in &r.suppressed {
            let mut v = sarif_result(&s.finding, &place(i, &s.finding));
            v["suppressions"] = json!([{
                "kind": match s.kind {
                    SuppressionKind::Review => "external",
                    SuppressionKind::InSource => "inSource",
                },
                "justification": s.justification,
            }]);
            results.push(v);
        }
    }
    let doc = json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": [{
            "tool": { "driver": {
                "name": "sqldiff",
                "version": crate::TOOL_VERSION,
                "informationUri": "https://github.com/DavySc/SQLDiff",
                "rules": rules.iter().map(|r| json!({ "id": r })).collect::<Vec<_>>(),
            }},
            "results": results,
        }]
    });
    serde_json::to_string_pretty(&doc).expect("sarif serializes")
}
