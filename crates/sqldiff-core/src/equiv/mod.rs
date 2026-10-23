//! Statement-level semantic equivalence (design section 3, layer 5 item 3).
//!
//! Each side's routine is lowered to an ordered list of canonical statement
//! terms (`canon`), paired in order and compared (`compare`). A pair is
//! equal, equal under the assumptions its rewrites needed, different with a
//! witness, or unknown with the reason. Control flow other than the
//! recognised transaction protocols is reported as not covered, so the
//! verdict never claims more than was compared.

pub mod ast;
pub mod canon;
pub mod compare;
pub mod lower;
pub mod schema;
pub mod term;

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
    pair_items(a, b, &ia, &ib, &mut out);
    error_continues(a, b, &ua, &ub, &ia, &ib, &mut out);
    out
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
    let Some((i, j)) = (0..items.len()).find_map(|i| {
        if inner(&items[i]) {
            return Some((i, i));
        }
        (i + 1..items.len())
            .find(|&j| effect(&items[j].term))
            .map(|j| (i, j))
    }) else {
        return;
    };
    let (x, y) = (&items[i], &items[j]);
    let lx = location(tside, &x.nodes[..1]);
    let ly = location(tside, &y.nodes[y.nodes.len() - 1..]);
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
    let anchor = crate::normalize::hash_strs(std::iter::once(x.term.to_string().as_str()));
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
        match (&x.default, &y.default) {
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

fn key(t: &Term) -> String {
    format!(
        "{}:{}",
        t.head(),
        t.kid(0).map(|k| k.to_string()).unwrap_or_default()
    )
}

fn pair_items(a: &Analyzed, b: &Analyzed, ia: &[Item], ib: &[Item], out: &mut Outcome) {
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
    for (i, j) in pairs {
        compare_pair(a, b, &ia[i], &ib[j], out);
    }
}

fn compare_pair(a: &Analyzed, b: &Analyzed, x: &Item, y: &Item, out: &mut Outcome) {
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
        let mut sub = Outcome::default();
        compare_pair(a, b, &ia[i], &ib[j], &mut sub);
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
