//! Session report: a short Markdown summary of a review session (design
//! §6.2 "sessions export as short reports, optionally redacted").
//!
//! Redaction uses the shared redactor of `sqldiff-review` (pack-derived
//! terms, stable keyed placeholders, leak check), seeded with the session's
//! paths, objects and every identifier seen in either source; string
//! literals are masked as well.

use std::fmt::Write;

use regex::Regex;
use sqldiff_core::Report;
use sqldiff_review::redact::Kind;
use sqldiff_source::Resolved;
use sqldiff_syntax::{SyntaxElement, SyntaxKind};

use crate::review::{time, Store};
use crate::session::{Flip, QueueEntry, SessionInfo};

/// Replaces names with stable placeholders, or passes text through.
pub struct Redactor {
    inner: Option<sqldiff_review::Redactor>,
    literal: Regex,
    /// Redacted outputs that still held a registered term.
    leaked: usize,
}

impl Redactor {
    pub fn new(cfg: &Resolved) -> Result<Redactor, String> {
        Ok(Redactor {
            inner: Some(sqldiff_review::Redactor::from_config(cfg).map_err(|e| e.0)?),
            literal: Regex::new(r"'(?:[^']|'')*'").unwrap(),
            leaked: 0,
        })
    }

    pub fn off() -> Redactor {
        Redactor {
            inner: None,
            literal: Regex::new("$^").unwrap(),
            leaked: 0,
        }
    }

    pub fn is_on(&self) -> bool {
        self.inner.is_some()
    }

    pub fn path(&mut self, p: &str) {
        if let Some(r) = &mut self.inner {
            r.add_path(p);
        }
    }

    pub fn report(&mut self, rep: &Report) {
        if let Some(r) = &mut self.inner {
            r.add_report(rep);
        }
    }

    /// Every identifier, variable and parameter of a source text.
    pub fn source(&mut self, text: &str, dialect: sqldiff_syntax::Dialect) {
        let Some(r) = &mut self.inner else { return };
        let parse = sqldiff_syntax::parse(text, dialect);
        for t in parse
            .syntax()
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            use SyntaxKind::*;
            let keep = match t.kind() {
                IDENT => {
                    let up = t.text().to_uppercase();
                    !(sqldiff_core::normalize::is_keyword(&up) || crate::is_extra_keyword(&up))
                }
                QUOTED_IDENT | VARIABLE | PARAM => true,
                _ => false,
            };
            if keep {
                let name = sqldiff_syntax::ast::unquote(t.text());
                r.add(Kind::Object, &name);
                r.add(Kind::Object, name.trim_start_matches(['@', '#', ':']));
            }
        }
    }

    pub fn apply(&mut self, s: &str) -> String {
        let Some(r) = &mut self.inner else {
            return s.to_string();
        };
        let s = self.literal.replace_all(s, "'…'");
        let out = r.redact_str(&s);
        if !r.leaks(&out).is_empty() {
            self.leaked += 1;
        }
        out
    }

    /// Refuses the report if any redacted value still held a registered
    /// term. Only values that went through [`Redactor::apply`] are checked:
    /// the report's own template words (`config`, `verdict`) are not data.
    pub fn verify(&self) -> Result<(), String> {
        if self.leaked > 0 {
            return Err(format!(
                "redacted report still contains identifying terms in {} value(s); not written",
                self.leaked
            ));
        }
        Ok(())
    }
}

/// Renders the session report.
pub fn markdown(
    info: &SessionInfo,
    queue: &[QueueEntry],
    reports: &[(Option<Report>, String)],
    store: &Store,
    red: &mut Redactor,
) -> String {
    let now = time::now();
    let mut o = String::new();
    let _ = writeln!(o, "# SQLDiff review session\n");
    let _ = writeln!(
        o,
        "- sqldiff {} · read {}{}",
        info.tool_version,
        info.loaded_at,
        if red.is_on() { " · redacted" } else { "" }
    );
    let layers: Vec<String> = info
        .layers
        .iter()
        .map(|l| {
            let name = if red.is_on() {
                format!("{:?}", l.kind).to_lowercase()
            } else {
                l.name.clone()
            };
            format!("{name} `{}`", &l.hash[..l.hash.len().min(12)])
        })
        .collect();
    let _ = writeln!(
        o,
        "- config: {}",
        if layers.is_empty() {
            "built-in".to_string()
        } else {
            layers.join(", ")
        }
    );
    let _ = writeln!(
        o,
        "- dialect versions: {}",
        info.dialect_versions
            .iter()
            .map(|(d, v)| format!("{d} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let flipped: Vec<String> = info
        .assumptions
        .iter()
        .filter(|a| a.flip != Flip::Open)
        .map(|a| {
            format!(
                "{} {}",
                red.apply(&a.id),
                if a.flip == Flip::Denied {
                    "denied"
                } else {
                    "confirmed"
                }
            )
        })
        .collect();
    if !flipped.is_empty() {
        let _ = writeln!(o, "- assumption flips: {}", flipped.join(", "));
    }
    let _ = writeln!(
        o,
        "\n| # | verdict | highest | findings | reviewed | pair |\n|---|---|---|---|---|---|"
    );
    for e in queue {
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} | {}/{} | {} ↔ {} |",
            e.index + 1,
            e.verdict.map_or("error", |v| v.label()),
            e.highest_class.map_or("-", |c| c.label()),
            e.findings,
            e.reviewed,
            e.findings,
            red.apply(&e.a),
            red.apply(&e.b),
        );
    }
    for p in &info.only_a {
        let _ = writeln!(o, "| - | only in A | - | - | - | {} |", red.apply(p));
    }
    for p in &info.only_b {
        let _ = writeln!(o, "| - | only in B | - | - | - | {} |", red.apply(p));
    }
    for (e, (r, snap)) in queue.iter().zip(reports) {
        let Some(r) = r else { continue };
        if r.findings.is_empty() {
            continue;
        }
        let _ = writeln!(
            o,
            "\n## {}. {} ↔ {}\n",
            e.index + 1,
            red.apply(&e.a),
            red.apply(&e.b)
        );
        for f in &r.findings {
            let line = |l: &Option<sqldiff_core::model::Location>| {
                l.as_ref().map(|l| format!("{:?} {}", l.side, l.start_line))
            };
            let at: Vec<String> = [line(&f.a), line(&f.b)].into_iter().flatten().collect();
            let _ = write!(
                o,
                "- **{}** {} · {} · `{}` `{}`{}: {}",
                f.class.label(),
                f.severity.label(),
                f.verdict.label(),
                red.apply(&f.rule),
                f.fingerprint,
                if at.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", at.join(" ↔ "))
                },
                red.apply(&f.witness)
            );
            let st = store.status(&f.fingerprint, snap, &now);
            if let Some(d) = &st.decision {
                let _ = write!(o, " — {}", st.state.label());
                if st.state == sqldiff_review::ReviewState::Outdated {
                    let _ = write!(o, " ({} on an earlier snapshot)", d.state.label());
                }
                if let Some(c) = &d.comment {
                    let _ = write!(o, ": “{}”", red.apply(c));
                }
            }
            let _ = writeln!(o);
        }
    }
    o
}
