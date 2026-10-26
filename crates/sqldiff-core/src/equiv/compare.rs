//! Comparison of canonical terms: equal (possibly under assumptions), a
//! difference with a concrete witness, or unexplained (unknown).

use super::canon::asm;
use super::term::Term;
use crate::model::{Assumption, ChangeClass};

/// A pair of subterms that differ, with their parents for context.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub a: Term,
    pub b: Term,
    pub parent_a: Option<Term>,
    pub parent_b: Option<Term>,
    pub index: usize,
}

#[derive(Debug, Default)]
pub struct Cmp {
    pub mismatches: Vec<Mismatch>,
    pub assumptions: Vec<Assumption>,
}

pub fn compare(a: &Term, b: &Term) -> Cmp {
    let mut c = Cmp::default();
    walk(a, b, None, None, 0, &mut c);
    c
}

fn assume(c: &mut Cmp, x: Assumption) {
    if !c.assumptions.iter().any(|y| y.id == x.id) {
        c.assumptions.push(x);
    }
}

fn walk(a: &Term, b: &Term, pa: Option<&Term>, pb: Option<&Term>, index: usize, c: &mut Cmp) {
    if a == b {
        return;
    }
    // Hooks that make different spellings equal under an assumption.
    if let Some(x) = hook(a, b).or_else(|| hook(b, a)) {
        assume(c, x);
        return;
    }
    match (a, b) {
        (Term::N(ha, ka), Term::N(hb, kb)) if ha == hb && ka.len() == kb.len() => {
            // Scalar contracts: two silent picks of an arbitrary row agree when
            // at most one row qualifies.
            if ha == "scalar" {
                let m = (ka[1].atom(), kb[1].atom());
                let unknown_keys =
                    m.0.is_some_and(|x| x.ends_with('?')) || m.1.is_some_and(|x| x.ends_with('?'));
                let base = |x: Option<&str>| x.map(|s| s.trim_end_matches('?').to_string());
                let silent = |x: &Option<String>| matches!(x.as_deref(), Some("last" | "first"));
                let (ba, bb) = (base(m.0), base(m.1));
                let _ = unknown_keys;
                if ba != bb && silent(&ba) && silent(&bb) {
                    assume(c, asm::make(asm::SINGLE_ROW));
                    walk(&ka[0], &kb[0], Some(a), Some(b), 0, c);
                    walk(&ka[2], &kb[2], Some(a), Some(b), 2, c);
                    return;
                }
            }
            for (i, (x, y)) in ka.iter().zip(kb).enumerate() {
                walk(x, y, Some(a), Some(b), i, c);
            }
        }
        _ => c.mismatches.push(Mismatch {
            a: a.clone(),
            b: b.clone(),
            parent_a: pa.cloned(),
            parent_b: pb.cloned(),
            index,
        }),
    }
}

fn hook(a: &Term, b: &Term) -> Option<Assumption> {
    // identity(t) vs col(t, c): SCOPE_IDENTITY() returns the identity column.
    if a.is("identity") && b.is("col") && a.kid(0) == b.kid(0) {
        let t = a.kid(0)?.atom()?;
        let col = b.kid(1)?.atom()?;
        return Some(Assumption::default(
            &format!("ASM-IDENTITY-{}", t.to_uppercase()),
            &format!("{t}.{col} is the identity column of {t}"),
        ));
    }
    None
}

/// Plain-language explanation of a mismatch that is a real behavioural
/// difference, from the side names' point of view.
pub fn explain(
    m: &Mismatch,
    side_a: &str,
    side_b: &str,
) -> Option<(ChangeClass, &'static str, String)> {
    let parent = m.parent_a.as_ref().map(Term::head).unwrap_or("");
    let (x, y) = (m.a.atom(), m.b.atom());
    match (parent, m.index) {
        ("strcmp", 1) => {
            let (ci, cs) = match (x?, y?) {
                ("ci", "cs") => (side_a, side_b),
                ("cs", "ci") => (side_b, side_a),
                _ => return None,
            };
            let what = operands(m.parent_a.as_ref()?);
            Some((
                ChangeClass::Semantic,
                "equiv.case-sensitivity",
                format!(
                    "For values that differ only in letter case (for example 'A@example.com' and 'a@example.com'), {ci} finds {what} equal and {cs} does not."
                ),
            ))
        }
        ("strcmp", 2) => {
            let (pad, nopad) = match (x?, y?) {
                ("pad", "nopad") => (side_a, side_b),
                ("nopad", "pad") => (side_b, side_a),
                _ => return None,
            };
            let what = operands(m.parent_a.as_ref()?);
            Some((
                ChangeClass::Semantic,
                "equiv.trailing-spaces",
                format!(
                    "For values that differ only in trailing spaces (for example 'a@example.com ' and 'a@example.com'), {pad} finds {what} equal (blank-padded comparison) and {nopad} does not."
                ),
            ))
        }
        ("ord", 2) => {
            let (first, last) = match (x?, y?) {
                ("nf", "nl") => (side_a, side_b),
                ("nl", "nf") => (side_b, side_a),
                _ => return None,
            };
            let key = render(m.parent_a.as_ref()?.kid(0)?);
            Some((
                ChangeClass::Semantic,
                "equiv.null-ordering",
                format!("Rows where {key} is NULL sort first on {first} and last on {last}, so the order (and any page of it) differs."),
            ))
        }
        ("scalar", 1) if x?.ends_with('?') || y?.ends_with('?') => None,
        ("scalar", 1) => {
            let (x, y) = (x?.trim_end_matches('?'), y?.trim_end_matches('?'));
            let desc = |s: &str| match s {
                "n/a" => "reads at most one row (a unique key is matched)",
                "last" => "keeps the last row silently",
                "first" => "keeps the first row silently",
                "error" => "raises a cardinality error",
                _ => "behaves differently",
            };
            Some((
                ChangeClass::Cardinality,
                "equiv.cardinality",
                if x == "n/a" || y == "n/a" {
                    let (one, many, how) = if x == "n/a" {
                        (side_a, side_b, y)
                    } else {
                        (side_b, side_a, x)
                    };
                    format!(
                        "{one} {}; on {many} several rows can qualify, and it {}.",
                        desc("n/a"),
                        desc(how)
                    )
                } else {
                    format!(
                        "If several rows qualify, {side_a} {} and {side_b} {}.",
                        desc(x),
                        desc(y)
                    )
                },
            ))
        }
        ("scalar", 2) => {
            let desc = |s: &str| match s {
                "null" => "sets NULL",
                "unchanged" => "leaves the previous value",
                "error" => "raises a no-data error",
                _ => "behaves differently",
            };
            Some((
                ChangeClass::Cardinality,
                "equiv.no-rows",
                format!(
                    "If no row qualifies, {side_a} {} and {side_b} {}.",
                    desc(x?),
                    desc(y?)
                ),
            ))
        }
        ("time", 0) => {
            let desc = |s: &str| match s {
                "stmt" => "the statement's start time",
                "txn" => "the transaction's start time",
                "clock" => "the current clock time, which changes while the statement runs",
                "stmt-datetime" => "the statement time in datetime precision (1/300 s)",
                _ => "another time source",
            };
            Some((
                ChangeClass::Semantic,
                "equiv.time-source",
                format!(
                    "{side_a} uses {} and {side_b} uses {}, so the stored times differ when the transaction or statement runs for a while.",
                    desc(x?),
                    desc(y?)
                ),
            ))
        }
        ("concurrency", 0) => {
            let (safe, race) = match (x?, y?) {
                ("safe", "race") => (side_a, side_b),
                ("race", "safe") => (side_b, side_a),
                _ => return None,
            };
            Some((
                ChangeClass::Concurrency,
                "equiv.upsert-race",
                format!(
                    "If two sessions upsert the same new key at the same time, {race} can try to insert it twice and one session fails with a duplicate-key error; {safe} serializes them."
                ),
            ))
        }
        _ => explain_terms(&m.a, &m.b, side_a, side_b)
            .or_else(|| explain_terms(&m.b, &m.a, side_b, side_a)),
    }
}

/// Why an unexplained mismatch could not be decided, when there is a
/// specific reason; otherwise None.
pub fn describe_unknown(m: &Mismatch, side_a: &str, side_b: &str) -> Option<String> {
    let parent = m.parent_a.as_ref()?;
    // A string literal whose conversion to the compared type is not modelled
    // on one side: it may round, fail or depend on session settings.
    for (t, side) in [(&m.a, side_a), (&m.b, side_b)] {
        if t.is("lit-conv") {
            let ty = t.kid(1).and_then(Term::atom).unwrap_or("?");
            let lit = t.kid(2).and_then(Term::atom).unwrap_or("");
            return Some(if ty == "?" {
                format!("on {side}, the type the literal '{lit}' converts to is unknown")
            } else {
                format!(
                    "on {side}, the literal '{lit}' converts to {ty} in a way that is not modelled for certain (it may round, fail or depend on session settings)"
                )
            });
        }
    }
    if parent.is("cmp-untyped") && m.index == 0 {
        let x = parent.kid(2).map(render).unwrap_or_default();
        let y = parent.kid(3).map(render).unwrap_or_default();
        return Some(format!(
            "the type of the value compared in {x} {} {y} is unknown (no catalog facts): a character string compares under each side's collation, any other type converts the literal",
            parent.kid(1).and_then(Term::atom).unwrap_or("=")
        ));
    }
    if parent.is("cmp-conv") && m.index == 0 {
        return Some(
            "a character string is compared with a value of another type: T-SQL converts the string, PostgreSQL has no such comparison for a typed string, and Db2 for i is not checked".to_string(),
        );
    }
    let (x, y) = (m.a.atom()?, m.b.atom()?);
    match (parent.head(), m.index) {
        ("ord", 2) => Some(format!(
            "whether {} can be NULL is unknown (no catalog facts); NULLs sort {} on {side_a} and {} on {side_b}",
            render(parent.kid(0)?),
            if x.starts_with("nf") { "first" } else { "last" },
            if y.starts_with("nf") { "first" } else { "last" },
        )),
        ("scalar", 1) => Some(format!(
            "whether the query can return several rows is unknown (no catalog facts for its table); then {side_a} {} and {side_b} {}",
            many(x),
            many(y)
        )),
        _ => None,
    }
}

fn many(s: &str) -> &'static str {
    match s.trim_end_matches('?') {
        "n/a" => "reads at most one row",
        "last" => "keeps the last row",
        "first" => "keeps the first row",
        "error" => "raises a cardinality error",
        _ => "behaves differently",
    }
}

fn explain_terms(
    x: &Term,
    y: &Term,
    sx: &str,
    sy: &str,
) -> Option<(ChangeClass, &'static str, String)> {
    // Day boundaries crossed vs whole elapsed days.
    if x.is("daydiff")
        && y.any(&|t| t.is("extract") && t.kid(0).and_then(Term::atom) == Some("DAY"))
    {
        let from = render(x.kid(0)?);
        let to = render(x.kid(1)?);
        return Some((
            ChangeClass::Semantic,
            "equiv.date-difference",
            format!(
                "For {from} = 2026-01-01 23:00 and {to} = 2026-01-02 01:00, {sx} returns 1 (midnight boundaries crossed) and {sy} returns 0 (whole elapsed days)."
            ),
        ));
    }
    // ISNULL truncating the replacement to the first argument's type.
    if x.is("isnull-narrow") && y.is("fn") && y.kid(0).and_then(Term::atom) == Some("coalesce") {
        let first = render(x.kid(1)?);
        let second = render(x.kid(2)?);
        return Some((
            ChangeClass::Semantic,
            "equiv.isnull-type",
            format!(
                "When {first} is NULL and {second} is longer than {first}'s type, ISNULL on {sx} converts it to that type and truncates it; COALESCE on {sy} returns it whole."
            ),
        ));
    }
    None
}

fn operands(strcmp: &Term) -> String {
    match (strcmp.kid(3), strcmp.kid(4)) {
        (Some(x), Some(y)) => format!("{} and {}", render(x), render(y)),
        _ => "the compared values".into(),
    }
}

/// Short SQL-like rendering of a term for witnesses and messages.
pub fn render(t: &Term) -> String {
    let s = render_inner(t);
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

fn render_inner(t: &Term) -> String {
    let k = t.kids();
    let r = |i: usize| k.get(i).map(render_inner).unwrap_or_default();
    match t.head() {
        "col" => {
            let tab = k.first().and_then(Term::atom).unwrap_or("");
            let col = k.get(1).and_then(Term::atom).unwrap_or("");
            if tab == "?" || tab.is_empty() || tab.starts_with("merge:") {
                col.to_string()
            } else {
                format!("{tab}.{col}")
            }
        }
        "sym" => {
            let s = r(0);
            match s.split_at(1) {
                ("p", i) => format!("parameter {}", i.trim_start_matches(':')),
                ("l", i) => format!("local variable {}", i.trim_start_matches(':')),
                _ => s,
            }
        }
        "num" => r(0),
        "str" => format!("'{}'", r(0)),
        "tval" | "uuid" | "lit-conv" => format!("'{}'", r(k.len().saturating_sub(1))),
        "fn" => format!(
            "{}({})",
            r(0),
            k.iter()
                .skip(1)
                .map(render_inner)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "dfn" => format!(
            "{}({})",
            r(1),
            k.iter()
                .skip(2)
                .map(render_inner)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "concat" => format!("{} || {}", r(0), r(1)),
        "cmp" => format!("{} {} {}", r(1), r(0), r(2)),
        "strcmp" => format!("{} {} {}", r(3), r(0), r(4)),
        "daydiff" => format!("days between {} and {}", r(0), r(1)),
        "time" => format!("time({})", r(0)),
        _ => t.to_string(),
    }
}
