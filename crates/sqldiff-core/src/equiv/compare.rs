//! Comparison of canonical terms: equal (possibly under assumptions), a
//! difference with a concrete witness, or unexplained (unknown).

use super::canon::asm;
use sqldiff_syntax::Dialect;

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
    /// Steps that hold one way only: B refines A here, A does not refine B
    /// (decision 1). With no mismatch the pair is equal but not equivalent.
    pub refinements: Vec<String>,
    /// T-SQL against another dialect in a conversion check: string equality
    /// that differs only in the collation is equal under `ASM-CI-COLLATION`.
    ci_collation: bool,
    /// T-SQL against PostgreSQL in a conversion check, and whether side A is
    /// T-SQL: T-SQL's end of time against PostgreSQL's 6-digit one is equal
    /// under `ASM-END-OF-TIME`.
    fraction_tsql_a: Option<bool>,
}

thread_local! {
    static CI_COLLATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the diff running on this thread checks a conversion that keeps
/// the target's text types (Davy, 2026-10-29; diffs report the difference
/// instead): then
/// T-SQL's case-insensitive string equality against the other side's
/// case-sensitive one is equal under `ASM-CI-COLLATION`, statement by
/// statement, instead of a difference.
pub fn ci_collation() -> bool {
    CI_COLLATION.with(|c| c.get())
}

/// Sets [`ci_collation`] for this thread until the guard is dropped (a
/// conversion check sets it around the diffs it runs).
pub fn ci_collation_scope(on: bool) -> CiCollationScope {
    CiCollationScope(CI_COLLATION.with(|c| c.replace(on)))
}

pub struct CiCollationScope(bool);

impl Drop for CiCollationScope {
    fn drop(&mut self) {
        CI_COLLATION.with(|c| c.set(self.0));
    }
}

pub fn compare(a: &Term, b: &Term) -> Cmp {
    let mut c = Cmp::default();
    walk(a, b, None, None, 0, &mut c);
    c
}

/// [`compare`] for statements of dialects `da` and `db`.
pub fn compare_across(a: &Term, b: &Term, da: Dialect, db: Dialect) -> Cmp {
    let conversion = ci_collation();
    let mut c = Cmp {
        ci_collation: conversion && da != db && (da == Dialect::Tsql || db == Dialect::Tsql),
        fraction_tsql_a: match (da, db) {
            (Dialect::Tsql, Dialect::Postgres) if conversion => Some(true),
            (Dialect::Postgres, Dialect::Tsql) if conversion => Some(false),
            _ => None,
        },
        ..Cmp::default()
    };
    walk(a, b, None, None, 0, &mut c);
    c
}

/// Kid positions of `a` and `b` (same head, same arity) that hold only the
/// collation of an equality: case sensitivity and trailing-space handling
/// of `=` / `<>` (`strcmp`), or the dialect whose collation decides
/// `IN` membership (`coll-eq`), LIKE on varchar with a plain literal
/// pattern (`coll-like`), GROUP BY keys (`collate`) and DISTINCT.
/// None when the pair differs elsewhere in those positions.
fn collation_kids(a: &Term, b: &Term) -> Option<&'static [usize]> {
    let dialect = |t: &Term| matches!(t.atom(), Some("tsql" | "postgres" | "db2i"));
    match a.head() {
        "strcmp" if a.kid(0) == b.kid(0) => {
            let ci = |t: &Term| matches!(t.atom(), Some("ci" | "cs"));
            let pad = |t: &Term| matches!(t.atom(), Some("pad" | "nopad" | "pad-mixed"));
            let ok = [(1, ci as fn(&Term) -> bool), (2, pad)]
                .iter()
                .all(|&(i, f)| a.kid(i).zip(b.kid(i)).is_some_and(|(x, y)| f(x) && f(y)));
            ok.then_some(&[1, 2])
        }
        // REPLACE / CHARINDEX against replace / strpos: only the
        // case sensitivity of the substring match.
        "strreplace" | "strpos"
            if a.kid(0).zip(b.kid(0)).is_some_and(|(x, y)| {
                matches!((x.atom(), y.atom()), (Some("ci" | "cs"), Some("ci" | "cs")))
            }) =>
        {
            Some(&[0])
        }
        "coll-eq" | "coll-like" | "collate" | "distinct"
            if a.kid(0)
                .zip(b.kid(0))
                .is_some_and(|(x, y)| dialect(x) && dialect(y)) =>
        {
            Some(&[0])
        }
        _ => None,
    }
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
    // Result column names (decision 1): exact within a dialect, up to letter
    // case across dialects under ASM-RESULT-NAMES-CI; the kind of name
    // (alias, column, declared) is not observed.
    if a.is("colname") && b.is("colname") {
        let (na, nb) = (a.kid(2).and_then(Term::atom), b.kid(2).and_then(Term::atom));
        if na.is_some() && na == nb {
            return;
        }
        let cross = a.kid(0) != b.kid(0);
        if cross && na.zip(nb).is_some_and(|(x, y)| x.eq_ignore_ascii_case(y)) {
            assume(c, asm::make(asm::RESULT_NAMES_CI));
            return;
        }
        c.mismatches.push(Mismatch {
            a: a.clone(),
            b: b.clone(),
            parent_a: pa.cloned(),
            parent_b: pb.cloned(),
            index,
        });
        return;
    }
    // Row counts and offsets that are not literals (canon `row_count`):
    // integers on both sides agree across dialects when never NULL.
    if a.is("count")
        && b.is("count")
        && a.kid(0) != b.kid(0)
        && a.kid(2) == b.kid(2)
        && a.kid(1).and_then(Term::atom) == Some("int")
        && b.kid(1).and_then(Term::atom) == Some("int")
    {
        assume(c, asm::make(asm::PAGE_COUNT_NOT_NULL));
        return;
    }
    // T-SQL's end of time against the 6-digit one the converter writes for
    // PostgreSQL, which would round the 7-digit value to 10000-01-01.
    if let Some(tsql_a) = c.fraction_tsql_a {
        let (t, p) = if tsql_a { (a, b) } else { (b, a) };
        if end_of_time(t, p) {
            assume(c, asm::make(asm::END_OF_TIME));
            return;
        }
    }
    // Hooks that make different spellings equal under an assumption.
    if let Some(x) = hook(a, b).or_else(|| hook(b, a)) {
        assume(c, x);
        return;
    }
    // String equality whose only difference is the collation.
    if c.ci_collation && a.head() == b.head() && a.kids().len() == b.kids().len() {
        if let Some(coll) = collation_kids(a, b) {
            for (i, (x, y)) in a.kids().iter().zip(b.kids()).enumerate() {
                if coll.contains(&i) {
                    if x != y {
                        assume(c, asm::make(asm::CI_COLLATION));
                    }
                } else {
                    walk(x, y, Some(a), Some(b), i, c);
                }
            }
            return;
        }
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

/// Whether `a` (dialect `da`) and `b` (dialect `db`) are T-SQL's end of
/// time and PostgreSQL's 6-digit one, in a conversion check
/// (`ASM-END-OF-TIME`).
pub fn end_of_time_across(a: &Term, b: &Term, da: Dialect, db: Dialect) -> bool {
    ci_collation()
        && match (da, db) {
            (Dialect::Tsql, Dialect::Postgres) => end_of_time(a, b),
            (Dialect::Postgres, Dialect::Tsql) => end_of_time(b, a),
            _ => false,
        }
}

/// `t` is the timestamp 9999-12-31 23:59:59.9999999 and `p` the timestamp
/// 9999-12-31 23:59:59.999999.
fn end_of_time(t: &Term, p: &Term) -> bool {
    let ts = |x: &Term, v: &str| {
        x.is("tval")
            && x.kid(0).and_then(Term::atom) == Some("ts")
            && x.kid(1).and_then(Term::atom) == Some(v)
    };
    ts(t, "9999-12-31 23:59:59.9999999") && ts(p, "9999-12-31 23:59:59.999999")
}

fn hook(a: &Term, b: &Term) -> Option<Assumption> {
    // identity(t) vs col(t, c): SCOPE_IDENTITY() returns the identity column.
    if a.is("identity") && b.is("col") && a.kid(0) == b.kid(0) {
        let t = a.kid(0)?.atom()?;
        let col = b.kid(1)?.atom()?;
        return Some(Assumption::default(
            &format!("ASM-IDENTITY-COLUMN:{t}"),
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
    if m.a.is("colname") && m.b.is("colname") {
        // Only two explicit aliases in one dialect are certainly different
        // names; a column reference's returned name can depend on the catalog.
        let alias = |t: &Term| t.kid(1).and_then(Term::atom) == Some("alias");
        if !(alias(&m.a) && alias(&m.b) && m.a.kid(0) == m.b.kid(0)) {
            return None;
        }
        let name = |t: &Term| t.kid(2).and_then(Term::atom).unwrap_or("").to_string();
        return Some((
            ChangeClass::Semantic,
            "equiv.result-column-name",
            format!(
                "A result column is named `{}` on {side_a} and `{}` on {side_b}; a client that reads it by name does not find it.",
                name(&m.a),
                name(&m.b)
            ),
        ));
    }
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
        ("dcast", 0) => {
            let (pa, pb) = (m.parent_a.as_ref()?, m.parent_b.as_ref()?);
            explain_conv(pa, pb, side_a, side_b).or_else(|| explain_conv(pb, pa, side_b, side_a))
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
                "n/a" => "always reads one row (an aggregate without GROUP BY)",
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
    if let Some(e) = explain_conv(x, y, sx, sy) {
        return Some(e);
    }
    // A PostgreSQL boolean meeting a number: the statement fails there.
    if x.is("type-error") && x.kid(0).and_then(Term::atom) == Some("postgres") {
        let why = match x.kid(1).and_then(Term::atom)? {
            "cmp" | "in" => format!("PostgreSQL has no comparison between boolean and a number (operator does not exist: boolean = integer), while {sy} compares the bit as 0 or 1. Compare the boolean with TRUE or FALSE"),
            "arith" => format!("PostgreSQL has no arithmetic on boolean (operator does not exist: boolean + integer), while {sy} computes with the bit as 0 or 1. Cast the boolean to integer first"),
            "mix" => format!("PostgreSQL cannot give one CASE or COALESCE both boolean and numeric values (types boolean and integer cannot be matched), while {sy} returns the bit as a number"),
            "assign" => format!("PostgreSQL does not store a number in a boolean (column is of type boolean but expression is of type integer), while {sy} stores 0 as 0 and any other number as 1. Write TRUE or FALSE, or (value <> 0)"),
            "cast" => format!("PostgreSQL casts boolean only to integer and text, and only integer to boolean (cannot cast type), while {sy} converts the bit"),
            _ => return None,
        };
        return Some((
            ChangeClass::Semantic,
            "equiv.bool-type-error",
            format!("{sx} fails here: {why}."),
        ));
    }
    // SQL Server rejects arithmetic between two bits.
    if x.is("type-error") && x.kid(0).and_then(Term::atom) == Some("tsql") {
        return Some((
            ChangeClass::Semantic,
            "equiv.bool-type-error",
            format!("{sx} fails here: SQL Server has no arithmetic between two bits (Msg 402 / 8117), while {sy} computes it."),
        ));
    }
    // A boolean written as text: 'true' / 'false' against a bit's '1' / '0'.
    if x.is("booltext")
        && ((y.is("cast") && y.kid(1) == x.kid(1))
            || (y.is("dcast") && y.kid(4) == x.kid(1))
            || (y.is("tostr") && y.kid(0) == x.kid(1)))
    {
        let v = render(x.kid(1)?);
        return Some((
            ChangeClass::Semantic,
            "equiv.bool-text",
            format!("When {v} is set, {sx} writes it as 'true' or 'false' (a boolean as text) and {sy} as '1' or '0' (a bit as text)."),
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

/// A conversion term: `cast` / `conv` (the same in every dialect, `trunc`
/// marking T-SQL's dropped fraction) or `dcast` (one dialect's).
struct ConvView<'a> {
    dialect: Option<&'a str>,
    dst: &'a Term,
    src: Option<TyParts<'a>>,
    v: &'a Term,
    trunc: bool,
}

#[derive(Clone, Copy)]
struct TyParts<'a> {
    fam: &'a str,
    len: Option<u32>,
    scale: Option<u32>,
}

fn ty_parts(t: &Term) -> Option<TyParts<'_>> {
    if !t.is("ty") {
        return None;
    }
    let num = |i: usize| t.kid(i).and_then(Term::atom).and_then(|s| s.parse().ok());
    Some(TyParts {
        fam: t.kid(0)?.atom()?,
        len: num(1),
        scale: num(2),
    })
}

fn conv_view(t: &Term) -> Option<ConvView<'_>> {
    match t.head() {
        "cast" | "conv" => {
            let v = t.kid(1)?;
            let inner = (v.is("fn") && v.kid(0).and_then(Term::atom) == Some("trunc"))
                .then(|| v.kid(1))
                .flatten();
            Some(ConvView {
                dialect: None,
                dst: t.kid(0)?,
                src: None,
                v: inner.unwrap_or(v),
                trunc: inner.is_some(),
            })
        }
        "dcast" => Some(ConvView {
            dialect: t.kid(0)?.atom(),
            dst: t.kid(2)?,
            src: ty_parts(t.kid(3)?),
            v: t.kid(4)?,
            trunc: false,
        }),
        _ => None,
    }
}

/// A conversion that gives different results in T-SQL and PostgreSQL, with
/// a value that shows it (each checked on SQL Server 2022 and PostgreSQL 18,
/// eval/casts). Only for the same operand converted to the same type.
fn explain_conv(
    x: &Term,
    y: &Term,
    sx: &str,
    sy: &str,
) -> Option<(ChangeClass, &'static str, String)> {
    let (a, b) = (conv_view(x)?, conv_view(y)?);
    if a.dst != b.dst {
        return None;
    }
    let dst = ty_parts(a.dst)?;
    let what = render(a.v);
    let fits = |n: u32| dst.len.map_or(true, |l| l >= n);
    let diff = |rule, text: String| Some((ChangeClass::Semantic, rule, text));
    // T-SQL truncates to an integer, PostgreSQL rounds.
    if a.trunc && b.dialect == Some("postgres") && dst.fam == "Int" && a.v == b.v {
        let src = b.src?;
        if let Some(lit) = num_literal(a.v) {
            let (t, r) = (trunc_text(lit)?, round_text(lit)?);
            if t == r {
                return None;
            }
            return diff("equiv.cast-rounding", format!("{sx} converts {lit} to an integer as {t} (it drops the fraction) and {sy} as {r} (it rounds)."));
        }
        let (v, t, r) = match (src.fam, src.len, src.scale) {
            ("Float", _, None) => ("1.5", "1", "2"),
            ("Exact", Some(p), Some(s)) if s > 0 && p > s => ("1.5", "1", "2"),
            ("Exact", Some(_), Some(s)) if s > 0 => ("0.5", "0", "1"),
            _ => return None,
        };
        return diff("equiv.cast-rounding", format!("For {what} = {v}, {sx} converts it to an integer as {t} (it drops the fraction) and {sy} as {r} (it rounds)."));
    }
    let (tsql, pg) = match (a.dialect?, b.dialect?) {
        ("tsql", "postgres") => (&a, &b),
        _ => return None,
    };
    let (src_t, src_p) = (tsql.src?, pg.src?);
    let string = |f: &str| matches!(f, "VarChar" | "Char");
    // Float as text: T-SQL keeps 6 significant digits.
    if string(dst.fam) && src_t.fam == "Float" && fits(12) {
        if let (Some(lt), Some(lp)) = (literal(tsql.v, "flt"), literal(pg.v, "num")) {
            if lt != lp {
                return None;
            }
            let (t, p) = (tsql_float_text(lt.parse().ok()?), pg_numeric_text(lp)?);
            if t == p {
                return None;
            }
            return diff("equiv.float-text", format!("{sx} reads {lt} as float and writes it as '{t}', and {sy} reads it as numeric and writes '{p}'."));
        }
        if src_p.fam == "Float" && tsql.v == pg.v {
            return diff("equiv.float-text", format!("For {what} = 1234567, {sx} writes '1.23457e+006' (float as text keeps 6 significant digits) and {sy} writes '1234567'."));
        }
        return None;
    }
    if tsql.v != pg.v {
        return None;
    }
    // A string converted to a number. Only for a value that is not a
    // literal: a literal converts to one known value (or none) on each side,
    // so '' and '1e3' are no witnesses for it.
    if string(src_t.fam) && string(src_p.fam) {
        if tsql.v.is("str") {
            return None;
        }
        let long = |t: TyParts| t.len.map_or(true, |l| l >= 3);
        return match dst.fam {
            "Int" | "Float" => diff("equiv.cast-string-number", format!("For {what} = '' (an empty string), {sx} converts it to 0 and {sy} fails (invalid input syntax).")),
            "Exact" if long(src_t) && long(src_p) => diff("equiv.cast-string-number", format!("For {what} = '1e3', {sx} fails (error converting to numeric) and {sy} converts it to 1000.")),
            _ => None,
        };
    }
    // Fewer fractional digits.
    let same_src = src_t.fam == src_p.fam && src_t.scale.is_none() && src_p.scale.is_none();
    if same_src && src_t.fam == dst.fam && dst.scale.is_none() {
        let (dl, sl) = (dst.len?, src_t.len?.min(src_p.len?));
        if dl < sl && dst.fam == "Timestamp" {
            let (v, t, p) = if dl == 0 {
                (
                    "1999-12-31 23:59:59.5".to_string(),
                    "2000-01-01 00:00:00".to_string(),
                    "1999-12-31 23:59:59".to_string(),
                )
            } else {
                let z = "0".repeat(dl as usize - 1);
                (
                    format!("1999-12-31 23:59:59.{z}15"),
                    format!("1999-12-31 23:59:59.{z}2"),
                    format!("1999-12-31 23:59:59.{z}1"),
                )
            };
            return diff("equiv.time-rounding", format!("For {what} = '{v}', {sx} rounds it to '{t}' and {sy} to '{p}' (PostgreSQL rounds a half away from 2000-01-01)."));
        }
        if dl == 0 && sl > 0 && dst.fam == "Time" {
            return diff(
                "equiv.time-rounding",
                format!("For {what} = '23:59:59.5', {sx} gives '23:59:59' and {sy} '24:00:00'."),
            );
        }
        return None;
    }
    // Float to a decimal with 2 decimals: PostgreSQL rounds the shortest
    // decimal form, T-SQL the binary value.
    if dst.fam == "Exact"
        && dst.scale == Some(2)
        && dst.len.is_some_and(|p| p >= 3)
        && src_t.fam == "Float"
        && src_p.fam == "Float"
        && src_t.len == Some(53)
        && src_p.len == Some(53)
    {
        return diff(
            "equiv.cast-rounding",
            format!("For {what} = 2.675 (a float), {sx} converts it to 2.67 and {sy} to 2.68."),
        );
    }
    // Times as text.
    if string(dst.fam) && matches!(src_p.fam, "Timestamp" | "Time") && src_t.fam == src_p.fam {
        let pg_text = if src_t.fam == "Time" {
            "03:04:00"
        } else {
            "2026-01-02 03:04:00"
        };
        let t = match (src_t.fam, src_t.len, src_t.scale) {
            ("Timestamp", Some(_), Some(_)) if fits(19) => "Jan  2 2026  3:04AM".to_string(),
            (_, Some(p), None) if p > 0 && fits(pg_text.len() as u32 + 1 + p) => {
                format!("{pg_text}.{}", "0".repeat(p as usize))
            }
            _ => return None,
        };
        return diff(
            "equiv.time-text",
            format!("For {what} = '{pg_text}', {sx} writes '{t}' and {sy} writes '{pg_text}'."),
        );
    }
    None
}

/// The text of a numeric literal term with a decimal point.
fn num_literal(t: &Term) -> Option<&str> {
    literal(t, "num").filter(|s| s.contains('.') && !s.contains('e'))
}

fn literal<'a>(t: &'a Term, head: &str) -> Option<&'a str> {
    t.is(head).then(|| t.kid(0).and_then(Term::atom)).flatten()
}

/// A decimal literal with its fraction dropped.
fn trunc_text(lit: &str) -> Option<String> {
    let (i, _) = lit.split_once('.')?;
    let neg = i.starts_with('-');
    let digits = i.trim_start_matches('-').trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    Some(if neg && digits != "0" {
        format!("-{digits}")
    } else {
        digits.to_string()
    })
}

/// A decimal literal rounded half away from zero, as PostgreSQL's numeric.
fn round_text(lit: &str) -> Option<String> {
    let (i, f) = lit.split_once('.')?;
    let neg = i.starts_with('-');
    let mut v: u128 = i.trim_start_matches('-').parse().unwrap_or(0);
    if f.bytes().next().is_some_and(|b| b >= b'5') {
        v += 1;
    }
    Some(if neg && v != 0 {
        format!("-{v}")
    } else {
        v.to_string()
    })
}

/// A float as T-SQL writes it with CAST to varchar (style 0): like C's
/// `%g` with 6 significant digits and a three-digit exponent.
pub fn tsql_float_text(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let sci = format!("{:.5e}", v);
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:03}", trim(mant.to_string()), exp.abs())
    } else {
        let decimals = (5 - exp).max(0) as usize;
        trim(format!("{v:.decimals$}"))
    }
}

/// A numeric literal written with an exponent as PostgreSQL prints it: the
/// value with max(0, decimals - exponent) decimals.
pub fn pg_numeric_text(lit: &str) -> Option<String> {
    let (m, e) = lit.split_once('e')?;
    let e: i64 = e.parse().ok()?;
    let neg = m.starts_with('-');
    let m = m.trim_start_matches(['-', '+']);
    let (i, f) = m.split_once('.').unwrap_or((m, ""));
    let digits = format!("{i}{f}");
    let point = i.len() as i64 + e;
    let scale = (f.len() as i64 - e).max(0) as usize;
    let (int, frac) = if point <= 0 {
        (
            String::new(),
            format!("{}{digits}", "0".repeat((-point) as usize)),
        )
    } else if point as usize >= digits.len() {
        (
            format!("{digits}{}", "0".repeat(point as usize - digits.len())),
            String::new(),
        )
    } else {
        (
            digits[..point as usize].to_string(),
            digits[point as usize..].to_string(),
        )
    };
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let mut frac: String = frac.chars().take(scale).collect();
    while frac.len() < scale {
        frac.push('0');
    }
    let zero = int == "0" && frac.bytes().all(|b| b == b'0');
    let sign = if neg && !zero { "-" } else { "" };
    Some(if scale == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    })
}

fn operands(strcmp: &Term) -> String {
    match (strcmp.kid(3), strcmp.kid(4)) {
        (Some(x), Some(y)) => format!("{} and {}", render(x), render(y)),
        _ => "the compared values".into(),
    }
}

/// Short SQL-like rendering of a term for witnesses and messages.
/// "A has `x` where B has `y`" for a mismatch the engine cannot explain.
/// When the two differ only in their dialect tag (a construct lowered per
/// dialect, such as `dfn tsql replace …` against `dfn postgres replace …`),
/// the terms that carry the tags are named instead, with their dialects.
pub fn mismatch_text(m: &Mismatch) -> String {
    let dialect = |t: &Term| match t.atom() {
        Some("tsql") => Some("T-SQL"),
        Some("postgres") => Some("PostgreSQL"),
        Some("db2i") => Some("Db2 for i"),
        _ => None,
    };
    if let (Some(da), Some(db), Some(pa), Some(pb)) = (
        dialect(&m.a),
        dialect(&m.b),
        m.parent_a.as_ref(),
        m.parent_b.as_ref(),
    ) {
        return format!(
            "A has `{}` ({da}) where B has `{}` ({db})",
            render(pa),
            render(pb)
        );
    }
    format!("A has `{}` where B has `{}`", render(&m.a), render(&m.b))
}

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
        "num" | "flt" => r(0),
        "cast" | "conv" => format!("CAST({} AS {})", r(1), render_ty(k.first())),
        "dcast" => format!("CAST({} AS {})", r(4), render_ty(k.get(2))),
        "numstr" | "tostr" => format!("CAST({} AS varchar)", r(k.len() - 1)),
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
        "strreplace" => format!("replace({}, {}, {}) ({})", r(1), r(2), r(3), r(0)),
        "strpos" => format!("position of {} in {} ({})", r(2), r(1), r(0)),
        "trim" => format!("{}trim({})", r(0), r(1)),
        "quoteident" => format!("quoted identifier {}", r(0)),
        "ceil" | "floor" | "sqrt" => format!("{}({})", t.head(), r(0)),
        "daydiff" => format!("days between {} and {}", r(0), r(1)),
        "time" => format!("time({})", r(0)),
        _ => t.to_string(),
    }
}

fn render_ty(t: Option<&Term>) -> String {
    let Some(p) = t.and_then(ty_parts) else {
        return "?".into();
    };
    let base = match (p.fam, p.len) {
        ("Int", Some(8)) => "tinyint",
        ("Int", Some(16)) => "smallint",
        ("Int", Some(64)) => "bigint",
        ("Int", _) => "int",
        ("Exact", _) => "decimal",
        ("Float", Some(24)) => "real",
        ("Float", _) => "float",
        ("VarChar", _) => "varchar",
        ("Char", _) => "char",
        ("Date", _) => "date",
        ("Timestamp", _) => "timestamp",
        ("TimestampTz", _) => "timestamp with time zone",
        ("Time", _) => "time",
        ("Bool", _) => "boolean",
        _ => return "?".into(),
    };
    match (p.fam, p.len, p.scale) {
        ("Exact", Some(l), Some(s)) => format!("{base}({l},{s})"),
        ("VarChar" | "Char" | "Timestamp" | "Time", Some(l), None) => format!("{base}({l})"),
        _ => base.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equiv::term::{a, n};

    fn ts(v: &str) -> Term {
        n("tval", vec![a("ts"), a(v)])
    }

    #[test]
    fn the_end_of_time_against_its_six_digit_form() {
        let end = ts("9999-12-31 23:59:59.9999999");
        let six = ts("9999-12-31 23:59:59.999999");
        let (t, p) = (Dialect::Tsql, Dialect::Postgres);
        // Only in a conversion check.
        assert!(!end_of_time_across(&end, &six, t, p));
        let _scope = ci_collation_scope(true);
        assert!(end_of_time_across(&end, &six, t, p));
        assert!(end_of_time_across(&six, &end, p, t));
        // Rounded; another dialect pair; any other 7-digit value.
        assert!(!end_of_time_across(&end, &ts("10000-01-01 00:00:00"), t, p));
        assert!(!end_of_time_across(&end, &six, t, Dialect::Db2i));
        assert!(!end_of_time_across(&six, &end, t, p));
        assert!(!end_of_time_across(
            &ts("2020-01-01 10:00:00.1234567"),
            &ts("2020-01-01 10:00:00.123456"),
            t,
            p
        ));
        let c = compare_across(&end, &six, t, p);
        assert!(c.mismatches.is_empty());
        assert_eq!(c.assumptions[0].id, "ASM-END-OF-TIME");
        let c = compare_across(&end, &ts("9999-12-31 23:59:59.999998"), t, p);
        assert!(!c.mismatches.is_empty());
    }
}
