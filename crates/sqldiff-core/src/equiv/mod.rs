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
pub mod edit;
pub mod eval;
pub mod literal;
pub mod lower;
pub mod options;
pub mod schema;
pub mod table;
pub mod term;
pub mod xwit;

use sqldiff_syntax::{ast as cst, SyntaxNode};

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
}

fn side_name(a: &Analyzed) -> String {
    format!("side {:?} ({})", a.side, a.dialect().display_name())
}

/// Lowers every data statement of a unit.
fn items(unit: &Unit, schema: &Schema, by_name: bool) -> Vec<Item> {
    let mut c = Canon::new(unit, schema);
    c.by_name = by_name;
    let mut out = Vec::new();
    let mut stmts = unit.stmts.iter().peekable();
    while let Some(s) = stmts.next() {
        c.reset();
        let mut nodes = vec![s.node.clone()];
        let res = match &s.ast {
            Err(e) => Err(e.clone()),
            // T-SQL inline table function: `RETURN` and its SELECT are two CST statements.
            Ok(ast::Stmt::Return(None)) if unit.kind == UnitKind::TableFn => match stmts.peek() {
                Some(next) => match &next.ast {
                    Ok(ast::Stmt::Select(q)) => {
                        nodes.push(next.node.clone());
                        stmts.next();
                        c.stmt(&ast::Stmt::Return(Some(ast::Expr::Subquery(q.clone()))))
                    }
                    _ => c.stmt(&ast::Stmt::Return(None)),
                },
                None => c.stmt(&ast::Stmt::Return(None)),
            },
            Ok(st) => c.stmt(st),
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

/// Cross-dialect comparison of two artifacts.
pub fn compare(a: &Analyzed, b: &Analyzed, sa: &Schema, sb: &Schema) -> Outcome {
    let mut out = Outcome::default();
    let ua = lower::lower(a);
    let ub = lower::lower(b);
    let (na, nb) = (side_name(a), side_name(b));
    for (side, u, name) in [(a, &ua, &na), (b, &ub, &nb)] {
        for (node, what) in &u.uncovered {
            out.unknown_reasons.push(format!(
                "{name} line {}: {what} is not covered by statement-level equivalence",
                side.line_of(node)
            ));
        }
        for note in &u.notes {
            out.normalize(note.clone());
        }
    }
    signature(&ua, &ub, &na, &nb, &mut out);
    tables(a, b, &ua, &ub, sa, sb, &mut out);
    calls(a, b, &ua, &ub, sa, sb, &mut out);
    for x in ua.assumptions.iter().chain(&ub.assumptions) {
        out.assume(x.clone());
    }
    if ua.txn_control || ub.txn_control {
        out.assume(asm::make(asm::NO_CALLER_TXN));
    }
    let ia = items(&ua, sa, false);
    let ib = items(&ub, sb, false);
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
    security(a, b, &ua, &ub, touches, &mut out);
    // Witnesses across dialects compare parameters by position, so the
    // routines must take the same parameters.
    // Statements outside the comparison could change or undo the witness.
    let wit = (ua.kind == ub.kind
        && ua.params.len() == ub.params.len()
        && ua.uncovered.is_empty()
        && ub.uncovered.is_empty()
        // A called procedure can touch the witness's table.
        && ua.calls.is_empty()
        && ub.calls.is_empty())
    .then(|| Witnessing {
        a: xwit::Side {
            unit: &ua,
            schema: sa,
            name: na.clone(),
        },
        b: xwit::Side {
            unit: &ub,
            schema: sb,
            name: nb.clone(),
        },
        // One statement on each side and nothing around it: the witness
        // inputs reach it and its result is the routine's.
        certain: ia.len() == 1 && ib.len() == 1 && !ua.txn_control && !ub.txn_control,
    });
    pair_items(a, b, &ia, &ib, wit.as_ref(), &mut out);
    error_continues(a, b, &ua, &ub, &ia, &ib, &mut out);
    out
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
    let lowered =
        |toks: &[ast::Tok], u: &Unit, s: &Schema| -> Result<(Term, Vec<Assumption>), String> {
            let st = ast::parse_stmt(toks, u.dialect)?;
            let mut c = Canon::new(u, s);
            let t = c.stmt(&st)?;
            Ok((t, std::mem::take(&mut c.asm)))
        };
    let (tx, mut asms) = match lowered(x, ua, sa) {
        Ok(v) => v,
        Err(e) => return Same::Unknown(format!("{na}: {e}")),
    };
    let (ty, by) = match lowered(y, ub, sb) {
        Ok(v) => v,
        Err(e) => return Same::Unknown(format!("{nb}: {e}")),
    };
    let c = compare::compare(&tx, &ty);
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
                    let (sa2, sb2) = sch.get_or_init(|| {
                        (
                            sa.with_ddl(&xa.text().to_string(), a.dialect()),
                            sb.with_ddl(&xb.text().to_string(), b.dialect()),
                        )
                    });
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
    if t.stops_on_error
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
    // Data statements and calls in text order: a call can fail, and it
    // has an effect (the procedure's, or the comment).
    #[derive(Clone, Copy)]
    enum E {
        Item(usize),
        Call(usize),
    }
    let mut seq: Vec<(usize, E)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            (
                it.nodes.first().map_or(0, |n| cst::sig_range(n).0),
                E::Item(i),
            )
        })
        .chain(
            t.calls
                .iter()
                .enumerate()
                // Inside a procedure; how a script goes on after a failed
                // statement depends on the client running it.
                .filter(|(_, (_, at))| at.outside.is_none() && t.kind == UnitKind::Proc)
                .map(|(i, (n, _))| (cst::sig_range(n).0, E::Call(i))),
        )
        .collect();
    seq.sort_by_key(|x| x.0);
    let is_effect = |e: E| match e {
        E::Item(i) => effect(&items[i].term),
        E::Call(_) => true,
    };
    let Some((i, j)) = (0..seq.len()).find_map(|i| {
        if let E::Item(k) = seq[i].1 {
            if inner(&items[k]) {
                return Some((i, i));
            }
        }
        (i + 1..seq.len())
            .find(|&j| is_effect(seq[j].1))
            .map(|j| (i, j))
    }) else {
        return;
    };
    let nodes = |e: E| -> Vec<SyntaxNode> {
        match e {
            E::Item(k) => items[k].nodes.clone(),
            E::Call(k) => vec![t.calls[k].0.clone()],
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
        E::Call(_) => xn[0].text().to_string(),
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

fn pair_items(
    a: &Analyzed,
    b: &Analyzed,
    ia: &[Item],
    ib: &[Item],
    wit: Option<&Witnessing>,
    out: &mut Outcome,
) {
    let pairs: Vec<(usize, usize)> = if ia.len() == ib.len() {
        (0..ia.len()).map(|i| (i, i)).collect()
    } else {
        let ka: Vec<String> = ia.iter().map(|x| key(&x.term)).collect();
        let kb: Vec<String> = ib.iter().map(|x| key(&x.term)).collect();
        crate::normalize::lcs_pairs(&ka, &kb)
    };
    let (na, nb) = (side_name(a), side_name(b));
    for (side, items, name, is_a) in [(a, ia, &na, true), (b, ib, &nb, false)] {
        for (i, it) in items.iter().enumerate() {
            let paired = pairs.iter().any(|p| if is_a { p.0 == i } else { p.1 == i });
            if !paired {
                out.unknown_reasons.push(format!(
                    "{name} line {}: statement has no counterpart on the other side",
                    line(side, &it.nodes)
                ));
            }
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
                || !compare::compare(&ia[i].term, &ib[j].term)
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
    } else if dx
        .iter()
        .any(|(k, d)| dy.iter().any(|(k2, d2)| k == k2 && d != d2))
    {
        v.push(asm::make(asm::DEFAULT_SCHEMA));
    }
    v
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
    let c = compare::compare(&x.term, &y.term);
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
    let ia = items(&ua, sa, true);
    let ib = items(&ub, sb, true);
    let find = |items: &[Item], side: Side, f: &Finding| -> Option<usize> {
        let loc = if side == Side::A {
            f.a.as_ref()?
        } else {
            f.b.as_ref()?
        };
        items.iter().position(|it| {
            it.nodes
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
        if sub.findings.is_empty() {
            out.structural += 1;
            for s in sub.assumptions {
                out.assume(s);
            }
            for nrm in sub.normalizations {
                out.normalize(nrm);
            }
        } else if sub.findings.iter().any(|g| g.verdict == Verdict::Different) {
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
