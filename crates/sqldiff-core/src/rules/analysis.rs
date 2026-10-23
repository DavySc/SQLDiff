//! Rule lints and near misses as findings.

use sqldiff_syntax::ast as cst;

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
    let mut out: Vec<Hit> = Vec::new();
    for u in surface::units(&surface::body_units(&a.root), d) {
        let (start, end) = cst::sig_range(&u.node);
        let mut log = Log::default();
        let lifted = engine
            .lift(&u.term, d, &mut log)
            .unwrap_or_else(|_| u.term.clone());
        for t in [&u.term, &lifted] {
            for (r, m) in engine.lints(t, d) {
                let spec = r.lint.as_ref().unwrap();
                let key = sexp::write(&m);
                if out.iter().any(|h| h.rule == r.id && h.key == key) {
                    continue;
                }
                out.push(Hit {
                    rule: r.id.clone(),
                    class: spec.class,
                    severity: spec.severity,
                    message: spec.message.clone(),
                    key,
                    start,
                    end,
                    diff_different: spec.diff_different,
                });
            }
        }
        if near {
            if let Some(m) = engine.near_misses(&u.term, d).into_iter().next() {
                out.push(Hit {
                    rule: format!("idiom.near-miss/{}", m.rule),
                    class: ChangeClass::Semantic,
                    severity: Severity::Low,
                    message: near_message(a, &m),
                    key: sexp::write(&m.at),
                    start,
                    end,
                    diff_different: false,
                });
            }
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
