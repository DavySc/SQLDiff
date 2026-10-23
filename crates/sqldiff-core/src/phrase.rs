//! Deterministic phrase grammar for review comments (design sections 6.1
//! and 11, Tier 0).
//!
//! A comment is built from the finding's structure only (class, verdict,
//! witness, locations, assumptions), so the same findings always give the
//! same text and no model is involved:
//!
//! ```text
//! comment := intro group+
//! group   := [pair line] item+
//! item    := n ". " lead " " witness where* relies? ask? ref?
//! lead    := subject(class) predicate(verdict) "."
//! where   := "A|B: path line n: `excerpt`"
//! relies  := "This holds only if" | "Assumed" ": " assumption-text ("; " ...)* "."
//! ask     := request(class) when the verdict is different or unknown
//! ref     := "(sqldiff <fingerprint>)"
//! ```

use std::fmt::Write;

use crate::model::{Assumption, ChangeClass, Finding, Location, Verdict};

/// How a comment is rendered.
#[derive(Debug, Clone)]
pub struct Style {
    /// Excerpt lines quoted per location; 0 quotes none.
    pub excerpt_lines: usize,
    /// End each item with its fingerprint, so a reply can cite it.
    pub fingerprints: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            excerpt_lines: 1,
            fingerprints: true,
        }
    }
}

/// Findings of one pair, with the assumptions their ids refer to.
pub struct Group<'a> {
    /// Shown above the items when the comment covers several pairs.
    pub label: String,
    pub findings: Vec<&'a Finding>,
    pub assumptions: &'a [Assumption],
    /// Assumption ids a reviewer denied; findings relying on them say so.
    pub denied: &'a [String],
}

fn subject(c: ChangeClass) -> &'static str {
    match c {
        ChangeClass::Transactional => "Transaction behaviour",
        ChangeClass::Concurrency => "Locking behaviour",
        ChangeClass::Cardinality => "The number of rows read or written",
        ChangeClass::Semantic => "Behaviour",
        ChangeClass::Binding => "Name resolution",
        ChangeClass::Structural => "Structure",
        ChangeClass::Cosmetic => "Layout",
        ChangeClass::Residue | ChangeClass::Limit => "",
    }
}

/// The opening sentence of an item.
pub fn lead(f: &Finding) -> String {
    match f.class {
        ChangeClass::Residue => "Syntax from another dialect is left in the code.".to_string(),
        ChangeClass::Limit => "A configured limit is exceeded.".to_string(),
        c => {
            let predicate = match f.verdict {
                Verdict::Different => "differs",
                Verdict::Unknown => "could not be shown to match",
                Verdict::EqualUnderAssumptions => "matches only under assumptions",
                Verdict::Equal => "matches",
            };
            format!("{} {predicate}.", subject(c))
        }
    }
}

fn ask(f: &Finding) -> Option<&'static str> {
    if !matches!(f.verdict, Verdict::Different | Verdict::Unknown) {
        return None;
    }
    Some(match f.class {
        ChangeClass::Transactional => {
            "Please check that a failure part-way through leaves the same data committed on both sides."
        }
        ChangeClass::Concurrency => "Please check that concurrent callers see the same locking.",
        ChangeClass::Cardinality => {
            "Please check how many rows this can read or write on each side."
        }
        ChangeClass::Semantic => "Please confirm the new behaviour is intended.",
        ChangeClass::Binding => "Please check which object each name refers to.",
        ChangeClass::Residue => "Please replace it with the target dialect's equivalent.",
        ChangeClass::Limit => "Please check it against the target's limits.",
        ChangeClass::Structural | ChangeClass::Cosmetic => return None,
    })
}

fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.ends_with(['.', '!', '?']) {
        s.to_string()
    } else {
        format!("{s}.")
    }
}

fn where_line(loc: &Location, lines: usize) -> String {
    let side = match loc.side {
        crate::model::Side::A => "A",
        crate::model::Side::B => "B",
    };
    let mut s = format!("{side}: {} line {}", loc.path, loc.start_line);
    let quoted: Vec<&str> = loc
        .excerpt
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(lines)
        .collect();
    if !quoted.is_empty() {
        let _ = write!(s, ": `{}`", quoted.join(" ").replace('`', "'"));
    }
    s
}

/// One numbered item.
pub fn item(
    n: usize,
    f: &Finding,
    assumptions: &[Assumption],
    denied: &[String],
    style: &Style,
) -> String {
    let mut out = format!("{n}. {} {}", lead(f), sentence(&f.witness));
    for loc in [&f.a, &f.b].into_iter().flatten() {
        let _ = write!(out, "\n   {}", where_line(loc, style.excerpt_lines));
    }
    let text_of = |id: &String| {
        assumptions
            .iter()
            .find(|a| &a.id == id)
            .map(|a| a.text.trim_end_matches('.').to_string())
            .unwrap_or_else(|| id.clone())
    };
    let (no, yes): (Vec<&String>, Vec<&String>) =
        f.assumptions.iter().partition(|id| denied.contains(id));
    if !no.is_empty() {
        let texts: Vec<String> = no.into_iter().map(text_of).collect();
        let _ = write!(
            out,
            "\n   The witness relies on what review says does not hold: {}.",
            texts.join("; ")
        );
    }
    let texts: Vec<String> = yes.into_iter().map(text_of).collect();
    if !texts.is_empty() {
        let label = if f.verdict == Verdict::Different {
            "Assumed"
        } else {
            "This holds only if"
        };
        let _ = write!(out, "\n   {label}: {}.", texts.join("; "));
    }
    if let Some(a) = ask(f) {
        let _ = write!(out, "\n   {a}");
    }
    if style.fingerprints {
        let _ = write!(out, " (sqldiff {})", f.fingerprint);
    }
    out
}

/// Most severe first; ties by class, then fingerprint, so order is stable.
fn ordered<'a>(fs: &[&'a Finding]) -> Vec<&'a Finding> {
    let mut v = fs.to_vec();
    v.sort_by(|x, y| {
        y.severity
            .cmp(&x.severity)
            .then(y.class.cmp(&x.class))
            .then(x.fingerprint.cmp(&y.fingerprint))
    });
    v.dedup_by(|x, y| x.fingerprint == y.fingerprint);
    v
}

/// A plain-text review comment for findings of one or more pairs.
pub fn comment(groups: &[Group<'_>], style: &Style) -> String {
    let groups: Vec<&Group> = groups.iter().filter(|g| !g.findings.is_empty()).collect();
    let total: usize = groups.iter().map(|g| ordered(&g.findings).len()).sum();
    if total == 0 {
        return "SQLDiff found nothing to raise.\n".to_string();
    }
    let mut out = format!(
        "SQLDiff found {total} issue{} to review.\n",
        if total == 1 { "" } else { "s" }
    );
    let mut n = 0;
    for g in &groups {
        out.push('\n');
        if groups.len() > 1 {
            let _ = writeln!(out, "{}:", g.label);
        }
        for f in ordered(&g.findings) {
            n += 1;
            let _ = writeln!(out, "{}", item(n, f, g.assumptions, g.denied, style));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssumptionSource, Severity, Side};

    fn finding(fp: &str, class: ChangeClass, verdict: Verdict, asm: &[&str]) -> Finding {
        Finding {
            fingerprint: fp.to_string(),
            rule: "r".to_string(),
            class,
            severity: class.default_severity(),
            verdict,
            witness: "If X fails, side A rolls back and side B keeps the UPDATE".to_string(),
            a: Some(Location {
                side: Side::A,
                path: "a.sql".to_string(),
                start_line: 4,
                start_col: 1,
                end_line: 4,
                end_col: 9,
                byte_start: 0,
                byte_end: 0,
                excerpt: vec!["  COMMIT;".to_string(), "END".to_string()],
            }),
            b: None,
            assumptions: asm.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn grammar_is_deterministic_and_ordered() {
        let asm = vec![Assumption {
            id: "ASM-X".to_string(),
            text: "The caller's XACT_ABORT setting is OFF".to_string(),
            source: AssumptionSource::Default,
        }];
        let f1 = finding("bb", ChangeClass::Semantic, Verdict::Unknown, &[]);
        let f2 = finding(
            "aa",
            ChangeClass::Transactional,
            Verdict::Different,
            &["ASM-X"],
        );
        let g = |fs: Vec<&'static Finding>| Group {
            label: "a.sql ↔ b.sql".to_string(),
            findings: fs,
            assumptions: Box::leak(asm.clone().into_boxed_slice()),
            denied: &[],
        };
        let f1: &'static Finding = Box::leak(Box::new(f1));
        let f2: &'static Finding = Box::leak(Box::new(f2));
        let one = comment(&[g(vec![f1, f2])], &Style::default());
        let two = comment(&[g(vec![f2, f1, f2])], &Style::default());
        assert_eq!(one, two);
        assert_eq!(
            one,
            "SQLDiff found 2 issues to review.\n\n\
1. Transaction behaviour differs. If X fails, side A rolls back and side B keeps the UPDATE.\n   \
A: a.sql line 4: `COMMIT;`\n   \
Assumed: The caller's XACT_ABORT setting is OFF.\n   \
Please check that a failure part-way through leaves the same data committed on both sides. (sqldiff aa)\n\
2. Behaviour could not be shown to match. If X fails, side A rolls back and side B keeps the UPDATE.\n   \
A: a.sql line 4: `COMMIT;`\n   \
Please confirm the new behaviour is intended. (sqldiff bb)\n"
        );
        assert_eq!(f1.severity, Severity::Medium);
    }

    #[test]
    fn empty_comment() {
        assert_eq!(
            comment(&[], &Style::default()),
            "SQLDiff found nothing to raise.\n"
        );
    }
}
