//! Output formats: human text, JSON (serde) and SARIF 2.1.0.

use std::fmt::Write;

use serde_json::{json, Value};

use crate::model::{Finding, Location, Report, Severity};

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
    let _ = writeln!(out, "Verdict: {}", report.verdict.label().to_uppercase());

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

    if !report.assumptions.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Assumptions:");
        for x in &report.assumptions {
            if show_assumption_details {
                let _ = writeln!(out, "  {:<32} {:?}  {}", x.id, x.source, x.text);
            } else {
                let _ = writeln!(out, "  {:<32} {}", x.id, x.text);
            }
        }
    }
    if !report.normalizations.is_empty() && report.findings.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Normalizations: {}", report.normalizations.join("; "));
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
    json!({
        "physicalLocation": {
            "artifactLocation": { "uri": loc.path },
            "region": {
                "startLine": loc.start_line,
                "startColumn": loc.start_col,
                "endLine": loc.end_line,
                "endColumn": loc.end_col,
            }
        },
        "message": { "text": format!("side {:?}", loc.side) }
    })
}

fn sarif_result(f: &Finding) -> Value {
    // The primary location is the side under review (B), else A.
    let (primary, related) = match (&f.b, &f.a) {
        (Some(b), a) => (Some(b), a.as_ref()),
        (None, a) => (a.as_ref(), None),
    };
    json!({
        "ruleId": f.rule,
        "level": sarif_level(f.severity),
        "message": { "text": f.witness },
        "locations": primary.map(|l| vec![sarif_location(l)]).unwrap_or_default(),
        "relatedLocations": related.map(|l| vec![sarif_location(l)]).unwrap_or_default(),
        "partialFingerprints": { "sqldiff/v1": f.fingerprint },
        "properties": {
            "class": f.class.label(),
            "severity": f.severity.label(),
            "verdict": f.verdict.label(),
            "assumptions": f.assumptions,
        }
    })
}

/// SARIF 2.1.0 for one or more reports.
pub fn sarif(reports: &[Report]) -> String {
    let mut rules: Vec<String> = reports
        .iter()
        .flat_map(|r| r.findings.iter().map(|f| f.rule.clone()))
        .collect();
    rules.sort();
    rules.dedup();
    let results: Vec<Value> = reports
        .iter()
        .flat_map(|r| r.findings.iter().map(sarif_result))
        .collect();
    let doc = json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": [{
            "tool": { "driver": {
                "name": "sqldiff",
                "version": crate::TOOL_VERSION,
                "rules": rules.iter().map(|r| json!({ "id": r })).collect::<Vec<_>>(),
            }},
            "results": results,
        }]
    });
    serde_json::to_string_pretty(&doc).expect("sarif serializes")
}
