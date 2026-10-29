//! Statement-level semantic equivalence (design section 3, layer 5 item 3).
//!
//! Each side's routine is lowered to an ordered list of canonical statement
//! terms (`canon`), paired in order and compared (`compare`). A pair is
//! equal, equal under the assumptions its rewrites needed, different with a
//! witness, or unknown with the reason. Control flow other than the
//! recognised transaction protocols is reported as not covered, so the
//! verdict never claims more than was compared.

pub mod ast;
mod call;
pub mod canon;
pub mod compare;
pub mod ddl;
pub mod dynamic;
pub mod edit;
pub mod eval;
pub mod literal;
pub mod lower;
pub mod options;
pub mod schema;
pub mod script;
pub mod table;
pub mod term;
mod trigger;
pub mod xwit;

use std::collections::BTreeMap;

use sqldiff_syntax::{ast as cst, Dialect, SyntaxNode};

use crate::artifact::Analyzed;
use crate::model::{
    fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, Location, Side, Verdict,
};
use canon::{asm, Canon, Item};
use lower::{Unit, UnitKind};
use schema::Schema;
use term::{a, n, Term};

/// Result of comparing two artifacts statement by statement.
#[derive(Debug, Default)]
pub struct Outcome {
    pub findings: Vec<Finding>,
    pub assumptions: Vec<Assumption>,
    pub normalizations: Vec<String>,
    pub unknown_reasons: Vec<String>,
    /// Statement pairs proven equal that are not token-identical.
    pub structural: usize,
    /// Statement pairs proven equal.
    pub equal: usize,
    /// Why B only refines A (`compare::Cmp::refinements`).
    pub refinements: Vec<String>,
    /// The statement pairs decided here (`coverage::unreceipted`).
    pub receipts: Vec<crate::coverage::Receipt>,
}

impl Outcome {
    fn assume(&mut self, x: Assumption) {
        if !self.assumptions.iter().any(|y| y.id == x.id) {
            self.assumptions.push(x);
        }
    }

    fn normalize(&mut self, s: String) {
        if !self.normalizations.contains(&s) {
            self.normalizations.push(s);
        }
    }

    fn refine(&mut self, s: String) {
        if !self.refinements.contains(&s) {
            self.refinements.push(s);
        }
    }
}

fn side_name(a: &Analyzed) -> String {
    format!("side {:?} ({})", a.side, a.dialect().display_name())
}

/// Lowers every data statement of a unit, to compare with statements in
/// dialect `other`.
fn items(unit: &Unit, schema: &Schema, by_name: bool, other: Dialect) -> Vec<Item> {
    let mut c = Canon::new(unit, schema);
    c.by_name = by_name;
    c.fold_quoted = other == Dialect::Tsql;
    let mut out = Vec::new();
    let mut stmts = unit.stmts.iter().enumerate().peekable();
    let mut ctl = unit.ctl.iter().peekable();
    loop {
        // The IF and loop markers before the next statement.
        let next = stmts.peek().map_or(usize::MAX, |(i, _)| *i);
        while let Some(m) = ctl.next_if(|m| m.after <= next) {
            c.reset();
            out.push(ctl_item(&mut c, unit, m));
        }
        let Some((_, s)) = stmts.next() else { break };
        c.reset();
        let mut nodes = vec![s.node.clone()];
        let res = match (&s.msg, &s.ast) {
            // A message a format builds: compared as its text.
            (Some(m), _) => c.message(m),
            (None, Err(e)) => Err(e.clone()),
            // T-SQL inline table function: `RETURN` and its SELECT are two CST statements.
            (None, Ok(ast::Stmt::Return(None))) if unit.kind == UnitKind::TableFn => {
                match stmts.peek() {
                    Some((_, next)) => match &next.ast {
                        Ok(ast::Stmt::Select(q)) => {
                            nodes.push(next.node.clone());
                            stmts.next();
                            c.stmt(&ast::Stmt::Return(Some(ast::Expr::Subquery(q.clone()))))
                        }
                        _ => c.stmt(&ast::Stmt::Return(None)),
                    },
                    None => c.stmt(&ast::Stmt::Return(None)),
                }
            }
            (None, Ok(st)) => c.stmt(st),
        };
        let (term, err) = match res {
            Ok(t) => (t, None),
            Err(e) => (
                n(
                    "raw",
                    std::iter::once(a(unit.dialect.id()))
                        .chain(s.toks.iter().map(|t| {
                            // Only keywords and unquoted names fold; literals and
                            // quoted identifiers keep their case.
                            a(if t.kind == ast::K::Word {
                                t.up.clone()
                            } else {
                                t.text.clone()
                            })
                        }))
                        .collect(),
                ),
                Some(e),
            ),
        };
        out.push(Item {
            term,
            nodes,
            asm: std::mem::take(&mut c.asm),
            disc: std::mem::take(&mut c.discharged),
            err,
            names: std::mem::take(&mut c.names),
        });
    }
    c.reset();
    c.lift(out)
}

/// The item of an IF or loop marker (`lower::CtlKind`): `ctl-if(cond)`,
/// `ctl-while(cond)`, `ctl-else`, `ctl-end`, `ctl-break`, `ctl-continue`,
/// `ctl-call(callee)`.
fn ctl_item(c: &mut Canon, unit: &Unit, m: &lower::Ctl) -> Item {
    use lower::CtlKind as C;
    let (head, res): (&str, Result<Vec<Term>, String>) = match &m.kind {
        C::If(e) => (
            "ctl-if",
            e.clone().and_then(|e| c.condition(&e)).map(|t| vec![t]),
        ),
        C::While(e) => (
            "ctl-while",
            e.clone().and_then(|e| c.condition(&e)).map(|t| vec![t]),
        ),
        C::Else => ("ctl-else", Ok(vec![])),
        C::End => ("ctl-end", Ok(vec![])),
        C::Break => ("ctl-break", Ok(vec![])),
        C::Continue => ("ctl-continue", Ok(vec![])),
        C::Call => (
            "ctl-call",
            call::parse(&ast::tokens(&m.node), unit.dialect)
                .and_then(|s| call::pair_key(&s, unit.dialect))
                .map(|k| vec![a(k)]),
        ),
    };
    // A marker that could not be read keeps its head, so the branches
    // around it still line up; its own comparison is left unknown.
    let (term, err) = match res {
        Ok(kids) => (n(head, kids), None),
        Err(e) => (
            n(
                head,
                vec![n(
                    "raw",
                    vec![a(unit.dialect.id()), a(m.node.text().to_string())],
                )],
            ),
            Some(e),
        ),
    };
    Item {
        term,
        // The anchor last: `start` places a marker by it.
        nodes: vec![m.node.clone(), m.anchor.clone()],
        asm: std::mem::take(&mut c.asm),
        disc: std::mem::take(&mut c.discharged),
        err,
        names: std::mem::take(&mut c.names),
    }
}

/// Whether an item is an IF or loop marker.
fn is_ctl(it: &Item) -> bool {
    it.term.head().starts_with("ctl-")
}

fn location(side: &Analyzed, nodes: &[SyntaxNode]) -> Location {
    let start = nodes.iter().map(|n| cst::sig_range(n).0).min().unwrap_or(0);
    let end = nodes
        .iter()
        .map(|n| cst::sig_range(n).1)
        .max()
        .unwrap_or(start);
    side.location(start, end)
}

fn line(side: &Analyzed, nodes: &[SyntaxNode]) -> usize {
    nodes.first().map(|n| side.line_of(n)).unwrap_or(0)
}

pub use trigger::functions as trigger_functions;

/// A piece of side B that a conversion wrote from a piece of side A: their
/// byte spans. A conversion records one per unit; units nest (a routine's
/// header unit spans its statements' units), and one input statement can
/// give several units (a declaration moved to the top of its block and the
/// assignment left in its place).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub a: (usize, usize),
    pub b: (usize, usize),
}

/// Cross-dialect comparison of two artifacts. Scripts are compared segment
/// by segment (`script.rs`): a T-SQL batch against the block it became.
/// With `anchors` (a conversion's trace), statements are paired only where
/// the anchors link them (`pair_items`).
pub fn compare(
    a: &Analyzed,
    b: &Analyzed,
    sa: &Schema,
    sb: &Schema,
    anchors: &[Anchor],
) -> Outcome {
    let mut out = Outcome::default();
    if let Some(pairs) = trigger::pairs(a, b, sa, sb, &mut out) {
        for (ua, ub, sa, sb) in &pairs {
            compare_units(a, b, ua, ub, sa, sb, anchors, &mut out);
        }
        return out;
    }
    let pairs =
        script::segment_pairs(a, b).unwrap_or_else(|| vec![(lower::lower(a), lower::lower(b))]);
    for (mut ua, mut ub) in pairs {
        script::name_locals(&mut ua, &mut ub);
        compare_units(a, b, &ua, &ub, sa, sb, anchors, &mut out);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn compare_units(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    anchors: &[Anchor],
    out: &mut Outcome,
) {
    let (na, nb) = (side_name(a), side_name(b));
    for (side, u, name) in [(a, ua, &na), (b, ub, &nb)] {
        for (node, what) in &u.uncovered {
            let at = side.line_of(node);
            out.unknown_reasons.push(format!(
                "{name} line {at}: {what} is not covered by statement-level equivalence"
            ));
            // The statements inside anything uncovered (an IF, loop, CATCH,
            // handler, or a routine beside others) were not compared either:
            // each is named on its own line, so none of them counts as
            // converted.
            let mut seen = vec![at];
            for st in node
                .descendants()
                .filter(|d| d != node && d.kind() == sqldiff_syntax::SyntaxKind::STATEMENT)
            {
                let l = side.line_of(&st);
                if !seen.contains(&l) {
                    seen.push(l);
                    out.unknown_reasons.push(format!(
                        "{name} line {l}: a statement inside {what} at line {at} is not covered by statement-level equivalence"
                    ));
                }
            }
        }
        for note in &u.notes {
            out.normalize(note.clone());
        }
    }
    signature(ua, ub, &na, &nb, out);
    script::locals(a, b, ua, ub, &na, &nb, out);
    tables(a, b, ua, ub, sa, sb, out);
    calls(a, b, ua, ub, sa, sb, out);
    dynamic_sql(a, b, ua, ub, sa, sb, out);
    ddl_stmts(a, b, ua, ub, sa, sb, out);
    for x in ua.assumptions.iter().chain(&ub.assumptions) {
        out.assume(x.clone());
    }
    if ua.txn_control || ub.txn_control {
        out.assume(asm::make(asm::NO_CALLER_TXN));
    }
    let ia = items(ua, sa, false, ub.dialect);
    let ib = items(ub, sb, false, ua.dialect);
    if std::env::var_os("SQLDIFF_DEBUG_TERMS").is_some() {
        for (name, items) in [("A", &ia), ("B", &ib)] {
            for it in items.iter() {
                eprintln!(
                    "{name}: {}{}",
                    it.term,
                    it.err
                        .as_ref()
                        .map(|e| format!("  [{e}]"))
                        .unwrap_or_default()
                );
            }
        }
    }
    let touches = ia.iter().chain(&ib).any(|it| !it.names.is_empty());
    security(a, b, ua, ub, touches, out);
    // Witnesses across dialects compare parameters by position, so the
    // routines must take the same parameters.
    // Statements outside the comparison could change or undo the witness.
    // A trigger's images are rows of its table, which witnesses (tables
    // filled independently) do not keep.
    let wit = (ua.kind == ub.kind
        && ua.kind != UnitKind::Trigger
        && ua.params.len() == ub.params.len()
        && ua.uncovered.is_empty()
        && ub.uncovered.is_empty()
        // A called procedure can touch the witness's table.
        && ua.calls.is_empty()
        && ub.calls.is_empty()
        && ua.dynamic.is_empty()
        && ub.dynamic.is_empty()
        // Witnesses run the statements in a straight line.
        && !ia.iter().chain(&ib).any(is_ctl))
    .then(|| Witnessing {
        a: xwit::Side {
            unit: ua,
            schema: sa,
            name: na.clone(),
        },
        b: xwit::Side {
            unit: ub,
            schema: sb,
            name: nb.clone(),
        },
        // One statement on each side and nothing around it: the witness
        // inputs reach it and its result is the routine's.
        certain: ia.len() == 1 && ib.len() == 1 && !ua.txn_control && !ub.txn_control,
    });
    let gaps = [gaps(a, ua, &ia), gaps(b, ub, &ib)];
    pair_items(a, b, &ia, &ib, &gaps, anchors, wit.as_ref(), out);
    error_continues(a, b, ua, ub, &ia, &ib, out);
}

/// Compares the procedure calls and object comments of both sides
/// (`call.rs`), paired in order by what they call or comment on.
#[allow(clippy::too_many_arguments)]
fn calls(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    out: &mut Outcome,
) {
    use sqldiff_syntax::Dialect;
    if ua.calls.is_empty() && ub.calls.is_empty() {
        return;
    }
    let (na, nb) = (side_name(a), side_name(b));
    let shown = |n: &SyntaxNode| {
        let text = n.text().to_string();
        let w: Vec<&str> = text.split_whitespace().collect();
        let mut s = w.join(" ");
        if s.chars().count() > 60 {
            s = s.chars().take(57).collect::<String>() + "…";
        }
        format!("`{s}`")
    };
    let t_is_a = match (a.dialect(), b.dialect()) {
        (Dialect::Tsql, Dialect::Postgres) => true,
        (Dialect::Postgres, Dialect::Tsql) => false,
        _ => {
            for (side, u, name) in [(a, ua, &na), (b, ub, &nb)] {
                for (node, _) in &u.calls {
                    out.unknown_reasons.push(format!(
                        "{name} line {}: the call {} is not compared: only T-SQL against PostgreSQL is",
                        side.line_of(node),
                        shown(node)
                    ));
                }
            }
            return;
        }
    };
    let read = |side: &Analyzed, u: &Unit, name: &str, out: &mut Outcome| {
        let mut stmts = Vec::new();
        let mut keys = Vec::new();
        for (node, _) in &u.calls {
            match call::parse(&ast::tokens(node), side.dialect())
                .and_then(|s| call::pair_key(&s, side.dialect()).map(|k| (s, k)))
            {
                Ok((s, k)) => {
                    stmts.push(Some(s));
                    keys.push(Some(k));
                }
                Err(e) => {
                    out.unknown_reasons.push(format!(
                        "{name} line {}: the call {} is not compared: {e}",
                        side.line_of(node),
                        shown(node)
                    ));
                    stmts.push(None);
                    keys.push(None);
                }
            }
        }
        // The same object commented twice: T-SQL keeps two properties (or
        // fails on the same name), PostgreSQL keeps the last comment.
        for i in 0..keys.len() {
            let dup = keys[i].as_ref().is_some_and(|k| {
                k.starts_with("comment ")
                    && keys.iter().filter(|x| x.as_ref() == Some(k)).count() > 1
            });
            if dup {
                out.unknown_reasons.push(format!(
                    "{name} line {}: the comment {} is not compared: the same object is commented more than once",
                    side.line_of(&u.calls[i].0),
                    shown(&u.calls[i].0)
                ));
                keys[i] = None;
            }
        }
        (stmts, keys)
    };
    let (xa, ka) = read(a, ua, &na, out);
    let (xb, kb) = read(b, ub, &nb, out);
    let pairs = call::align(&ka, &kb);
    for (side, u, name, keys, mine) in [
        (
            a,
            ua,
            &na,
            &ka,
            pairs.iter().map(|p| p.0).collect::<Vec<_>>(),
        ),
        (
            b,
            ub,
            &nb,
            &kb,
            pairs.iter().map(|p| p.1).collect::<Vec<_>>(),
        ),
    ] {
        for (i, (node, _)) in u.calls.iter().enumerate() {
            if keys[i].is_some() && !mine.contains(&i) {
                out.unknown_reasons.push(format!(
                    "{name} line {}: the call {} is not compared: it has no counterpart on the other side",
                    side.line_of(node),
                    shown(node)
                ));
            }
        }
    }
    for (i, j) in pairs {
        let (xn, at_a) = (&ua.calls[i].0, ua.calls[i].1);
        let (yn, at_b) = (&ub.calls[j].0, ub.calls[j].1);
        let (la, lb) = (a.line_of(xn), b.line_of(yn));
        let unknown = |out: &mut Outcome, why: String| {
            out.unknown_reasons.push(format!(
                "{na} line {la}: the call {} (and {nb} line {lb}) is not compared: {why}",
                shown(xn)
            ));
        };
        // A comment reads and writes no data, so where it sits among the
        // data statements does not matter, as long as nothing after it can
        // create, change or drop the object again: only comments follow.
        let comment = lower::is_comment(xn, a.dialect()) && lower::is_comment(yn, b.dialect());
        if comment {
            if !only_comments_after(a, xn) || !only_comments_after(b, yn) {
                unknown(
                    out,
                    "statements other than comments follow it, so it is not the object's last change".into(),
                );
                continue;
            }
        } else if at_a != at_b {
            unknown(
                out,
                "it does not sit in the same place among the data statements".into(),
            );
            continue;
        }
        let (Some(sx), Some(sy)) = (&xa[i], &xb[j]) else {
            continue;
        };
        let side_a = call::Side {
            d: a.dialect(),
            unit: ua,
            schema: sa,
            name: &na,
        };
        let side_b = call::Side {
            d: b.dialect(),
            unit: ub,
            schema: sb,
            name: &nb,
        };
        let verdict = if t_is_a {
            call::compare((sx, &side_a), (sy, &side_b))
        } else {
            call::compare((sy, &side_b), (sx, &side_a))
        };
        match verdict {
            call::Verdict::Equal(asms) => {
                for x in asms {
                    out.assume(x);
                }
                out.equal += 1;
                out.structural += 1;
            }
            call::Verdict::Unknown(why) => unknown(out, why),
            call::Verdict::Diff(rule, witness, (ot, op)) => {
                let (oa, ob) = if t_is_a { (ot, op) } else { (op, ot) };
                let oa = if oa == 0 { cst::sig_range(xn).0 } else { oa };
                let ob = if ob == 0 { cst::sig_range(yn).0 } else { ob };
                let class = ChangeClass::Semantic;
                let anchor = |n: &SyntaxNode| {
                    crate::normalize::hash_strs(std::iter::once(n.text().to_string().as_str()))
                };
                out.findings.push(Finding {
                    fingerprint: fingerprint(&FingerprintInput {
                        object: &a.object,
                        class,
                        rule,
                        anchor_a: &anchor(xn),
                        anchor_b: &anchor(yn),
                    }),
                    rule: rule.to_string(),
                    class,
                    severity: class.default_severity(),
                    verdict: Verdict::Different,
                    witness: format!("Statements (A line {la}, B line {lb}) differ: {witness}."),
                    a: Some(line_at(a, oa)),
                    b: Some(line_at(b, ob)),
                    assumptions: Vec::new(),
                });
            }
        }
    }
}

/// Compares dynamic SQL (`dynamic.rs`): T-SQL `EXEC (…)` / `sp_executesql`
/// against PL/pgSQL `EXECUTE '…'`, paired in order, each by the one
/// statement its text runs. The texts are compared like two files, so the
/// pair is equal only when that comparison is.
#[allow(clippy::too_many_arguments)]
fn dynamic_sql(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    out: &mut Outcome,
) {
    use sqldiff_syntax::Dialect;
    if ua.dynamic.is_empty() && ub.dynamic.is_empty() {
        return;
    }
    let (na, nb) = (side_name(a), side_name(b));
    let shown = |n: &SyntaxNode| {
        let text = n.text().to_string();
        let w: Vec<&str> = text.split_whitespace().collect();
        let mut s = w.join(" ");
        if s.chars().count() > 60 {
            s = s.chars().take(57).collect::<String>() + "…";
        }
        format!("`{s}`")
    };
    let t_is_a = match (a.dialect(), b.dialect()) {
        (Dialect::Tsql, Dialect::Postgres) => Some(true),
        (Dialect::Postgres, Dialect::Tsql) => Some(false),
        _ => None,
    };
    let paired = t_is_a.is_some() && ua.dynamic.len() == ub.dynamic.len();
    if !paired {
        let why = if t_is_a.is_none() {
            "only T-SQL against PostgreSQL is".to_string()
        } else {
            format!(
                "the sides run dynamic SQL {} and {} times",
                ua.dynamic.len(),
                ub.dynamic.len()
            )
        };
        for (side, u, name) in [(a, ua, &na), (b, ub, &nb)] {
            for (node, _) in &u.dynamic {
                out.unknown_reasons.push(format!(
                    "{name} line {}: the dynamic SQL {} is not compared: {why}",
                    side.line_of(node),
                    shown(node)
                ));
            }
        }
        return;
    }
    let t_is_a = t_is_a == Some(true);
    // Only assignments of variables run before: the tables hold whatever
    // the caller left, so a witness of the texts alone is one of the routine.
    let untouched = |u: &Unit, at: &lower::CallAt| {
        u.calls.is_empty()
            && u.uncovered.is_empty()
            && u.txn_stmts.is_empty()
            && u.stmts[..at.after.min(u.stmts.len())]
                .iter()
                .all(|s| matches!(s.ast, Ok(ast::Stmt::Assign(..))))
    };
    for (k, ((xn, at_a), (yn, at_b))) in ua.dynamic.iter().zip(&ub.dynamic).enumerate() {
        let (la, lb) = (a.line_of(xn), b.line_of(yn));
        let unknown = |out: &mut Outcome, why: &str| {
            out.unknown_reasons.push(format!(
                "{na} line {la}: the dynamic SQL {} (and {nb} line {lb}) is not compared: {why}",
                shown(xn)
            ));
        };
        if at_a != at_b {
            unknown(
                out,
                "it does not sit in the same place among the data statements",
            );
            continue;
        }
        // Calls in the same gap run before or after it: both sides must
        // order them the same way, or a call's changes reach one text only.
        let around = |side: &Analyzed, u: &Unit, n: &SyntaxNode, at: &lower::CallAt| {
            let gap = u.calls.iter().filter(|(c, c_at)| {
                c_at.after == at.after && !lower::is_comment(c, side.dialect())
            });
            let before = gap
                .clone()
                .filter(|(c, _)| c.text_range().start() < n.text_range().start())
                .count();
            (before, gap.count())
        };
        if around(a, ua, xn, at_a) != around(b, ub, yn, at_b) {
            unknown(
                out,
                "calls next to it run in another order relative to it on the two sides",
            );
            continue;
        }
        let (tn, pn) = if t_is_a { (xn, yn) } else { (yn, xn) };
        let ttext = match dynamic::tsql_text(tn) {
            dynamic::Text::Known(s) => s,
            dynamic::Text::Null => {
                unknown(out, "the T-SQL text is NULL, so T-SQL runs nothing");
                continue;
            }
            dynamic::Text::Unknown(why) => {
                unknown(
                    out,
                    &format!("its T-SQL text is not known before it runs: {why}"),
                );
                continue;
            }
        };
        let ptext = match dynamic::pg_text(pn) {
            Some(Ok(s)) => s,
            Some(Err(why)) => {
                unknown(
                    out,
                    &format!("the PostgreSQL text is not a constant: {why}"),
                );
                continue;
            }
            None => {
                unknown(out, "the PostgreSQL side is not EXECUTE");
                continue;
            }
        };
        if let Some(why) = dynamic::tsql_inner_refusal(&ttext) {
            unknown(out, &why);
            continue;
        }
        let pparse = sqldiff_syntax::parse(&ptext, Dialect::Postgres);
        let top = pparse
            .syntax()
            .children()
            .filter(|n| n.kind().is_unit())
            .count();
        if !pparse.diagnostics.is_empty() || top != 1 {
            unknown(out, "the PostgreSQL text is not one statement");
            continue;
        }
        let (ta, tb) = if t_is_a {
            (ttext.as_str(), ptext.as_str())
        } else {
            (ptext.as_str(), ttext.as_str())
        };
        let xa = crate::artifact::Artifact::new(
            format!("{} (dynamic SQL at line {la})", a.artifact.path),
            a.dialect(),
            ta,
        );
        let xb = crate::artifact::Artifact::new(
            format!("{} (dynamic SQL at line {lb})", b.artifact.path),
            b.dialect(),
            tb,
        );
        let (ia, ib) = (
            Analyzed::new(&xa, Side::A, a.excerpt_lines),
            Analyzed::new(&xb, Side::B, b.excerpt_lines),
        );
        let inner = compare(&ia, &ib, sa, sb, &[]);
        // The batch reads ANSI_NULLS and QUOTED_IDENTIFIER from the session,
        // not from the procedure.
        out.assume(asm::make(asm::DYNAMIC_SESSION_OPTIONS));
        for x in inner.assumptions {
            out.assume(x);
        }
        for n in inner.normalizations {
            out.normalize(n);
        }
        for r in inner.refinements {
            out.refine(r);
        }
        for r in &inner.unknown_reasons {
            // `side A (T-SQL) line 1: why` → `why`.
            let why = r.split_once(": ").map_or(r.as_str(), |(_, w)| w);
            unknown(out, &format!("inside its text, {why}"));
        }
        let settled = inner.findings.is_empty() && inner.unknown_reasons.is_empty();
        let first = k == 0 && untouched(ua, at_a) && untouched(ub, at_b);
        for f in inner.findings {
            if f.verdict == Verdict::Different && !first {
                unknown(
                    out,
                    &format!(
                        "inside its text the statements differ on data that earlier statements of the routine may not leave: {}",
                        f.witness
                    ),
                );
                continue;
            }
            let anchor = |n: &SyntaxNode| {
                crate::normalize::hash_strs(std::iter::once(n.text().to_string().as_str()))
            };
            out.findings.push(Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class: f.class,
                    rule: &f.rule,
                    anchor_a: &anchor(xn),
                    anchor_b: &anchor(yn),
                }),
                witness: format!(
                    "The dynamic SQL at A line {la} and B line {lb} runs statements that differ: {}",
                    f.witness
                ),
                a: Some(location(a, std::slice::from_ref(xn))),
                b: Some(location(b, std::slice::from_ref(yn))),
                ..f
            });
        }
        if settled {
            if inner.equal == 0 {
                unknown(out, "nothing in its text was compared");
            } else {
                out.equal += 1;
                out.structural += 1;
            }
        }
    }
}

/// Every statement after `node` in the artifact is an object comment.
fn only_comments_after(side: &Analyzed, node: &SyntaxNode) -> bool {
    let end = node.text_range().end();
    side.root
        .descendants()
        .filter(|n| {
            matches!(
                n.kind(),
                sqldiff_syntax::SyntaxKind::STATEMENT | sqldiff_syntax::SyntaxKind::ROUTINE
            ) && n.text_range().start() >= end
        })
        .all(|n| {
            n.kind() == sqldiff_syntax::SyntaxKind::STATEMENT
                && lower::is_comment(&n, side.dialect())
        })
}

/// A token made up for a statement built around a table's parts.
fn synth(kind: ast::K, text: &str) -> ast::Tok {
    ast::Tok {
        kind,
        text: text.to_string(),
        up: text.to_uppercase(),
        off: 0,
    }
}

/// How two expressions built into statements compare.
enum Same {
    Yes(Vec<Assumption>),
    /// A difference the engine explains: class, rule, witness.
    No(ChangeClass, &'static str, String),
    Unknown(String),
}

/// Compares statement `x` (side A) with `y` (side B) term by term.
#[allow(clippy::too_many_arguments)]
fn same_stmt(
    x: &[ast::Tok],
    y: &[ast::Tok],
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    na: &str,
    nb: &str,
) -> Same {
    let lowered = |toks: &[ast::Tok],
                   u: &Unit,
                   s: &Schema,
                   other: &Unit|
     -> Result<(Term, Vec<Assumption>), String> {
        let st = ast::parse_stmt(toks, u.dialect)?;
        let mut c = Canon::new(u, s);
        c.fold_quoted = other.dialect == Dialect::Tsql;
        let t = c.stmt(&st)?;
        Ok((t, std::mem::take(&mut c.asm)))
    };
    let (tx, mut asms) = match lowered(x, ua, sa, ub) {
        Ok(v) => v,
        Err(e) => return Same::Unknown(format!("{na}: {e}")),
    };
    let (ty, by) = match lowered(y, ub, sb, ua) {
        Ok(v) => v,
        Err(e) => return Same::Unknown(format!("{nb}: {e}")),
    };
    let c = compare::compare_across(&tx, &ty, ua.dialect, ub.dialect);
    for s in by.into_iter().chain(c.assumptions) {
        if !asms.iter().any(|z| z.id == s.id) {
            asms.push(s);
        }
    }
    if c.mismatches.is_empty() {
        return Same::Yes(asms);
    }
    for m in &c.mismatches {
        if let Some((class, rule, witness)) = compare::explain(m, na, nb) {
            return Same::No(class, rule, witness);
        }
    }
    let m = &c.mismatches[0];
    Same::Unknown(compare::describe_unknown(m, na, nb).unwrap_or_else(|| {
        format!(
            "A has `{}` where B has `{}`",
            compare::render(&m.a),
            compare::render(&m.b)
        )
    }))
}

/// The rest of the line from byte `off`, white space trimmed.
fn line_at(side: &Analyzed, off: usize) -> crate::model::Location {
    let text = &side.artifact.text;
    let off = off.min(text.len());
    let end = text[off..].find('\n').map_or(text.len(), |i| off + i);
    let end = off + text[off..end].trim_end().len();
    side.location(off, end.max(off + 1).min(text.len().max(off)))
}

/// Compares the scripts' `CREATE TABLE` statements, in order (`table.rs`).
#[allow(clippy::too_many_arguments)]
fn tables(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    out: &mut Outcome,
) {
    use ast::K;
    if ua.tables.is_empty() && ub.tables.is_empty() {
        return;
    }
    let (na, nb) = (side_name(a), side_name(b));
    if ua.tables.len() != ub.tables.len() {
        for (side, u, name) in [(a, ua, &na), (b, ub, &nb)] {
            for (node, _) in &u.tables {
                out.unknown_reasons.push(format!(
                    "{name} line {}: a CREATE TABLE is not compared: the sides create {} and {} tables",
                    side.line_of(node),
                    ua.tables.len(),
                    ub.tables.len()
                ));
            }
        }
        return;
    }
    for ((xa, pa), (xb, pb)) in ua.tables.iter().zip(&ub.tables) {
        let (la, lb) = (a.line_of(xa), b.line_of(xb));
        let unknown = |out: &mut Outcome, why: String| {
            out.unknown_reasons.push(format!(
                "{na} line {la}: the CREATE TABLE (and {nb} line {lb}) is not compared: {why}"
            ));
        };
        if pa != pb {
            unknown(
                out,
                format!("it comes after {pa} data statement(s) on one side and {pb} on the other"),
            );
            continue;
        }
        let (ta, tb) = match (
            table::parse(&ast::tokens(xa), a.dialect()),
            table::parse(&ast::tokens(xb), b.dialect()),
        ) {
            (Ok(x), Ok(y)) => (x, y),
            (Err(e), _) => {
                unknown(out, format!("{na}: {e}"));
                continue;
            }
            (_, Err(e)) => {
                unknown(out, format!("{nb}: {e}"));
                continue;
            }
        };
        let anchor = |n: &SyntaxNode| {
            crate::normalize::hash_strs(std::iter::once(n.text().to_string().as_str()))
        };
        let mut decided = true;
        let finding = |out: &mut Outcome,
                       rule: &str,
                       class: ChangeClass,
                       witness: String,
                       at: (usize, usize),
                       asms: Vec<String>| {
            out.findings.push(Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class,
                    rule,
                    anchor_a: &format!("{}:{}", anchor(xa), at.0),
                    anchor_b: &format!("{}:{}", anchor(xb), at.1),
                }),
                rule: rule.to_string(),
                class,
                severity: class.default_severity(),
                verdict: Verdict::Different,
                witness,
                a: Some(line_at(a, at.0)),
                b: Some(line_at(b, at.1)),
                assumptions: asms,
            });
        };
        let mut asms: Vec<Assumption> = Vec::new();
        let add = |asms: &mut Vec<Assumption>, x: Assumption| {
            if !asms.iter().any(|y| y.id == x.id) {
                asms.push(x);
            }
        };
        let mut found = false;
        let sch = std::cell::OnceCell::new();
        let with_table = || {
            sch.get_or_init(|| {
                (
                    sa.with_ddl(&xa.text().to_string(), a.dialect()),
                    sb.with_ddl(&xb.text().to_string(), b.dialect()),
                )
            })
        };
        let pg = sqldiff_syntax::Dialect::Postgres;
        if a.dialect() == pg || b.dialect() == pg {
            let (sa2, sb2) = with_table();
            if let Err(e) = ta.case_check(sa2).and_then(|_| tb.case_check(sb2)) {
                unknown(out, e);
                continue;
            }
        }
        for e in table::compare(&ta, &tb, &na, &nb) {
            match e {
                table::Event::Asm(x) => add(&mut asms, asm::make(x)),
                table::Event::Note(s) => out.normalize(s),
                table::Event::Unknown(s) => {
                    decided = false;
                    unknown(out, s);
                }
                table::Event::Diff { rule, witness, at } => {
                    found = true;
                    let ids = asms.iter().map(|x| x.id.clone()).collect();
                    finding(
                        out,
                        rule,
                        ChangeClass::Semantic,
                        format!("Tables (A line {la}, B line {lb}) differ: {witness}."),
                        at,
                        ids,
                    );
                }
                table::Event::Default {
                    col,
                    a: ea,
                    b: eb,
                    at,
                } => {
                    let select = |e: &[ast::Tok]| {
                        let mut v = vec![synth(K::Word, "SELECT")];
                        v.extend(e.iter().cloned());
                        v
                    };
                    match same_stmt(&select(&ea), &select(&eb), ua, ub, sa, sb, &na, &nb) {
                        Same::Yes(xs) => xs.into_iter().for_each(|x| add(&mut asms, x)),
                        Same::No(class, rule, w) => {
                            found = true;
                            finding(
                                out,
                                rule,
                                class,
                                format!("Tables (A line {la}, B line {lb}) differ in the default of column {col}: {w}"),
                                at,
                                Vec::new(),
                            );
                        }
                        Same::Unknown(w) => {
                            decided = false;
                            unknown(
                                out,
                                format!("the defaults of column {col} were not shown equal: {w}"),
                            );
                        }
                    }
                }
                table::Event::Check {
                    what,
                    a: ea,
                    b: eb,
                    at,
                } => {
                    let (sa2, sb2) = with_table();
                    // A check rejects the rows its predicate makes false:
                    // those `WHERE NOT (predicate)` selects.
                    let rejected = |e: &[ast::Tok], name: &[ast::Tok]| {
                        let mut v = vec![
                            synth(K::Word, "SELECT"),
                            synth(K::Num, "1"),
                            synth(K::Word, "FROM"),
                        ];
                        v.extend(name.iter().cloned());
                        v.push(synth(K::Word, "WHERE"));
                        v.push(synth(K::Word, "NOT"));
                        v.push(synth(K::LParen, "("));
                        v.extend(e.iter().cloned());
                        v.push(synth(K::RParen, ")"));
                        v
                    };
                    match same_stmt(
                        &rejected(&ea, &ta.name_toks),
                        &rejected(&eb, &tb.name_toks),
                        ua,
                        ub,
                        sa2,
                        sb2,
                        &na,
                        &nb,
                    ) {
                        Same::Yes(xs) => xs.into_iter().for_each(|x| add(&mut asms, x)),
                        Same::No(class, rule, w) => {
                            found = true;
                            finding(
                                out,
                                rule,
                                class,
                                format!("Tables (A line {la}, B line {lb}) differ in the CHECK constraint `{what}`: {w}"),
                                at,
                                Vec::new(),
                            );
                        }
                        Same::Unknown(w) => {
                            decided = false;
                            unknown(
                                out,
                                format!("the CHECK constraint `{what}` was not shown equal: {w}"),
                            );
                        }
                    }
                }
            }
        }
        for x in asms {
            out.assume(x);
        }
        if decided && !found {
            out.equal += 1;
            out.structural += 1;
        }
    }
}

/// The facts of a key column from its table: a `CREATE TABLE` in the
/// file, else the catalog (when its name is not ambiguous there).
fn col_facts(
    side: &Analyzed,
    u: &Unit,
    schema: &Schema,
    table: &[table::Name],
    col: &str,
) -> Option<ddl::ColFacts> {
    let d = side.dialect();
    let want = ddl::stored_key(table, d).ok()?;
    for (node, _) in &u.tables {
        let Ok(t) = table::parse(&ast::tokens(node), d) else {
            continue;
        };
        if ddl::stored_key(&t.name, d).ok().as_ref() != Some(&want) {
            continue;
        }
        let c = table::column_by_key(&t, col)?;
        return Some(ddl::ColFacts {
            nullable: table::nullable(&t, c).ok()?,
            string: match c.ty {
                table::Ty::Str { fixed, .. } => Some(fixed),
                _ => None,
            },
        });
    }
    let t = schema.table_exact(want.0.as_deref(), &want.1)?;
    let c = t.column(col)?;
    if c.collation.is_some() || matches!(c.ty.fam, schema::Fam::Other | schema::Fam::Binary) {
        return None;
    }
    Some(ddl::ColFacts {
        nullable: !c.not_null,
        string: match c.ty.fam {
            schema::Fam::Char => Some(true),
            schema::Fam::VarChar => Some(false),
            _ => None,
        },
    })
}

/// Compares the scripts' `CREATE INDEX`, `CREATE SEQUENCE` and `CREATE
/// SCHEMA` statements (`ddl.rs`), paired by kind and name.
#[allow(clippy::too_many_arguments)]
fn ddl_stmts(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    out: &mut Outcome,
) {
    use ast::K;
    if ua.ddl.is_empty() && ub.ddl.is_empty() {
        return;
    }
    let (na, nb) = (side_name(a), side_name(b));
    struct Stmt<'n> {
        node: &'n SyntaxNode,
        pos: usize,
        ddl: Result<ddl::Ddl, String>,
        key: Option<String>,
    }
    fn read<'n>(side: &Analyzed, u: &'n Unit) -> Vec<Stmt<'n>> {
        u.ddl
            .iter()
            .map(|(node, pos)| {
                let ddl = ddl::parse(&ast::tokens(node), side.dialect());
                let key = ddl.as_ref().ok().and_then(|x| ddl::pair_key(x).ok());
                Stmt {
                    node,
                    pos: *pos,
                    ddl,
                    key,
                }
            })
            .collect()
    }
    let (xs, ys) = (read(a, ua), read(b, ub));
    // What a statement whose name has no counterpart is paired by: its kind
    // (and an index's table), in order, when both sides have as many such.
    let group = |s: &Stmt| -> Option<String> {
        match s.ddl.as_ref().ok()? {
            ddl::Ddl::Index(i) => {
                let (sc, t) = ddl::object_key(&i.table, i.dialect).ok()?;
                Some(format!("index {}.{t}", sc.unwrap_or_default()))
            }
            x => Some(x.what().to_string()),
        }
    };
    let named = |s: &Stmt, other: &[Stmt]| {
        s.key
            .as_ref()
            .is_some_and(|k| other.iter().any(|o| o.key.as_ref() == Some(k)))
    };
    let mut renamed: Vec<(usize, usize)> = Vec::new();
    {
        let lx: Vec<usize> = (0..xs.len())
            .filter(|&i| xs[i].key.is_some() && !named(&xs[i], &ys))
            .collect();
        let ly: Vec<usize> = (0..ys.len())
            .filter(|&j| ys[j].key.is_some() && !named(&ys[j], &xs))
            .collect();
        let groups: std::collections::BTreeSet<String> =
            lx.iter().filter_map(|&i| group(&xs[i])).collect();
        for g in groups {
            let gx: Vec<usize> = lx
                .iter()
                .copied()
                .filter(|&i| group(&xs[i]).as_ref() == Some(&g))
                .collect();
            let gy: Vec<usize> = ly
                .iter()
                .copied()
                .filter(|&j| group(&ys[j]).as_ref() == Some(&g))
                .collect();
            if gx.len() == gy.len() {
                renamed.extend(gx.into_iter().zip(gy));
            }
        }
    }
    let is_renamed = |is_a: bool, i: usize| {
        renamed
            .iter()
            .any(|&(x, y)| if is_a { x == i } else { y == i })
    };
    let what_of = |s: &Stmt| -> String {
        match &s.ddl {
            Ok(x) => x.what().to_string(),
            Err(_) => cst::first_words(s.node, 2).join(" "),
        }
    };
    // Statements that cannot be paired: unread, or a name used twice.
    let count = |v: &[Stmt], k: &str| v.iter().filter(|s| s.key.as_deref() == Some(k)).count();
    for (side, v, other, name, is_a) in [(a, &xs, &ys, &na, true), (b, &ys, &xs, &nb, false)] {
        for (i, s) in v.iter().enumerate() {
            if is_renamed(is_a, i) {
                continue;
            }
            let why = match (&s.ddl, &s.key) {
                (Err(e), _) => e.clone(),
                (Ok(_), None) => "its name is not read".to_string(),
                (Ok(_), Some(k)) if count(v, k) > 1 => {
                    "the same object is created twice on this side".to_string()
                }
                (Ok(_), Some(k)) if count(other, k) == 0 => {
                    if other.iter().any(|o| o.key.is_none()) {
                        "it has no counterpart among the other side's statements that were read"
                            .to_string()
                    } else {
                        "it has no counterpart on the other side".to_string()
                    }
                }
                _ => continue,
            };
            out.unknown_reasons.push(format!(
                "{name} line {}: the {} is not compared: {why}",
                side.line_of(s.node),
                what_of(s)
            ));
        }
    }
    // Index names are schema objects outside T-SQL: two indexes of one
    // name, or an index named like a table of the file, fail there.
    for (side, v, u, name) in [(a, &xs, ua, &na), (b, &ys, ub, &nb)] {
        let d = side.dialect();
        if d == sqldiff_syntax::Dialect::Tsql {
            continue;
        }
        let mut seen: Vec<(Option<String>, String)> = u
            .tables
            .iter()
            .filter_map(|(n, _)| {
                let toks = ast::tokens(n);
                let t = table::parse(&toks, d).ok()?;
                ddl::stored_key(&t.name, d).ok()
            })
            .collect();
        for s in v.iter() {
            let Ok(ddl::Ddl::Index(ix)) = &s.ddl else {
                continue;
            };
            let Ok((schema, _)) = ddl::stored_key(&ix.table, d) else {
                continue;
            };
            let n = if d == sqldiff_syntax::Dialect::Postgres {
                table::stored(&ix.name)
            } else {
                let Ok(n) = table::key(&ix.name, d) else {
                    continue;
                };
                n
            };
            let k = (schema, n);
            if seen.contains(&k) {
                let loc = line_at(side, ix.off);
                let rule = "equiv.index-name-clash";
                let class = ChangeClass::Semantic;
                out.findings.push(Finding {
                    fingerprint: fingerprint(&FingerprintInput {
                        object: &a.object,
                        class,
                        rule,
                        anchor_a: &k.1,
                        anchor_b: &format!("{}", ix.off),
                    }),
                    rule: rule.to_string(),
                    class,
                    severity: class.default_severity(),
                    verdict: Verdict::Different,
                    witness: format!(
                        "{name} already has a table or index named {} in this schema, so its CREATE INDEX at line {} fails (relation already exists); T-SQL index names are only unique per table",
                        ix.name.text, loc.start_line
                    ),
                    a: (side.side == Side::A).then(|| loc.clone()),
                    b: (side.side == Side::B).then(|| loc.clone()),
                    assumptions: Vec::new(),
                });
            } else {
                seen.push(k);
            }
        }
    }
    let file_schemas = std::cell::OnceCell::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (i, x) in xs.iter().enumerate() {
        let Some(k) = &x.key else { continue };
        if count(&xs, k) != 1 || count(&ys, k) != 1 {
            continue;
        }
        let j = ys.iter().position(|y| y.key.as_ref() == Some(k)).unwrap();
        pairs.push((i, j));
    }
    pairs.extend(renamed.iter().copied());
    pairs.sort();
    for (i, j) in pairs {
        let (x, y) = (&xs[i], &ys[j]);
        let (Ok(dx), Ok(dy)) = (&x.ddl, &y.ddl) else {
            continue;
        };
        let (la, lb) = (a.line_of(x.node), b.line_of(y.node));
        let what = dx.what();
        let unknown = |out: &mut Outcome, why: String| {
            out.unknown_reasons.push(format!(
                "{na} line {la}: the {what} (and {nb} line {lb}) is not compared: {why}"
            ));
        };
        if x.pos != y.pos {
            unknown(
                out,
                format!(
                    "it comes after {} data statement(s) on one side and {} on the other",
                    x.pos, y.pos
                ),
            );
            continue;
        }
        // An index must follow its table on both sides when the file creates it.
        if let (ddl::Ddl::Index(ix), ddl::Ddl::Index(iy)) = (dx, dy) {
            let before = |side: &Analyzed, u: &Unit, ix: &ddl::Index, at: &SyntaxNode| {
                let d = side.dialect();
                let want = ddl::stored_key(&ix.table, d).ok();
                u.tables.iter().any(|(n, _)| {
                    table::parse(&ast::tokens(n), d)
                        .ok()
                        .and_then(|t| ddl::stored_key(&t.name, d).ok())
                        == want
                        && n.text_range().start() > at.text_range().start()
                })
            };
            if before(a, ua, ix, x.node) || before(b, ub, iy, y.node) {
                unknown(
                    out,
                    "the file creates the index's table after the index on one side".into(),
                );
                continue;
            }
        }
        // PostgreSQL names are compared up to letter case only where no
        // other name differs from them only in case.
        let case_check = |side: &Analyzed, u: &Unit, s: &Schema, dd: &ddl::Ddl| {
            let d = side.dialect();
            if d != sqldiff_syntax::Dialect::Postgres {
                return Ok(());
            }
            let file_table = match dd {
                ddl::Ddl::Index(ix) => {
                    let want = ddl::stored_key(&ix.table, d).ok();
                    u.tables.iter().find(|(n, _)| {
                        table::parse(&ast::tokens(n), d)
                            .ok()
                            .and_then(|t| ddl::stored_key(&t.name, d).ok())
                            == want
                    })
                }
                _ => None,
            };
            match file_table {
                Some((n, _)) => ddl::case_check(dd, &s.with_ddl(&n.text().to_string(), d)),
                None => ddl::case_check(dd, s),
            }
        };
        if let Err(e) = case_check(a, ua, sa, dx).and_then(|_| case_check(b, ub, sb, dy)) {
            unknown(out, e);
            continue;
        }
        let table_of = |d: &ddl::Ddl| match d {
            ddl::Ddl::Index(i) => Some((i.table.clone(), i.table_toks.clone())),
            _ => None,
        };
        let (tx, ty) = (table_of(dx), table_of(dy));
        let facts = |is_a: bool, col: &str| -> Option<ddl::ColFacts> {
            if is_a {
                col_facts(a, ua, sa, &tx.as_ref()?.0, col)
            } else {
                col_facts(b, ub, sb, &ty.as_ref()?.0, col)
            }
        };
        let anchor = |n: &SyntaxNode| {
            crate::normalize::hash_strs(std::iter::once(n.text().to_string().as_str()))
        };
        let finding = |out: &mut Outcome,
                       rule: &str,
                       class: ChangeClass,
                       witness: String,
                       at: (usize, usize),
                       asms: Vec<String>| {
            out.findings.push(Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class,
                    rule,
                    anchor_a: &format!("{}:{}", anchor(x.node), at.0),
                    anchor_b: &format!("{}:{}", anchor(y.node), at.1),
                }),
                rule: rule.to_string(),
                class,
                severity: class.default_severity(),
                verdict: Verdict::Different,
                witness,
                a: Some(line_at(a, at.0)),
                b: Some(line_at(b, at.1)),
                assumptions: asms,
            });
        };
        let mut asms: Vec<Assumption> = Vec::new();
        let add = |asms: &mut Vec<Assumption>, x: Assumption| {
            if !asms.iter().any(|y| y.id == x.id) {
                asms.push(x);
            }
        };
        let (mut decided, mut found) = (true, false);
        for e in ddl::compare(dx, dy, &na, &nb, &facts) {
            match e {
                ddl::Event::Asm(x) => add(&mut asms, asm::make(x)),
                ddl::Event::Note(s) => out.normalize(s),
                ddl::Event::Unknown(s) => {
                    decided = false;
                    unknown(out, s);
                }
                ddl::Event::Diff { rule, witness, at } => {
                    found = true;
                    let ids = asms.iter().map(|x| x.id.clone()).collect();
                    finding(
                        out,
                        rule,
                        ChangeClass::Semantic,
                        format!("Statements (A line {la}, B line {lb}) differ: {witness}."),
                        at,
                        ids,
                    );
                }
                ddl::Event::Filter {
                    unique,
                    a: fa,
                    b: fb,
                    at,
                } => {
                    let (sa2, sb2) = file_schemas.get_or_init(|| {
                        (
                            sa.with_ddl(&a.artifact.text, a.dialect()),
                            sb.with_ddl(&b.artifact.text, b.dialect()),
                        )
                    });
                    // The rows an index covers: those its filter selects.
                    let covered = |e: &[ast::Tok], name: &[ast::Tok]| {
                        let mut v = vec![
                            synth(K::Word, "SELECT"),
                            synth(K::Num, "1"),
                            synth(K::Word, "FROM"),
                        ];
                        v.extend(name.iter().cloned());
                        v.push(synth(K::Word, "WHERE"));
                        v.extend(e.iter().cloned());
                        v
                    };
                    let (Some(tx), Some(ty)) = (&tx, &ty) else {
                        continue;
                    };
                    match same_stmt(
                        &covered(&fa, &tx.1),
                        &covered(&fb, &ty.1),
                        ua,
                        ub,
                        sa2,
                        sb2,
                        &na,
                        &nb,
                    ) {
                        Same::Yes(xs) => xs.into_iter().for_each(|x| add(&mut asms, x)),
                        Same::No(class, rule, w) if unique => {
                            found = true;
                            finding(
                                out,
                                rule,
                                class,
                                format!("Unique indexes (A line {la}, B line {lb}) cover different rows, so they reject different rows: {w}"),
                                at,
                                Vec::new(),
                            );
                        }
                        Same::No(_, _, w) => {
                            decided = false;
                            unknown(
                                out,
                                format!("the index filters cover different rows ({w}): this changes how fast statements run, not their results, and is not judged"),
                            );
                        }
                        Same::Unknown(w) => {
                            decided = false;
                            unknown(out, format!("the index filters were not shown equal: {w}"));
                        }
                    }
                }
            }
        }
        for x in asms {
            out.assume(x);
        }
        if decided && !found {
            out.equal += 1;
            out.structural += 1;
        }
    }
}

/// T-SQL keeps executing a procedure after a statement fails (unless
/// XACT_ABORT is on or a CATCH rethrows); PostgreSQL and Db2 for i stop at
/// the error. Any later statement with an effect then runs on one side only.
fn error_continues(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    ia: &[Item],
    ib: &[Item],
    out: &mut Outcome,
) {
    use sqldiff_syntax::Dialect::Tsql;
    let (t_is_a, t, items, tside, oside) = match (ua.dialect, ub.dialect) {
        (Tsql, d) if d != Tsql => (true, ua, ia, a, b),
        (d, Tsql) if d != Tsql => (false, ub, ib, b, a),
        _ => return,
    };
    // Error handlers on either side (uncovered) and statements that were not
    // lowered leave the error path to other checks.
    let o = if t_is_a { ub } else { ua };
    // Plain script statements on the other side run one by one: the client
    // decides whether the script goes on (the behaviour model's business).
    if !o.in_block
        || t.stops_on_error
        || !matches!(t.kind, UnitKind::Proc | UnitKind::Script)
        || !t.uncovered.is_empty()
        || !o.uncovered.is_empty()
        || items.iter().any(|it| it.err.is_some())
    {
        return;
    }
    let out_param = |s: &Term| {
        s.is("sym")
            && s.kid(0)
                .and_then(Term::atom)
                .and_then(|v| v.strip_prefix('p'))
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| t.params.get(i - 1))
                .is_some_and(|p| p.out)
    };
    let effect = |x: &Term| -> bool {
        if x.is("assign") {
            // Only an OUTPUT parameter is visible to the caller.
            return x.kid(0).is_some_and(out_param);
        }
        !(x.is("return") && x.kids().is_empty())
    };
    // An item lifted from several statements (INSERT; SET @id =
    // SCOPE_IDENTITY()) continues inside itself: the INSERT can fail and the
    // assignment still runs.
    let inner = |it: &Item| {
        it.nodes.len() > 1
            && (it.term.is("return")
                || it
                    .term
                    .kid(2)
                    .is_some_and(|k| k.is("into") && k.kid(0).is_some_and(out_param)))
    };
    // Data statements, IF and loop markers and calls in order: a call can
    // fail, and it has an effect (the procedure's, or the comment). A call
    // inside an IF or loop stands where its marker is.
    #[derive(Clone, Copy)]
    enum E {
        Item(usize),
        Call(usize),
        Dyn(usize),
    }
    let call_of = |it: &Item| {
        t.calls
            .iter()
            .position(|(n, _)| it.term.is("ctl-call") && it.nodes.first() == Some(n))
    };
    // An END stands where its IF or loop ends.
    let at = |it: &Item| {
        it.nodes.first().map_or(0, |n| {
            if it.term.is("ctl-end") {
                cst::sig_range(n).1
            } else {
                cst::sig_range(n).0
            }
        })
    };
    let marked: Vec<usize> = items.iter().filter_map(call_of).collect();
    let mut seq: Vec<(usize, E)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (at(it), call_of(it).map_or(E::Item(i), E::Call)))
        .chain(
            t.calls
                .iter()
                .enumerate()
                // Inside a procedure; how a script goes on after a failed
                // statement depends on the client running it.
                .filter(|(_, (_, at))| at.outside.is_none() && t.kind == UnitKind::Proc)
                .filter(|(i, _)| !marked.contains(i))
                .map(|(i, (n, _))| (cst::sig_range(n).0, E::Call(i))),
        )
        // Dynamic SQL fails like a call and changes what its text changes.
        .chain(
            t.dynamic
                .iter()
                .enumerate()
                .filter(|_| t.kind == UnitKind::Proc)
                .map(|(i, (n, _))| (cst::sig_range(n).0, E::Dyn(i))),
        )
        .collect();
    seq.sort_by_key(|x| x.0);
    // Calls inside a script's IF or loop are not in `seq` as effects.
    if t.kind != UnitKind::Proc {
        seq.retain(|(_, e)| matches!(e, E::Item(_)));
    }
    let head = |e: E| match e {
        E::Item(i) => items[i].term.head(),
        E::Call(_) | E::Dyn(_) => "call",
    };
    let is_effect = |e: E| match e {
        E::Item(i) => !is_ctl(&items[i]) && effect(&items[i].term),
        E::Call(_) | E::Dyn(_) => true,
    };
    // The END of each IF, ELSE and WHILE marker, and each END's opener.
    let mut close = vec![usize::MAX; seq.len()];
    let mut opener = vec![usize::MAX; seq.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (k, (_, e)) in seq.iter().enumerate() {
        match head(*e) {
            "ctl-if" | "ctl-while" => stack.push(k),
            "ctl-else" => {}
            "ctl-end" => {
                if let Some(o) = stack.pop() {
                    opener[k] = o;
                    close[o] = k;
                }
            }
            _ => {}
        }
    }
    for k in 0..seq.len() {
        if head(seq[k].1) == "ctl-else" {
            close[k] = (k..seq.len())
                .find(|&e| head(seq[e].1) == "ctl-end" && opener[e] < k)
                .unwrap_or(usize::MAX);
        }
    }
    // What runs after the statement at `p` fails and the routine goes on:
    // the first effect sure to run (no IF, loop or jump decides it), else
    // the first effect that may run.
    enum Next {
        Sure(usize),
        Maybe(usize),
        None,
    }
    let after = |p: usize, maybe_only: bool| -> Next {
        let mut depth = 0usize;
        let mut unsure = maybe_only;
        let mut maybe = None;
        let mut k = p + 1;
        while k < seq.len() {
            let e = seq[k].1;
            match head(e) {
                "ctl-if" | "ctl-while" => depth += 1,
                // The branch around `p` ends: skip the ELSE branches.
                "ctl-else" if depth == 0 => {
                    k = close[k];
                    continue;
                }
                "ctl-end" if depth > 0 => depth -= 1,
                "ctl-end" => {
                    // The loop around `p` goes round again: what it ran
                    // before `p` may run again, and it may never end.
                    let o = opener[k];
                    if o != usize::MAX && head(seq[o].1) == "ctl-while" {
                        if let Some(x) = (o + 1..p).find(|&x| is_effect(seq[x].1)) {
                            maybe = maybe.or(Some(x));
                        }
                        unsure = true;
                    }
                }
                "ctl-break" | "ctl-continue" => unsure = true,
                "return" => {
                    if is_effect(e) {
                        if depth == 0 && !unsure {
                            return Next::Sure(k);
                        }
                        maybe = maybe.or(Some(k));
                    }
                    if depth == 0 && !unsure {
                        break;
                    }
                    unsure = true;
                }
                _ if is_effect(e) => {
                    if depth == 0 && !unsure {
                        return Next::Sure(k);
                    }
                    maybe = maybe.or(Some(k));
                }
                _ => {}
            }
            k += 1;
        }
        maybe.map_or(Next::None, Next::Maybe)
    };
    let mut first_maybe = None;
    let mut found = None;
    for (i, &(_, e)) in seq.iter().enumerate() {
        if let E::Item(k) = e {
            if inner(&items[k]) {
                found = Some((i, i));
                break;
            }
            match items[k].term.head() {
                // These cannot fail; a call marker's call is in `seq`.
                "ctl-else" | "ctl-end" | "ctl-break" | "ctl-continue" | "ctl-call" => continue,
                _ => {}
            }
            if items[k].nodes.len() == 1 && script::cannot_fail(t, &items[k].nodes[0]) {
                continue;
            }
        }
        // Where a failing IF or WHILE condition goes on is not modelled.
        let cond = matches!(head(e), "ctl-if" | "ctl-while");
        match after(i, cond) {
            Next::Sure(j) => {
                found = Some((i, j));
                break;
            }
            Next::Maybe(j) => {
                first_maybe = first_maybe.or(Some((i, j)));
            }
            Next::None => {}
        }
    }
    let Some((i, j)) = found else {
        if let Some((i, j)) = first_maybe {
            let line_of = |e: E| match e {
                E::Item(k) => line(tside, &items[k].nodes),
                E::Call(k) => tside.line_of(&t.calls[k].0),
                E::Dyn(k) => tside.line_of(&t.dynamic[k].0),
            };
            let tname = side_name(tside);
            let oname = side_name(oside);
            out.unknown_reasons.push(format!(
                "{tname} line {}: if this statement fails, {tname} reports the error and goes on (no SET XACT_ABORT ON or rethrowing CATCH), and the statement at line {} may run after it, where {oname} stops at the error: not compared",
                line_of(seq[i].1),
                line_of(seq[j].1)
            ));
        }
        return;
    };
    let nodes = |e: E| -> Vec<SyntaxNode> {
        match e {
            E::Item(k) => items[k].nodes.clone(),
            E::Call(k) => vec![t.calls[k].0.clone()],
            E::Dyn(k) => vec![t.dynamic[k].0.clone()],
        }
    };
    let (xn, yn) = (nodes(seq[i].1), nodes(seq[j].1));
    let lx = location(tside, &xn[..1]);
    let ly = location(tside, &yn[yn.len() - 1..]);
    let tname = side_name(tside);
    let oname = side_name(oside);
    let witness = format!(
        "If the statement at {tname} line {} fails, {tname} reports the error and continues with line {} (no SET XACT_ABORT ON or rethrowing CATCH); {oname} stops at the error.",
        lx.start_line, ly.start_line
    );
    let asm = Assumption::default(
        "ASM-TSQL-CALLER-XACT-ABORT-OFF",
        "The caller's XACT_ABORT setting is OFF",
    );
    let rule = "equiv.error-continues";
    let class = ChangeClass::Transactional;
    let anchor_text = match seq[i].1 {
        E::Item(k) => items[k].term.to_string(),
        E::Call(_) | E::Dyn(_) => xn[0].text().to_string(),
    };
    let anchor = crate::normalize::hash_strs(std::iter::once(anchor_text.as_str()));
    out.findings.push(Finding {
        fingerprint: fingerprint(&FingerprintInput {
            object: &a.object,
            class,
            rule,
            anchor_a: if t_is_a { &anchor } else { "" },
            anchor_b: if t_is_a { "" } else { &anchor },
        }),
        rule: rule.to_string(),
        class,
        severity: class.default_severity(),
        verdict: Verdict::Different,
        witness,
        a: t_is_a.then(|| lx.clone()),
        b: (!t_is_a).then(|| lx.clone()),
        assumptions: vec![asm.id.clone()],
    });
    out.assume(asm);
}

/// Compares whose privileges the statements run with. Matching contexts
/// still need an assumption about the privileges themselves; an owner's
/// context against a caller's is a difference for any caller that may run
/// the routine but not touch its tables, the usual reason for the option.
fn security(a: &Analyzed, b: &Analyzed, ua: &Unit, ub: &Unit, touches: bool, out: &mut Outcome) {
    use lower::Security::{Caller, Naming, Owner};
    use sqldiff_syntax::Dialect::Tsql;
    let routine = |u: &Unit| !matches!(u.kind, UnitKind::View | UnitKind::Script);
    if !routine(ua) || !routine(ub) {
        return;
    }
    let tsql = ua.dialect == Tsql || ub.dialect == Tsql;
    let (na, nb) = (side_name(a), side_name(b));
    let cite = |u: &Unit| match &u.security_cite {
        Some((text, _)) => text.clone(),
        None => match (u.dialect, u.security) {
            (_, Naming) => "no SET OPTION USRPRF".to_string(),
            (Tsql, _) => "EXECUTE AS CALLER, the default".to_string(),
            _ => "SECURITY INVOKER, the default".to_string(),
        },
    };
    // Privileges matter only to statements that use tables.
    match (ua.security, ub.security) {
        _ if ua.security == ub.security && !touches => {}
        (Caller, Caller) => {
            if tsql {
                out.assume(asm::make(asm::OWNERSHIP_CHAIN));
            }
        }
        (Owner, Owner) => out.assume(asm::make(asm::OWNER_PRIVILEGES)),
        (Naming, Caller) | (Caller, Naming) if !touches => {}
        (Naming, Caller) | (Caller, Naming) => {
            out.assume(asm::make(asm::DB2_USRPRF_USER));
            if tsql {
                out.assume(asm::make(asm::OWNERSHIP_CHAIN));
            }
        }
        (Naming, Owner) | (Owner, Naming) => {
            let (owner, oname) = if ua.security == Owner {
                (ua, &na)
            } else {
                (ub, &nb)
            };
            out.unknown_reasons.push(format!(
                "{oname} runs with the owner's privileges ({}) and the Db2 for i side states no SET OPTION USRPRF, so its authority depends on the naming convention it was created with",
                cite(owner)
            ));
        }
        (Owner, Caller) | (Caller, Owner) => {
            let owner_is_a = ua.security == Owner;
            let (owner, caller, oname, cname) = if owner_is_a {
                (ua, ub, &na, &nb)
            } else {
                (ub, ua, &nb, &na)
            };
            if caller.dialect == Tsql {
                out.unknown_reasons.push(format!(
                    "{oname} runs with the owner's privileges ({}); {cname} checks the caller's ({}) except along T-SQL ownership chains, so which calls fail a permission check depends on who owns the objects",
                    cite(owner),
                    cite(caller)
                ));
                return;
            }
            let witness = format!(
                "A caller that may run the routine but holds no privileges of its own on the objects it uses: {oname} runs the statements with the owner's privileges ({}) and succeeds, {cname} checks the caller's privileges ({}) and fails with a permission error. Functions that report the current user differ too.",
                cite(owner),
                cite(caller)
            );
            let loc = |side: &Analyzed, u: &Unit| {
                let (s, e) = u.security_cite.as_ref().map(|c| c.1).unwrap_or(u.header_at);
                side.location(s, e)
            };
            let rule = "equiv.security-context";
            let class = ChangeClass::Semantic;
            out.findings.push(Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class,
                    rule,
                    anchor_a: &format!("{:?}", ua.security),
                    anchor_b: &format!("{:?}", ub.security),
                }),
                rule: rule.to_string(),
                class,
                severity: class.default_severity(),
                verdict: Verdict::Different,
                witness,
                a: Some(loc(a, ua)),
                b: Some(loc(b, ub)),
                assumptions: Vec::new(),
            });
        }
        (Naming, Naming) => {}
    }
}

fn signature(ua: &Unit, ub: &Unit, na: &str, nb: &str, out: &mut Outcome) {
    let result_kind = |k: UnitKind| matches!(k, UnitKind::View | UnitKind::TableFn);
    if ua.kind != ub.kind && !(result_kind(ua.kind) && result_kind(ub.kind)) {
        out.unknown_reasons.push(format!(
            "{na} is a {} and {nb} is a {}; calling conventions are not compared",
            ua.kind.label(),
            ub.kind.label()
        ));
    }
    if ua.params.len() != ub.params.len() {
        out.unknown_reasons.push(format!(
            "{na} takes {} parameter(s) and {nb} takes {}",
            ua.params.len(),
            ub.params.len()
        ));
        return;
    }
    for (i, (x, y)) in ua.params.iter().zip(&ub.params).enumerate() {
        match (x.ty, y.ty) {
            // A T-SQL bit becomes a PostgreSQL boolean: the converter's
            // pg.bit-boolean decision, and the terms treat them alike.
            (Some(tx), Some(ty)) if bit_bool(tx, ty) => {}
            (Some(tx), Some(ty)) if tx.fam != ty.fam => out.unknown_reasons.push(format!(
                "parameter {} is {:?} on {na} and {:?} on {nb}",
                i + 1,
                tx.fam,
                ty.fam
            )),
            (Some(tx), Some(ty)) if tx != ty => out.assume(Assumption::default(
                "ASM-ARGUMENT-LENGTHS",
                "Arguments fit the shorter of the declared parameter types",
            )),
            _ => {}
        }
        let bits = matches!((x.ty, y.ty), (Some(tx), Some(ty)) if bit_bool(tx, ty));
        match (&x.default, &y.default) {
            (Some(dx), Some(dy))
                if bits && bool_default(dx).is_some_and(|v| Some(v) == bool_default(dy)) => {}
            (Some(dx), Some(dy)) if dx != dy => out.unknown_reasons.push(format!(
                "parameter {} defaults to {dx} on {na} and {dy} on {nb}",
                i + 1
            )),
            // A default on one side only widens which calls are accepted;
            // calls both sides accept behave the same.
            (Some(_), None) | (None, Some(_)) => out.normalizations.push(format!(
                "parameter {} has a default only on {}; calls that omit it are valid only there",
                i + 1,
                if x.default.is_some() { na } else { nb }
            )),
            _ => {}
        }
        if x.out != y.out {
            out.unknown_reasons.push(format!(
                "parameter {} is an output parameter on only one side",
                i + 1
            ));
        }
    }
    match (ua.returns, ub.returns) {
        (Some(x), Some(y)) if bit_bool(x, y) => {}
        (Some(x), Some(y)) if x.fam != y.fam => out.unknown_reasons.push(format!(
            "the return type is {:?} on {na} and {:?} on {nb}",
            x.fam, y.fam
        )),
        (Some(x), Some(y)) if x != y => out.assume(Assumption::default(
            "ASM-RETURN-LENGTHS",
            "Returned values fit the shorter of the declared return types",
        )),
        _ => {}
    }
}

/// A bit (an integer of one bit, only T-SQL has one) against a boolean.
fn bit_bool(x: schema::Ty, y: schema::Ty) -> bool {
    let bit = |t: schema::Ty| t.fam == schema::Fam::Int && t.len == Some(1);
    let bool = |t: schema::Ty| t.fam == schema::Fam::Bool;
    (bit(x) && bool(y)) || (bool(x) && bit(y))
}

/// A bit or boolean parameter default as true or false: 1 / 0 or TRUE / FALSE.
fn bool_default(s: &str) -> Option<bool> {
    match s
        .trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace())
        .to_ascii_uppercase()
        .as_str()
    {
        "1" | "TRUE" => Some(true),
        "0" | "FALSE" => Some(false),
        _ => None,
    }
}

fn key(t: &Term) -> String {
    // IF and loop markers line up by kind; their conditions are compared
    // once paired.
    if t.head().starts_with("ctl-") {
        return t.head().to_string();
    }
    format!(
        "{}:{}",
        t.head(),
        t.kid(0).map(|k| k.to_string()).unwrap_or_default()
    )
}

/// What a cross-dialect witness search needs beyond the statement pair.
struct Witnessing<'u> {
    a: xwit::Side<'u>,
    b: xwit::Side<'u>,
    certain: bool,
}

fn other_terms(items: &[Item], k: usize) -> Vec<&Term> {
    items
        .iter()
        .enumerate()
        .filter(|(n, _)| *n != k)
        .map(|(_, x)| &x.term)
        .collect()
}

/// A stretch of a unit's body that statement-level equivalence leaves out
/// (an IF, a loop, an uncovered statement, a CATCH or EXCEPTION section):
/// the item index it sits before, its line and what it is. Declarations,
/// handlers, labels and routine options are not gaps: they do not take a
/// statement's place.
struct Gap {
    before: usize,
    /// Byte offset where it starts.
    start: usize,
    line: usize,
    what: String,
}

fn gaps(side: &Analyzed, u: &Unit, items: &[Item]) -> Vec<Gap> {
    use sqldiff_syntax::SyntaxKind as SK;
    // A marker stands where it is written, not at its anchor.
    let starts: Vec<usize> = items
        .iter()
        .map(|it| {
            let nodes = if is_ctl(it) {
                &it.nodes[..1]
            } else {
                &it.nodes[..]
            };
            nodes.iter().map(|n| cst::sig_range(n).0).min().unwrap_or(0)
        })
        .collect();
    let mut found: Vec<(usize, Gap)> = Vec::new();
    for (node, what) in &u.uncovered {
        let takes_place = match node.kind() {
            SK::IF_STMT
            | SK::LOOP_STMT
            | SK::CASE_STMT
            | SK::CATCH_BLOCK
            | SK::EXCEPTION_SECTION
            | SK::ERROR => true,
            SK::STATEMENT => !node
                .parent()
                .is_some_and(|p| p.kind() == SK::DECLARE_SECTION),
            _ => false,
        };
        let (s, e) = cst::sig_range(node);
        // A construct around compared statements (a routine's option) is
        // not in their sequence.
        if !takes_place || starts.iter().any(|&x| s <= x && x < e) {
            continue;
        }
        found.push((
            s,
            Gap {
                before: starts.iter().filter(|&&x| x < s).count(),
                start: s,
                line: side.line_of(node),
                what: what.clone(),
            },
        ));
    }
    found.sort_by_key(|g| g.0);
    found.into_iter().map(|(_, g)| g).collect()
}

/// One position in a side's sequence: a statement (its index, pairing key
/// and whether it could not be read) or a gap.
#[derive(Clone, Copy, Debug)]
enum Slot<'k> {
    Item(usize, &'k str, bool),
    Gap(usize),
}

impl<'k> Slot<'k> {
    fn key(&self) -> Option<&'k str> {
        match self {
            Slot::Item(_, k, _) => Some(k),
            Slot::Gap(_) => None,
        }
    }
}

/// A statement (of side A or B) that lines up with a gap on the other side.
#[derive(Clone, Copy, Debug)]
struct Beside {
    item_a: bool,
    item: usize,
    gap: usize,
}

/// The slots of a side: its items in order, with each gap before the item
/// it precedes.
fn slots<'k>(keys: &'k [String], items: &[Item], gaps: &[Gap]) -> Vec<Slot<'k>> {
    let mut out = Vec::new();
    let mut g = gaps.iter().enumerate().peekable();
    for (i, k) in keys.iter().enumerate() {
        while let Some((gi, _)) = g.next_if(|(_, x)| x.before <= i) {
            out.push(Slot::Gap(gi));
        }
        out.push(Slot::Item(i, k, items[i].err.is_some()));
    }
    out.extend(g.map(|(gi, _)| Slot::Gap(gi)));
    out
}

/// Pairs the statements of two sides whose numbers differ. Without gaps:
/// the longest common subsequence of pairing keys. With gaps: an alignment
/// of the slots with the most matches of two statements with the same key,
/// then the most matches of two gaps, then the most matches of a gap against
/// one statement (a T-SQL statement converted into a PostgreSQL loop block,
/// or replaced by a not-converted placeholder) or of a statement that could
/// not be read against any statement. Only what every best alignment agrees
/// on is kept: two statements are paired, or a statement is put beside a
/// gap, when every best alignment does so. A statement that best alignments
/// place differently is left unpaired and reported, never compared with a
/// neighbour of its counterpart.
fn align(a: &[Slot], b: &[Slot]) -> Alignment {
    // IF and loop markers (`ctl-…` keys) are aligned like gaps are: a
    // statement that could sit inside a loop or after it is not paired.
    let gap = |s: &[Slot]| {
        s.iter().any(|x| match x {
            Slot::Gap(_) => true,
            Slot::Item(_, k, _) => k.starts_with("ctl-"),
        })
    };
    if !gap(a) && !gap(b) {
        let ka: Vec<&str> = a.iter().filter_map(Slot::key).collect();
        let kb: Vec<&str> = b.iter().filter_map(Slot::key).collect();
        return Alignment {
            pairs: crate::normalize::lcs_pairs(&ka, &kb),
            ..Alignment::default()
        };
    }
    align_certain(a, b)
}

/// The alignment search of `align`: only what every best alignment agrees
/// on.
fn align_certain(a: &[Slot], b: &[Slot]) -> Alignment {
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) > ALIGN_CELLS {
        return Alignment::default();
    }
    // A match of keys outweighs any number of gap matches, and a gap
    // against a gap any number of the others. With `ALIGN_CELLS` the
    // scores stay far below `u64::MAX`.
    let tier = (n + m + 1) as u64;
    let w = |x: &Slot, y: &Slot| match (x, y) {
        (Slot::Item(_, k, r), Slot::Item(_, l, q)) => match () {
            _ if k == l => Some(tier * tier),
            _ if *r || *q => Some(1),
            _ => None,
        },
        (Slot::Gap(_), Slot::Gap(_)) => Some(tier),
        _ => Some(1),
    };
    // f: best score of a[..i] against b[..j]; g: of a[i..] against b[j..].
    let mut f = vec![vec![0u64; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            let d = w(&a[i - 1], &b[j - 1]).map_or(0, |x| f[i - 1][j - 1] + x);
            f[i][j] = d.max(f[i - 1][j]).max(f[i][j - 1]);
        }
    }
    let mut g = vec![vec![0u64; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let d = w(&a[i], &b[j]).map_or(0, |x| g[i + 1][j + 1] + x);
            g[i][j] = d.max(g[i + 1][j]).max(g[i][j + 1]);
        }
    }
    let best = f[n][m];
    let usable =
        |i: usize, j: usize| w(&a[i], &b[j]).is_some_and(|x| f[i][j] + x + g[i + 1][j + 1] == best);
    let mut out = Alignment::default();
    // Every best alignment matches slot `i` of a side, and with one partner.
    for is_a in [true, false] {
        let (len, other) = if is_a { (n, m) } else { (m, n) };
        for i in 0..len {
            let at = |k: usize| if is_a { (i, k) } else { (k, i) };
            let skipped = (0..=other).any(|k| {
                let (x, y) = at(k);
                if is_a {
                    f[x][y] + g[x + 1][y] == best
                } else {
                    f[x][y] + g[x][y + 1] == best
                }
            });
            let partners: Vec<usize> = (0..other)
                .filter(|&k| {
                    let (x, y) = at(k);
                    usable(x, y)
                })
                .collect();
            let mine = if is_a { &a[i] } else { &b[i] };
            let Slot::Item(item, ..) = *mine else {
                continue;
            };
            let theirs = |k: usize| if is_a { b[k] } else { a[k] };
            match (skipped, partners.as_slice()) {
                (false, &[k]) => match theirs(k) {
                    Slot::Item(j, ..) => {
                        if is_a {
                            out.pairs.push((item, j));
                        }
                    }
                    Slot::Gap(gi) => out.beside.push(Beside {
                        item_a: is_a,
                        item,
                        gap: gi,
                    }),
                },
                (_, []) => {}
                _ => out.ambiguous.push((is_a, item)),
            }
        }
    }
    out
}

/// Above this many cells the alignment is not searched and nothing is
/// paired.
const ALIGN_CELLS: usize = 16_000_000;

/// How two sides' statements were paired (`align`).
#[derive(Debug, Default)]
struct Alignment {
    pairs: Vec<(usize, usize)>,
    /// Statements every best alignment puts against a gap.
    beside: Vec<Beside>,
    /// Statements best alignments place differently: (side A, index).
    ambiguous: Vec<(bool, usize)>,
    /// Statements whose traced counterpart is out of order: (side A, index).
    moved: Vec<(bool, usize)>,
}

fn start(it: &Item) -> usize {
    if is_ctl(it) {
        return it.nodes.last().map_or(0, |n| cst::sig_range(n).0);
    }
    it.nodes
        .iter()
        .map(|n| cst::sig_range(n).0)
        .min()
        .unwrap_or(0)
}

/// The anchors a position of a side sits in most closely: of the anchors
/// whose span on that side holds it, those with the shortest span (several
/// when one input statement gave several pieces of output).
fn innermost(anchors: &[Anchor], side_a: bool, at: usize) -> Vec<usize> {
    let span = |x: &Anchor| if side_a { x.a } else { x.b };
    let holding: Vec<usize> = (0..anchors.len())
        .filter(|&k| {
            let (s, e) = span(&anchors[k]);
            s <= at && at < e
        })
        .collect();
    let Some(len) = holding
        .iter()
        .map(|&k| span(&anchors[k]).1 - span(&anchors[k]).0)
        .min()
    else {
        return Vec::new();
    };
    holding
        .into_iter()
        .filter(|&k| span(&anchors[k]).1 - span(&anchors[k]).0 == len)
        .collect()
}

/// Pairs statements through a conversion's trace: two statements can pair
/// only when they sit in the same anchor (the input statement and the
/// output written from it). Statements linked through shared anchors form a
/// group; a group of one statement per side is a pair, a larger group is
/// aligned on its own (only what every best alignment agrees on). A
/// statement in no anchor, or alone in its group, has no counterpart. Pairs
/// must keep the order of both sides: pairs that cross another pair are
/// dropped, since comparing statements one by one says nothing about a
/// changed order.
fn traced(
    ia: &[Item],
    ib: &[Item],
    ka: &[String],
    kb: &[String],
    gaps: &[Vec<Gap>; 2],
    anchors: &[Anchor],
) -> Alignment {
    let (n, m) = (ia.len(), ib.len());
    let at_a: Vec<Vec<usize>> = ia
        .iter()
        .map(|x| innermost(anchors, true, start(x)))
        .collect();
    let at_b: Vec<Vec<usize>> = ib
        .iter()
        .map(|x| innermost(anchors, false, start(x)))
        .collect();
    // Union-find over A items, B items and anchors.
    let mut up: Vec<usize> = (0..n + m + anchors.len()).collect();
    fn root(up: &mut [usize], mut x: usize) -> usize {
        while up[x] != x {
            up[x] = up[up[x]];
            x = up[x];
        }
        x
    }
    for (base, at) in [(0, &at_a), (n, &at_b)] {
        for (i, ks) in at.iter().enumerate() {
            for &k in ks {
                let (x, y) = (root(&mut up, base + i), root(&mut up, n + m + k));
                up[x] = y;
            }
        }
    }
    let mut groups: BTreeMap<usize, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (base, at) in [(0, &at_a), (n, &at_b)] {
        for (i, ks) in at.iter().enumerate() {
            if !ks.is_empty() {
                let g = groups.entry(root(&mut up, base + i)).or_default();
                if base == 0 { &mut g.0 } else { &mut g.1 }.push(i);
            }
        }
    }
    let mut out = Alignment::default();
    for (ga, gb) in groups.values() {
        match (ga.as_slice(), gb.as_slice()) {
            ([], _) | (_, []) => {}
            (&[i], &[j]) => out.pairs.push((i, j)),
            _ => {
                let sa: Vec<Slot> = ga
                    .iter()
                    .map(|&i| Slot::Item(i, &ka[i], ia[i].err.is_some()))
                    .collect();
                let sb: Vec<Slot> = gb
                    .iter()
                    .map(|&j| Slot::Item(j, &kb[j], ib[j].err.is_some()))
                    .collect();
                let g = align_certain(&sa, &sb);
                out.pairs.extend(g.pairs);
                out.ambiguous.extend(g.ambiguous);
            }
        }
    }
    // Pairs out of order with another pair are not kept.
    let crossing: Vec<bool> = out
        .pairs
        .iter()
        .map(|&(i, j)| {
            out.pairs
                .iter()
                .any(|&(x, y)| (x < i && y > j) || (x > i && y < j))
        })
        .collect();
    let mut kept = Vec::new();
    for (p, c) in out.pairs.iter().zip(crossing) {
        if c {
            out.moved.push((true, p.0));
            out.moved.push((false, p.1));
        } else {
            kept.push(*p);
        }
    }
    kept.sort_unstable();
    out.pairs = kept;
    // A statement left unpaired whose anchor holds a gap of the other side:
    // its counterpart is in that gap.
    for (is_a, at) in [(true, &at_a), (false, &at_b)] {
        let other = if is_a { &gaps[1] } else { &gaps[0] };
        for (i, ks) in at.iter().enumerate() {
            if ks.is_empty()
                || out
                    .pairs
                    .iter()
                    .any(|p| if is_a { p.0 == i } else { p.1 == i })
                || out.moved.contains(&(is_a, i))
            {
                continue;
            }
            let near: Vec<usize> = (0..other.len())
                .filter(|&g| {
                    innermost(anchors, !is_a, other[g].start)
                        .iter()
                        .any(|k| ks.contains(k))
                })
                .collect();
            if let [g] = near.as_slice() {
                out.ambiguous.retain(|x| *x != (is_a, i));
                out.beside.push(Beside {
                    item_a: is_a,
                    item: i,
                    gap: *g,
                });
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn pair_items(
    a: &Analyzed,
    b: &Analyzed,
    ia: &[Item],
    ib: &[Item],
    gaps: &[Vec<Gap>; 2],
    anchors: &[Anchor],
    wit: Option<&Witnessing>,
    out: &mut Outcome,
) {
    let ka: Vec<String> = ia.iter().map(|x| key(&x.term)).collect();
    let kb: Vec<String> = ib.iter().map(|x| key(&x.term)).collect();
    let Alignment {
        pairs,
        beside,
        ambiguous,
        moved,
    } = if !anchors.is_empty() {
        // Markers fall into their IF or loop's anchor; `same_branches`
        // below still drops any pair across branches.
        traced(ia, ib, &ka, &kb, gaps, anchors)
    } else if ia.iter().chain(ib).any(is_ctl) {
        align_flow(ia, ib, &ka, &kb, gaps)
    } else if ia.len() == ib.len() {
        Alignment {
            pairs: (0..ia.len()).map(|i| (i, i)).collect(),
            ..Alignment::default()
        }
    } else {
        let (sa, sb) = (slots(&ka, ia, &gaps[0]), slots(&kb, ib, &gaps[1]));
        align(&sa, &sb)
    };
    let (pairs, elsewhere) = same_branches(ia, ib, pairs);
    let (na, nb) = (side_name(a), side_name(b));
    for (side, items, name, is_a) in [(a, ia, &na, true), (b, ib, &nb, false)] {
        let is_paired = |i: usize| pairs.iter().any(|p| if is_a { p.0 == i } else { p.1 == i });
        let frames = branches(items);
        for (i, it) in items.iter().enumerate() {
            if is_paired(i) {
                continue;
            }
            // An ELSE or END whose IF or loop is not paired: the IF or
            // loop's own reason names its line.
            if frames[i].1.is_some_and(|o| !is_paired(o)) {
                continue;
            }
            let gap = beside.iter().find(|x| x.item_a == is_a && x.item == i);
            let across = elsewhere
                .iter()
                .any(|p| if is_a { p.0 == i } else { p.1 == i });
            // The IF or loop around it, when that one is not paired.
            let around = frames[i].0.filter(|&f| !is_paired(f));
            out.unknown_reasons.push(match gap {
                _ if across => format!(
                    "{name} line {}: statement is not paired: the statement it lines up with sits in another IF or loop branch",
                    line(side, &it.nodes)
                ),
                _ if around.is_some() && !ambiguous.contains(&(is_a, i)) => format!(
                    "{name} line {}: statement is not paired: the IF or loop around it (line {}) is not paired",
                    line(side, &it.nodes),
                    line(side, &items[around.unwrap_or(i)].nodes)
                ),
                Some(x) => {
                    let (gname, g) = if is_a {
                        (&nb, &gaps[1][x.gap])
                    } else {
                        (&na, &gaps[0][x.gap])
                    };
                    format!(
                        "{name} line {}: statement is not paired: its counterpart may be in {} at {gname} line {}, which is not compared",
                        line(side, &it.nodes),
                        g.what,
                        g.line
                    )
                }
                None if moved.contains(&(is_a, i)) => format!(
                    "{name} line {}: statement is not paired: the conversion put it in another order among the statements it is paired with",
                    line(side, &it.nodes)
                ),
                None if ambiguous.contains(&(is_a, i)) => format!(
                    "{name} line {}: statement is not paired: the statements around it line up with the other side in more than one way",
                    line(side, &it.nodes)
                ),
                None => format!(
                    "{name} line {}: statement has no counterpart on the other side",
                    line(side, &it.nodes)
                ),
            });
        }
    }
    // A witness is searched only for the one pair that is not proven
    // equal: two changed statements can compensate each other (`x := p + 1;
    // x := x * 2` against `x := p + 2; x := (x - 1) * 2`).
    let all_paired = pairs.len() == ia.len() && pairs.len() == ib.len();
    let unequal = pairs
        .iter()
        .filter(|&&(i, j)| {
            ia[i].err.is_some()
                || ib[j].err.is_some()
                || !compare::compare_across(
                    &ia[i].term,
                    &ib[j].term,
                    a.artifact.dialect,
                    b.artifact.dialect,
                )
                .mismatches
                .is_empty()
        })
        .count();
    let wit = wit.filter(|_| all_paired && unequal == 1);
    for &(i, j) in &pairs {
        let (oa, ob) = (other_terms(ia, i), other_terms(ib, j));
        compare_pair(
            a,
            b,
            &ia[i],
            &ib[j],
            wit.map(|w| (w, [&oa[..], &ob[..]])),
            out,
        );
    }
}

/// One entry of a statement sequence in the IF and loop structure: a
/// statement, an IF or loop with its branches, or a gap.
enum Entry {
    Leaf(usize),
    Flow(Flow),
    Gap(usize),
}

/// An IF or loop: its marker, its branches (the ELSE marker that starts
/// each one after the first) and its END marker.
struct Flow {
    open: usize,
    parts: Vec<(Option<usize>, Vec<Entry>)>,
    end: Option<usize>,
}

/// The sequence of items from `*k` up to the ELSE or END that ends it (or
/// the end of the items), with the gaps that sit in it.
fn flow_seq(items: &[Item], gaps: &[Gap], k: &mut usize, g: &mut usize) -> Vec<Entry> {
    let mut out = Vec::new();
    loop {
        while *g < gaps.len() && gaps[*g].before <= *k {
            out.push(Entry::Gap(*g));
            *g += 1;
        }
        let Some(it) = items.get(*k) else {
            return out;
        };
        match it.term.head() {
            "ctl-else" | "ctl-end" => return out,
            "ctl-if" | "ctl-while" => {
                let open = *k;
                *k += 1;
                let mut parts = vec![(None, flow_seq(items, gaps, k, g))];
                let is_if = it.term.is("ctl-if");
                while is_if && items.get(*k).is_some_and(|x| x.term.is("ctl-else")) {
                    let e = *k;
                    *k += 1;
                    parts.push((Some(e), flow_seq(items, gaps, k, g)));
                }
                let end = items
                    .get(*k)
                    .is_some_and(|x| x.term.is("ctl-end"))
                    .then(|| {
                        *k += 1;
                        *k - 1
                    });
                out.push(Entry::Flow(Flow { open, parts, end }));
            }
            _ => {
                out.push(Entry::Leaf(*k));
                *k += 1;
            }
        }
    }
}

/// Pairs statements level by level of the IF and loop structure: the
/// statements and IF/loop blocks of a sequence are aligned as `align` does
/// (an IF or loop stands as one statement keyed by its kind and number of
/// branches), and the branches of two paired blocks are aligned with each
/// other. A statement is only ever paired inside the branch its counterpart
/// sits in.
fn align_flow(
    ia: &[Item],
    ib: &[Item],
    ka: &[String],
    kb: &[String],
    gaps: &[Vec<Gap>; 2],
) -> Alignment {
    let (mut k, mut g) = (0, 0);
    let mut sa = flow_seq(ia, &gaps[0], &mut k, &mut g);
    // Stray ELSE or END markers (not produced by `lower`) stand alone.
    while k < ia.len() {
        sa.push(Entry::Leaf(k));
        k += 1;
        sa.extend(flow_seq(ia, &gaps[0], &mut k, &mut g));
    }
    let (mut k, mut g) = (0, 0);
    let mut sb = flow_seq(ib, &gaps[1], &mut k, &mut g);
    while k < ib.len() {
        sb.push(Entry::Leaf(k));
        k += 1;
        sb.extend(flow_seq(ib, &gaps[1], &mut k, &mut g));
    }
    let mut out = Alignment::default();
    level(&sa, &sb, ia, ib, ka, kb, &mut out);
    out
}

/// The slots of one level: a statement, or an IF or loop standing as its
/// marker's item, keyed by `keys` (one per entry).
fn level_slots<'k>(seq: &[Entry], keys: &'k [String], items: &[Item]) -> Vec<Slot<'k>> {
    seq.iter()
        .zip(keys)
        .map(|(e, k)| match e {
            Entry::Gap(gi) => Slot::Gap(*gi),
            Entry::Leaf(i) => Slot::Item(*i, k.as_str(), items[*i].err.is_some()),
            Entry::Flow(f) => Slot::Item(f.open, k.as_str(), false),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn level(
    sa: &[Entry],
    sb: &[Entry],
    ia: &[Item],
    ib: &[Item],
    ka: &[String],
    kb: &[String],
    out: &mut Alignment,
) {
    let flow_key =
        |f: &Flow, items: &[Item]| format!("{}/{}", items[f.open].term.head(), f.parts.len());
    let keys_a: Vec<String> = sa
        .iter()
        .map(|e| match e {
            Entry::Leaf(i) => ka[*i].clone(),
            Entry::Flow(f) => flow_key(f, ia),
            Entry::Gap(_) => String::new(),
        })
        .collect();
    let keys_b: Vec<String> = sb
        .iter()
        .map(|e| match e {
            Entry::Leaf(i) => kb[*i].clone(),
            Entry::Flow(f) => flow_key(f, ib),
            Entry::Gap(_) => String::new(),
        })
        .collect();
    let (la, lb) = (level_slots(sa, &keys_a, ia), level_slots(sb, &keys_b, ib));
    let count = |s: &[Slot]| s.iter().filter(|x| matches!(x, Slot::Item(..))).count();
    let al = if count(&la) == count(&lb) {
        let ia_: Vec<usize> = la
            .iter()
            .filter_map(|s| match s {
                Slot::Item(i, ..) => Some(*i),
                Slot::Gap(_) => None,
            })
            .collect();
        let ib_: Vec<usize> = lb
            .iter()
            .filter_map(|s| match s {
                Slot::Item(i, ..) => Some(*i),
                Slot::Gap(_) => None,
            })
            .collect();
        Alignment {
            pairs: ia_.into_iter().zip(ib_).collect(),
            ..Alignment::default()
        }
    } else {
        // Flow keys (`ctl-if/2`) make `align` weigh every best alignment,
        // as gaps do.
        align(&la, &lb)
    };
    out.beside.extend(al.beside);
    out.ambiguous.extend(al.ambiguous);
    let entry = |seq: &'_ [Entry], i: usize| -> Option<usize> {
        seq.iter().position(|e| match e {
            Entry::Leaf(x) => *x == i,
            Entry::Flow(f) => f.open == i,
            Entry::Gap(_) => false,
        })
    };
    for (i, j) in al.pairs {
        out.pairs.push((i, j));
        let (Some(ea), Some(eb)) = (entry(sa, i), entry(sb, j)) else {
            continue;
        };
        if let (Entry::Flow(fa), Entry::Flow(fb)) = (&sa[ea], &sb[eb]) {
            if fa.parts.len() != fb.parts.len()
                || ia[fa.open].term.head() != ib[fb.open].term.head()
            {
                continue;
            }
            for ((ma, pa), (mb, pb)) in fa.parts.iter().zip(&fb.parts) {
                if let (Some(x), Some(y)) = (ma, mb) {
                    out.pairs.push((*x, *y));
                }
                level(pa, pb, ia, ib, ka, kb, out);
            }
            if let (Some(x), Some(y)) = (fa.end, fb.end) {
                out.pairs.push((x, y));
            }
        }
    }
}

/// Where each item sits in the IF and loop structure (`lower::CtlKind`):
/// the marker that opens its branch (an IF, ELSE or WHILE marker), and for
/// an ELSE or END marker the IF or WHILE it belongs to.
fn branches(items: &[Item]) -> Vec<(Option<usize>, Option<usize>)> {
    // (opener, the marker that opens the current branch)
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut out = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        let frame = stack.last().map(|x| x.1);
        match it.term.head() {
            "ctl-if" | "ctl-while" => {
                out.push((frame, None));
                stack.push((i, i));
            }
            "ctl-else" => match stack.pop() {
                Some((opener, _)) => {
                    out.push((stack.last().map(|x| x.1), Some(opener)));
                    stack.push((opener, i));
                }
                None => out.push((None, None)),
            },
            "ctl-end" => match stack.pop() {
                Some((opener, _)) => out.push((stack.last().map(|x| x.1), Some(opener))),
                None => out.push((None, None)),
            },
            _ => out.push((frame, None)),
        }
    }
    out
}

/// Keeps the pairs whose items sit in branches that are paired with each
/// other (and ELSE and END markers whose IF or WHILE markers are paired):
/// a statement inside an IF is never compared with one outside it, or in
/// the other branch, so equal pairs compare the control flow too. Returns
/// the kept pairs and the dropped ones.
type Pairs = Vec<(usize, usize)>;

fn same_branches(ia: &[Item], ib: &[Item], mut pairs: Pairs) -> (Pairs, Pairs) {
    if !ia.iter().chain(ib).any(is_ctl) {
        return (pairs, Vec::new());
    }
    let (fa, fb) = (branches(ia), branches(ib));
    pairs.sort_unstable();
    let mut kept: BTreeMap<usize, usize> = BTreeMap::new();
    let mut dropped = Vec::new();
    // Openers come before what they open, so they are decided first.
    let linked = |x: Option<usize>, y: Option<usize>, kept: &BTreeMap<usize, usize>| match (x, y) {
        (None, None) => true,
        (Some(x), Some(y)) => kept.get(&x) == Some(&y),
        _ => false,
    };
    for (i, j) in pairs {
        if linked(fa[i].0, fb[j].0, &kept) && linked(fa[i].1, fb[j].1, &kept) {
            kept.insert(i, j);
        } else {
            dropped.push((i, j));
        }
    }
    (kept.into_iter().collect(), dropped)
}

/// What matching the two statements' table names relies on. Across
/// dialects: case folding, and default schemas mapping to each other. In one
/// dialect: only a default schema written on one side and left out on the
/// other (`dbo.t` against `t`).
fn name_assumptions(a: &Analyzed, b: &Analyzed, x: &Item, y: &Item) -> Vec<Assumption> {
    let mut v = Vec::new();
    let defaults = |it: &Item| -> Vec<(String, String)> {
        it.names
            .iter()
            .filter_map(|(k, d)| d.clone().map(|d| (k.clone(), d)))
            .collect()
    };
    let (dx, dy) = (defaults(x), defaults(y));
    if a.dialect() != b.dialect() {
        if !x.names.is_empty() || !y.names.is_empty() {
            v.push(asm::make(asm::IDENT_FOLD));
        }
        if !dx.is_empty() || !dy.is_empty() {
            v.push(asm::make(asm::DEFAULT_SCHEMA));
        }
    } else {
        if dx
            .iter()
            .any(|(k, d)| dy.iter().any(|(k2, d2)| k == k2 && d != d2))
        {
            v.push(asm::make(asm::DEFAULT_SCHEMA));
        }
        if a.dialect().lexical().folding == sqldiff_syntax::IdentFolding::Collation
            && respelled(a, b, x, y)
        {
            v.push(asm::make(asm::TSQL_CI_NAMES));
        }
    }
    v
}

/// Whether a name or variable is spelled in more than one case across the
/// two statements (`Orders` and `orders`). Terms fold names, which T-SQL only
/// does under a case-insensitive catalog collation. Counted over both sides
/// together, so a side that mixes spellings, or two sides that swap them,
/// also count.
fn respelled(a: &Analyzed, b: &Analyzed, x: &Item, y: &Item) -> bool {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let toks = x
        .nodes
        .iter()
        .flat_map(|n| crate::normalize::norm_tokens(n, a.dialect()))
        .chain(
            y.nodes
                .iter()
                .flat_map(|n| crate::normalize::norm_tokens(n, b.dialect())),
        );
    for t in toks {
        match seen.get(&t.loose) {
            Some(s) if *s != t.strict => return true,
            Some(_) => {}
            None => {
                seen.insert(t.loose, t.strict);
            }
        }
    }
    false
}

fn compare_pair(
    a: &Analyzed,
    b: &Analyzed,
    x: &Item,
    y: &Item,
    wit: Option<(&Witnessing, [&[&Term]; 2])>,
    out: &mut Outcome,
) {
    let (na, nb) = (side_name(a), side_name(b));
    let la = location(a, &x.nodes);
    let lb = location(b, &y.nodes);
    let anchor = |t: &Term| crate::normalize::hash_strs(std::iter::once(t.to_string().as_str()));
    let push = |out: &mut Outcome,
                rule: &str,
                class: ChangeClass,
                verdict: Verdict,
                witness: String,
                asms: Vec<String>| {
        out.findings.push(Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: &a.object,
                class,
                rule,
                anchor_a: &anchor(&x.term),
                anchor_b: &anchor(&y.term),
            }),
            rule: rule.to_string(),
            class,
            severity: class.default_severity(),
            verdict,
            witness,
            a: Some(la.clone()),
            b: Some(lb.clone()),
            assumptions: asms,
        });
    };
    if (is_ctl(x) || is_ctl(y)) && x.term.head() != y.term.head() {
        let what = |it: &Item| match it.term.head() {
            "ctl-if" => "an IF condition",
            "ctl-while" => "a WHILE condition",
            "ctl-else" => "the start of an ELSE branch",
            "ctl-end" => "the end of an IF or loop",
            "ctl-break" => "a loop exit",
            "ctl-continue" => "a CONTINUE",
            "ctl-call" => "a call",
            _ => "a statement",
        };
        push(
            out,
            "equiv.unproven",
            ChangeClass::Semantic,
            Verdict::Unknown,
            format!(
                "Statements (A line {}, B line {}) could not be shown equivalent: the control flow differs: A has {} where B has {}.",
                la.start_line,
                lb.start_line,
                what(x),
                what(y)
            ),
            Vec::new(),
        );
        return;
    }
    if let Some(e) = x
        .err
        .as_ref()
        .map(|e| (e, &na, line(a, &x.nodes)))
        .or(y.err.as_ref().map(|e| (e, &nb, line(b, &y.nodes))))
    {
        if x.term != y.term {
            push(
                out,
                "equiv.unsupported",
                ChangeClass::Semantic,
                Verdict::Unknown,
                format!(
                    "Statement equivalence was not checked (A line {}, B line {}): on {} line {}: {}.",
                    la.start_line, lb.start_line, e.1, e.2, e.0
                ),
                Vec::new(),
            );
            return;
        }
    }
    let c = compare::compare_across(&x.term, &y.term, a.artifact.dialect, b.artifact.dialect);
    let mut asms: Vec<Assumption> = x.asm.clone();
    for s in name_assumptions(a, b, x, y) {
        if !asms.iter().any(|z| z.id == s.id) {
            asms.push(s);
        }
    }
    for s in y.asm.iter().chain(c.assumptions.iter()) {
        if !asms.iter().any(|z| z.id == s.id) {
            asms.push(s.clone());
        }
    }
    let ids: Vec<String> = asms.iter().map(|s| s.id.clone()).collect();
    if c.mismatches.is_empty() {
        out.equal += 1;
        for r in c.refinements {
            out.refine(r);
        }
        if x.term.is("raw") {
            // Same dialect, same tokens.
        } else {
            out.structural += 1;
        }
        for s in asms {
            out.assume(s);
        }
        for d in x.disc.iter().chain(&y.disc) {
            out.normalize(format!("proven from the catalog: {d}"));
        }
        return;
    }
    let mut explained = Vec::new();
    let mut unexplained = Vec::new();
    for m in &c.mismatches {
        match compare::explain(m, &na, &nb) {
            Some(e) => {
                if !explained
                    .iter()
                    .any(|(_, r, _): &(ChangeClass, &str, String)| *r == e.1)
                {
                    explained.push(e);
                }
            }
            None => unexplained.push(m),
        }
    }
    if !explained.is_empty() {
        for s in &asms {
            out.assume(s.clone());
        }
        for (class, rule, witness) in explained {
            push(out, rule, class, Verdict::Different, witness, ids.clone());
        }
        return;
    }
    if let Some((w, later)) = wit {
        match xwit::search(&x.term, &y.term, &w.a, &w.b, later) {
            Ok(found) => {
                let mut extra = found.assumptions;
                if !w.certain {
                    extra.push(edit::witness_path());
                }
                for s in extra {
                    if !asms.iter().any(|z| z.id == s.id) {
                        asms.push(s);
                    }
                }
                for s in &asms {
                    out.assume(s.clone());
                }
                let ids = asms.iter().map(|s| s.id.clone()).collect();
                push(
                    out,
                    "equiv.witness",
                    ChangeClass::Semantic,
                    Verdict::Different,
                    format!(
                        "Statements (A line {}, B line {}) differ. {}.",
                        la.start_line, lb.start_line, found.witness
                    ),
                    ids,
                );
                return;
            }
            Err(e) => {
                if std::env::var_os("SQLDIFF_DEBUG_TERMS").is_some() {
                    eprintln!("witness: {e}");
                }
            }
        }
    }
    let m = unexplained[0];
    let what = compare::describe_unknown(m, &na, &nb).unwrap_or_else(|| {
        format!(
            "A has `{}` where B has `{}`",
            compare::render(&m.a),
            compare::render(&m.b)
        )
    });
    push(
        out,
        "equiv.unproven",
        ChangeClass::Semantic,
        Verdict::Unknown,
        format!(
            "Statements (A line {}, B line {}) could not be shown equivalent: {what}{}.",
            la.start_line,
            lb.start_line,
            if unexplained.len() > 1 {
                format!(" (+{} more)", unexplained.len() - 1)
            } else {
                String::new()
            }
        ),
        Vec::new(),
    );
}

/// Declared types of the `DECLARE` statements behind an item, in order.
fn declared_types(it: &Item, side: &Analyzed) -> Vec<Option<ast::Type>> {
    it.nodes
        .iter()
        .filter_map(|n| match ast::parse_stmt(&ast::tokens(n), side.dialect()) {
            Ok(ast::Stmt::Declare { ty, .. }) => Some(ty.map(|t| ast::Type {
                name: t.name.to_ascii_uppercase(),
                args: t.args,
            })),
            _ => None,
        })
        .collect()
}

/// Same dialect: re-examines statements the structural diff could only call
/// "modified". Equal ones are dropped (counted as structural), and ones with a
/// known behavioural difference become "different" with a witness.
pub fn refine_modified(
    a: &Analyzed,
    b: &Analyzed,
    sa: &Schema,
    sb: &Schema,
    findings: &mut Vec<Finding>,
    out: &mut Outcome,
) {
    if !findings
        .iter()
        .any(|f| f.rule == "stmt.modified" && f.verdict == Verdict::Unknown)
    {
        return;
    }
    let ua = lower::lower(a);
    let ub = lower::lower(b);
    let ia = items(&ua, sa, true, ub.dialect);
    let ib = items(&ub, sb, true, ua.dialect);
    let find = |items: &[Item], side: Side, f: &Finding| -> Option<usize> {
        let loc = if side == Side::A {
            f.a.as_ref()?
        } else {
            f.b.as_ref()?
        };
        // IF and loop markers are not statements: an END or ELSE marker
        // starts where its whole IF does, which a finding on the IF names.
        items.iter().position(|it| {
            !is_ctl(it)
                && it
                    .nodes
                    .iter()
                    .any(|n| cst::sig_range(n).0 == loc.byte_start)
        })
    };
    let mut keep = Vec::new();
    for f in findings.drain(..) {
        if f.rule != "stmt.modified" || f.verdict != Verdict::Unknown {
            keep.push(f);
            continue;
        }
        let (Some(i), Some(j)) = (find(&ia, Side::A, &f), find(&ib, Side::B, &f)) else {
            keep.push(f);
            continue;
        };
        if declared_types(&ia[i], a) != declared_types(&ib[j], b) {
            // The terms leave out the declared type, which converts the value.
            keep.push(f);
            continue;
        }
        let mut sub = Outcome::default();
        compare_pair(a, b, &ia[i], &ib[j], None, &mut sub);
        if std::env::var_os("SQLDIFF_DEBUG_TERMS").is_some() {
            eprintln!(
                "A: {}{}",
                ia[i].term,
                ia[i]
                    .err
                    .as_ref()
                    .map(|e| format!("  [{e}]"))
                    .unwrap_or_default()
            );
            eprintln!(
                "B: {}{}",
                ib[j].term,
                ib[j]
                    .err
                    .as_ref()
                    .map(|e| format!("  [{e}]"))
                    .unwrap_or_default()
            );
            for g in &sub.findings {
                eprintln!("=> {} {:?} {}", g.rule, g.verdict, g.witness);
            }
        }
        let settled = crate::coverage::Receipt::Judged {
            rule: "statement equivalence",
            a: f.a.as_ref().map(crate::coverage::Part::at),
            b: f.b.as_ref().map(crate::coverage::Part::at),
        };
        if sub.findings.is_empty() {
            out.receipts.push(settled);
            out.structural += 1;
            for s in sub.assumptions {
                out.assume(s);
            }
            for nrm in sub.normalizations {
                out.normalize(nrm);
            }
            for r in sub.refinements {
                out.refine(r);
            }
        } else if sub.findings.iter().any(|g| g.verdict == Verdict::Different) {
            out.receipts.push(settled);
            for s in sub.assumptions {
                out.assume(s);
            }
            keep.extend(
                sub.findings
                    .into_iter()
                    .filter(|g| g.verdict == Verdict::Different),
            );
        } else {
            keep.push(f);
        }
    }
    *findings = keep;
}
