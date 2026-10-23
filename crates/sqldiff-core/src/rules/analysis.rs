//! Rule lints and near misses as findings.

use sqldiff_syntax::ast as cst;

use super::pattern::{distance, size};
use super::print::Printer;
use super::rewrite::{Engine, Facts, Log, NearMiss};
use super::rule::RuleSet;
use super::{sexp, surface};
use crate::artifact::Analyzed;
use crate::equiv::term::Term;
use crate::model::{fingerprint, ChangeClass, Finding, FingerprintInput, Severity, Side, Verdict};

#[derive(Debug, Clone)]
pub struct Hit {
    pub rule: String,
    pub class: ChangeClass,
    pub severity: Severity,
    pub message: String,
    /// Normalised matched term, the fingerprint anchor.
    pub key: String,
    pub term: Term,
    pub start: usize,
    pub end: usize,
    /// New in the second version of a same-dialect diff: the pair differs.
    pub diff_different: bool,
}

fn show(a: &Analyzed, t: &Term) -> String {
    let vars = |v: &str| v.to_string();
    let p = Printer::new(a.dialect(), &vars, None);
    p.expr(t)
        .or_else(|_| p.stmt(t, 0))
        .unwrap_or_else(|_| sexp::write(t))
}

fn near_message(a: &Analyzed, m: &NearMiss) -> String {
    let parts: Vec<String> = m
        .conflicts
        .iter()
        .map(|c| {
            format!(
                "{} is `{}` in one place and `{}` in another",
                c.var.trim_start_matches('?'),
                show(a, &c.first),
                show(a, &c.second)
            )
        })
        .collect();
    format!(
        "Almost the `{}` idiom: {} (edit distance {}). If they are meant to be the same, the statements do not do what the idiom does.",
        m.rule,
        parts.join("; "),
        m.distance
    )
}

/// Lint hits of the rules, and near misses when `near` is set.
pub fn hits(a: &Analyzed, facts: &dyn Facts, near: bool) -> Vec<Hit> {
    let rules = RuleSet::builtin();
    let engine = Engine::new(rules, facts);
    let d = a.dialect();
    let units = surface::units(&surface::body_units(&a.root), d);
    let terms: Vec<Term> = units.iter().map(|u| u.term.clone()).collect();
    let range = |i: usize, len: usize| {
        (
            cst::sig_range(&units[i].node).0,
            cst::sig_range(&units[i + len - 1].node).1,
        )
    };
    let mut out: Vec<Hit> = Vec::new();
    for (i, u) in units.iter().enumerate() {
        let mut log = Log::default();
        let lifted = engine
            .lift(&u.term, d, &mut log)
            .unwrap_or_else(|_| u.term.clone());
        let mut cands = vec![(u.term.clone(), range(i, 1)), (lifted, range(i, 1))];
        // Sequence idioms over consecutive statements of the body.
        if let Ok(Some((len, c))) = engine.lift_seq(&terms[i..], d, &mut Log::default()) {
            if len > 1 {
                let c = engine.lift(&c, d, &mut Log::default()).unwrap_or(c);
                cands.push((c, range(i, len)));
            }
        }
        for (t, (start, end)) in cands {
            for (r, m) in engine.lints(&t, d) {
                let spec = r.lint.as_ref().unwrap();
                let key = sexp::write(&m);
                // The unit and its lifted form can both match; other units are other hits.
                if out
                    .iter()
                    .any(|h| h.rule == r.id && h.key == key && h.start == start)
                {
                    continue;
                }
                out.push(Hit {
                    rule: r.id.clone(),
                    class: spec.class,
                    severity: spec.severity,
                    message: spec.message.clone(),
                    key,
                    term: m,
                    start,
                    end,
                    diff_different: spec.diff_different,
                });
            }
        }
        if near {
            if let Some(m) = engine.near_misses(&u.term, d).into_iter().next() {
                let (start, end) = range(i, 1);
                out.push(near_hit(a, &m, start, end));
            }
        }
    }
    if near && !terms.is_empty() {
        // Sequence near misses over consecutive top-level statements.
        let body = Term::N("block".into(), terms.clone());
        for m in engine.near_misses(&body, d) {
            let Some(i) = top_window(&terms, &m.at) else {
                continue;
            };
            let (start, end) = range(i, m.at.kids().len());
            if !out
                .iter()
                .any(|h| h.start == start && h.rule.ends_with(&m.rule))
            {
                out.push(near_hit(a, &m, start, end));
            }
        }
    }
    out
}

/// Where a `(seq ...)` near miss starts among the top-level statements.
fn top_window(terms: &[Term], at: &Term) -> Option<usize> {
    let k = at.kids();
    if !at.is("seq") || k.is_empty() || k.len() > terms.len() {
        return None;
    }
    (0..=terms.len() - k.len()).find(|&i| terms[i..i + k.len()] == *k)
}

fn near_hit(a: &Analyzed, m: &NearMiss, start: usize, end: usize) -> Hit {
    Hit {
        rule: format!("idiom.near-miss/{}", m.rule),
        class: ChangeClass::Semantic,
        severity: Severity::Low,
        message: near_message(a, m),
        key: sexp::write(&m.at),
        term: m.at.clone(),
        start,
        end,
        diff_different: false,
    }
}

/// Hits of `new` that `old` does not have. Each old hit accounts for one new
/// hit of the same rule: the same term first, then a close one, so an edit
/// elsewhere in a flagged statement does not report its hazard again, while
/// a second copy of a flagged statement does.
pub fn new_hits(old: &[Hit], new: Vec<Hit>) -> Vec<Hit> {
    let mut used = vec![false; old.len()];
    let mut left = Vec::new();
    for h in new {
        match (0..old.len()).find(|&i| !used[i] && old[i].rule == h.rule && old[i].key == h.key) {
            Some(i) => used[i] = true,
            None => left.push(h),
        }
    }
    let mut out = Vec::new();
    for h in left {
        let close = (0..old.len())
            .filter(|&i| !used[i] && old[i].rule == h.rule)
            .map(|i| (distance(&old[i].term, &h.term), i))
            .filter(|&(d, i)| d * 4 <= size(&old[i].term).max(size(&h.term)))
            .min();
        match close {
            Some((_, i)) => used[i] = true,
            None => out.push(h),
        }
    }
    out
}

pub fn to_finding(a: &Analyzed, h: &Hit, verdict: Verdict) -> Finding {
    let loc = a.location(h.start, h.end);
    let (line, _) = a.lines.line_col(h.start);
    Finding {
        fingerprint: fingerprint(&FingerprintInput {
            object: &a.object,
            class: h.class,
            rule: &h.rule,
            anchor_a: if a.side == Side::A { &h.key } else { "" },
            anchor_b: if a.side == Side::B { &h.key } else { "" },
        }),
        rule: format!("rules.{}", h.rule),
        class: h.class,
        severity: h.severity,
        verdict,
        witness: format!("Side {:?} line {line}: {}", a.side, h.message),
        a: (a.side == Side::A).then(|| loc.clone()),
        b: (a.side == Side::B).then_some(loc),
        assumptions: Vec::new(),
    }
}
