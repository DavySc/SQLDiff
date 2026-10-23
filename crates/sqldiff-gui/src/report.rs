//! Session report: a short Markdown summary of a review session (design
//! §6.2 "sessions export as short reports, optionally redacted").
//!
//! Redaction here is a stand-in until the shared redacted export of WP6
//! lands: paths, object names, pack names, identifiers seen in either
//! source and string literals become stable placeholders.

use std::collections::BTreeMap;
use std::fmt::Write;

use regex::Regex;
use sqldiff_core::Report;
use sqldiff_syntax::{SyntaxElement, SyntaxKind};

use crate::review::Store;
use crate::session::{Flip, QueueEntry, SessionInfo};

/// Replaces names with stable placeholders.
pub struct Redactor {
    on: bool,
    /// Exact strings (paths), longest first.
    exact: Vec<(String, String)>,
    /// Lower-cased identifiers.
    words: BTreeMap<String, String>,
    word_re: Regex,
    string_re: Regex,
}

impl Redactor {
    pub fn new(on: bool) -> Redactor {
        Redactor {
            on,
            exact: Vec::new(),
            words: BTreeMap::new(),
            word_re: Regex::new(r"[@#]?[A-Za-z_][A-Za-z0-9_$#@]*").unwrap(),
            string_re: Regex::new(r"'(?:[^']|'')*'").unwrap(),
        }
    }

    pub fn path(&mut self, p: &str) {
        if self.on && !p.is_empty() && !self.exact.iter().any(|(k, _)| k == p) {
            let n = self.exact.len() + 1;
            self.exact.push((p.to_string(), format!("object-{n}")));
            self.exact.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        }
    }

    pub fn word(&mut self, w: &str) {
        let k = w.trim_start_matches(['@', '#']).to_lowercase();
        if self.on && !k.is_empty() && !self.words.contains_key(&k) {
            let n = self.words.len() + 1;
            self.words.insert(k, format!("name-{n}"));
        }
    }

    /// Every identifier, variable and parameter of a source text.
    pub fn source(&mut self, text: &str, dialect: sqldiff_syntax::Dialect) {
        if !self.on {
            return;
        }
        let parse = sqldiff_syntax::parse(text, dialect);
        for t in parse
            .syntax()
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            use SyntaxKind::*;
            let keep = match t.kind() {
                IDENT => {
                    let up = t.text().to_ascii_uppercase();
                    !(sqldiff_core::normalize::is_keyword(&up) || crate::is_extra_keyword(&up))
                }
                QUOTED_IDENT | VARIABLE | PARAM => true,
                _ => false,
            };
            if keep {
                self.word(&sqldiff_syntax::ast::unquote(t.text()));
            }
        }
    }

    pub fn apply(&self, s: &str) -> String {
        if !self.on {
            return s.to_string();
        }
        let mut s = s.to_string();
        for (k, v) in &self.exact {
            s = s.replace(k.as_str(), v);
        }
        let s = self.string_re.replace_all(&s, "'…'");
        self.word_re
            .replace_all(&s, |c: &regex::Captures| {
                let w = &c[0];
                let k = w.trim_start_matches(['@', '#']).to_lowercase();
                match self.words.get(&k) {
                    Some(p) => p.clone(),
                    None => w.to_string(),
                }
            })
            .into_owned()
    }
}

/// Renders the session report.
pub fn markdown(
    info: &SessionInfo,
    queue: &[QueueEntry],
    reports: &[(Option<Report>, String)],
    store: &Store,
    red: &Redactor,
) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "# SQLDiff review session\n");
    let _ = writeln!(
        o,
        "- sqldiff {} · read {}{}",
        info.tool_version,
        info.loaded_at,
        if red.on { " · redacted" } else { "" }
    );
    let layers: Vec<String> = info
        .layers
        .iter()
        .map(|l| {
            let name = if red.on {
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
                a.id,
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
                f.rule,
                f.fingerprint,
                if at.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", at.join(" ↔ "))
                },
                red.apply(&f.witness)
            );
            if let Some((d, current)) = store.lookup(&f.fingerprint, snap) {
                let _ = write!(
                    o,
                    " — {}{}",
                    format!("{:?}", d.state).to_lowercase(),
                    if current { "" } else { " (earlier snapshot)" }
                );
                if let Some(c) = &d.comment {
                    let _ = write!(o, ": “{}”", red.apply(c));
                }
            }
            let _ = writeln!(o);
        }
    }
    o
}
