//! Surface terms: the dialect AST as S-expressions, which rules match.
//!
//! Statements and expressions come from the equivalence parser
//! (`equiv::ast`); IF and BEGIN/END come from the CST. The tree stays close
//! to what was written (function names, operators, clause order), with two
//! normalisations that make rule matching alpha-equivalent:
//! - a column qualified by an alias refers to the table (`(col (name t) c)`);
//! - a column qualified by the only table in scope is unqualified.
//!
//! A statement outside the parser's subset becomes `(raw <dialect> tokens...)`.
//!
//! Shapes (optional parts in brackets):
//! ```text
//! (select [(distinct)] [(top e)] (items (item e [alias])...) [(into e...)]
//!         [(from f...)] [(where e)] [(group e...)] [(having e)]
//!         [(order (asc|desc e [nulls-first|nulls-last])...)] [(offset e)] [(limit e)] [(lock w...)])
//! (table (name p...) [(alias a)] [(hints h...)])   (sub q (alias a))   (join kind l r [e])
//! (update (table ...) (set (= (col c) e)...) [(from f...)] [(where e)])
//! (insert (table ...) (cols c...) (values (row e...)...) | (query q) | (default-values)
//!         [(returning (item e [alias])...)] [(returning-into e...)]
//!         [(on-conflict (cols k...) (do-nothing) | (do-update (set ...) [(where e)]))])
//! (delete (table ...) [(from f...)] [(where e)])
//! (merge (table ...) (using f) (on e) (when-matched [(and e)] action)...)
//! (assign target e)  (return [e])  (declare (var v) [(type ...)] [e])
//! (exec [(into (var rc))] (name p...) (arg [(var p)] e)...)  (perform e)
//! (if c then [else])  (block s...)
//! ```
//! Expressions: `(col [q] c)`, `(var @v)`, `(num n)`, `(str s)`, `(null)`,
//! `(star [t])`, `(call f a...)`, `(<op> l r)` for binary operators,
//! `(not e)`, `(neg e)`, `(is-null e)`, `(is-not-null e)`, `(in e (list ...))`,
//! `(in-q e q)`, `(between e lo hi)`, `(like e p)`, `(cast e (type t a...))`,
//! `(case [(operand e)] (when c r)... [(else e)])`, `(exists q)`, `(subq q)`,
//! `(tuple e...)`, `(special w)`, `(extract f e)`.

use sqldiff_syntax::{Dialect, SyntaxKind, SyntaxNode};

use crate::equiv::ast::{
    self as east, Call, ConflictAction, Expr, From, InsertSource, MergeAction, OrderItem, SelItem,
    Select, Stmt, Tok, K,
};
use crate::equiv::term::{a, n, Term};

type R<T> = Result<T, String>;

/// One lowered unit (statement or compound) with its source node.
#[derive(Debug, Clone)]
pub struct Unit {
    pub term: Term,
    pub node: SyntaxNode,
    /// Why the unit is (partly) raw, if it is.
    pub problem: Option<String>,
}

/// Lowers a list of CST units (statements, IF, blocks) in order.
pub fn units(nodes: &[SyntaxNode], d: Dialect) -> Vec<Unit> {
    nodes
        .iter()
        .map(|node| {
            let mut problem = None;
            let term = unit(node, d, &mut problem);
            Unit {
                term,
                node: node.clone(),
                problem,
            }
        })
        .collect()
}

/// The statement-level units directly under a container or block.
pub fn child_units(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    for c in node.children() {
        if c.kind().is_unit() {
            out.push(c);
        } else if c.kind().is_container() {
            out.extend(child_units(&c));
        }
    }
    out
}

fn raw(node: &SyntaxNode, d: Dialect) -> Term {
    n(
        "raw",
        std::iter::once(a(d.id()))
            .chain(east::tokens(node).into_iter().map(|t| a(t.text)))
            .collect(),
    )
}

fn unit(node: &SyntaxNode, d: Dialect, problem: &mut Option<String>) -> Term {
    fn fail(problem: &mut Option<String>, e: String, node: &SyntaxNode, d: Dialect) -> Term {
        problem.get_or_insert(e);
        raw(node, d)
    }
    match node.kind() {
        SyntaxKind::STATEMENT => match statement(node, d) {
            Ok(t) => t,
            Err(e) => fail(problem, e, node, d),
        },
        SyntaxKind::BLOCK => {
            if node.children().any(|c| {
                matches!(
                    c.kind(),
                    SyntaxKind::DECLARE_SECTION | SyntaxKind::EXCEPTION_SECTION
                )
            }) {
                return fail(
                    problem,
                    "blocks with declarations or handlers".into(),
                    node,
                    d,
                );
            }
            let kids = child_units(node)
                .iter()
                .map(|c| unit(c, d, problem))
                .collect();
            n("block", kids)
        }
        SyntaxKind::IF_STMT => match if_stmt(node, d, problem) {
            Ok(t) => t,
            Err(e) => fail(problem, e, node, d),
        },
        k => fail(
            problem,
            format!("{k:?} is not lowered to a surface term"),
            node,
            d,
        ),
    }
}

fn if_stmt(node: &SyntaxNode, d: Dialect, problem: &mut Option<String>) -> R<Term> {
    let conds: Vec<SyntaxNode> = node
        .children()
        .filter(|c| c.kind() == SyntaxKind::CONDITION)
        .collect();
    let branches: Vec<SyntaxNode> = node
        .children()
        .filter(|c| c.kind() == SyntaxKind::BRANCH)
        .collect();
    if conds.len() != 1 || branches.is_empty() || branches.len() > 2 {
        return Err("IF with ELSIF branches".into());
    }
    let toks = east::tokens(&conds[0]);
    let mut p = east::Parser::new(&toks, d);
    let c = p.expr()?;
    if !p.at_end() {
        return Err(p.err("unexpected tokens in the IF condition"));
    }
    let mut kids = vec![Scope::default().expr(&c)?];
    for b in &branches {
        let us = child_units(b);
        let mut ts: Vec<Term> = us.iter().map(|u| unit(u, d, problem)).collect();
        kids.push(if ts.len() == 1 {
            ts.remove(0)
        } else {
            n("block", ts)
        });
    }
    Ok(n("if", kids))
}

fn statement(node: &SyntaxNode, d: Dialect) -> R<Term> {
    let toks = east::tokens(node);
    let first = toks.first().map(|t| t.up.as_str()).unwrap_or("");
    match first {
        "EXEC" | "EXECUTE" if d == Dialect::Tsql => exec(&toks[1..], d),
        "PERFORM" if d == Dialect::Postgres => {
            let mut p = east::Parser::new(&toks[1..], d);
            let e = p.expr()?;
            if !p.at_end() {
                return Err(p.err("unexpected trailing tokens"));
            }
            Ok(n("perform", vec![Scope::default().expr(&e)?]))
        }
        _ => {
            let s = east::parse_stmt(&toks, d)?;
            Scope::default().stmt(&s)
        }
    }
}

/// `EXEC [@rc =] proc [@p =] value, ...`
fn exec(toks: &[Tok], d: Dialect) -> R<Term> {
    let mut i = 0;
    let mut kids = Vec::new();
    if toks.len() > 2 && toks[0].kind == K::Var && toks[1].text == "=" {
        kids.push(n("into", vec![n("var", vec![a(toks[0].text.clone())])]));
        i = 2;
    }
    let mut p = east::Parser::new(&toks[i..], d);
    let name = p.qname()?;
    kids.push(name_term(&name));
    let rest = &toks[i + p.i..];
    for arg in split_commas(rest) {
        if arg.is_empty() {
            continue;
        }
        let (named, value) = if arg.len() > 2 && arg[0].kind == K::Var && arg[1].text == "=" {
            (Some(arg[0].text.clone()), &arg[2..])
        } else {
            (None, arg)
        };
        if value
            .iter()
            .any(|t| t.kind == K::Word && matches!(t.up.as_str(), "OUTPUT" | "OUT"))
        {
            return Err("OUTPUT arguments are not lowered".into());
        }
        let mut p = east::Parser::new(value, d);
        let e = p.expr()?;
        if !p.at_end() {
            return Err(p.err("unexpected tokens in an EXEC argument"));
        }
        let mut k = Vec::new();
        if let Some(v) = named {
            k.push(n("var", vec![a(v)]));
        }
        k.push(Scope::default().expr(&e)?);
        kids.push(n("arg", k));
    }
    Ok(n("exec", kids))
}

fn split_commas(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Comma if depth == 0 => {
                out.push(&toks[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&toks[start..]);
    out
}

pub fn name_term(parts: &[String]) -> Term {
    n("name", parts.iter().map(|p| a(p.clone())).collect())
}

/// One FROM entry visible to column references.
#[derive(Debug, Clone)]
struct Entry {
    /// Lower-case alias, or the last name part.
    key: String,
    /// What a qualified reference becomes: the table's name term, or `(alias a)`.
    target: Term,
}

/// Name scopes, innermost last.
#[derive(Debug, Clone, Default)]
struct Scope {
    frames: Vec<Vec<Entry>>,
}

fn lc(s: &str) -> String {
    s.to_lowercase()
}

impl Scope {
    fn push_from(&mut self, from: &[From]) {
        let mut frame = Vec::new();
        for f in from {
            collect(f, &mut frame);
        }
        // Self-joins keep their aliases: renaming would conflate them. That
        // includes a subquery reading a table an outer query also reads, where
        // a correlated reference must stay apart from the inner table.
        let names: Vec<String> = frame.iter().map(|e| e.target.to_string()).collect();
        for (i, e) in frame.iter_mut().enumerate() {
            let outer = self
                .frames
                .iter_mut()
                .flatten()
                .filter(|o| o.target.to_string() == names[i]);
            let mut clash = false;
            for o in outer {
                o.target = n("alias", vec![a(o.key.clone())]);
                clash = true;
            }
            if clash || names.iter().filter(|x| **x == names[i]).count() > 1 {
                e.target = n("alias", vec![a(e.key.clone())]);
            }
        }
        self.frames.push(frame);
    }

    fn col(&self, parts: &[String]) -> Term {
        let (c, q) = parts.split_last().unwrap();
        if q.is_empty() {
            return n("col", vec![a(c.clone())]);
        }
        if q.len() == 1 {
            let key = lc(&q[0]);
            for (depth, frame) in self.frames.iter().enumerate().rev() {
                if let Some(e) = frame.iter().find(|e| e.key == key) {
                    let innermost = depth + 1 == self.frames.len();
                    if innermost && frame.len() == 1 && !e.target.is("alias") {
                        return n("col", vec![a(c.clone())]);
                    }
                    return n("col", vec![e.target.clone(), a(c.clone())]);
                }
            }
        }
        n("col", vec![name_term(q), a(c.clone())])
    }

    fn expr(&mut self, e: &Expr) -> R<Term> {
        Ok(match e {
            Expr::Name(p) => self.col(p),
            Expr::Var(v) => n("var", vec![a(v.clone())]),
            Expr::Num(s) => n("num", vec![a(s.clone())]),
            Expr::Str(s) => n("str", vec![a(s.clone())]),
            Expr::Null => n("null", vec![]),
            Expr::Star(None) => n("star", vec![]),
            Expr::Star(Some(t)) => n("star", vec![a(t.clone())]),
            Expr::Call(c) => self.call(c)?,
            Expr::Bin(op, l, r) => n(op, vec![self.expr(l)?, self.expr(r)?]),
            Expr::Unary(op, x) => match op.as_str() {
                "NOT" => n("not", vec![self.expr(x)?]),
                "-" => n("neg", vec![self.expr(x)?]),
                _ => n("unary", vec![a(op.clone()), self.expr(x)?]),
            },
            Expr::IsNull(x, neg) => n(
                if *neg { "is-not-null" } else { "is-null" },
                vec![self.expr(x)?],
            ),
            Expr::InList(x, l, neg) => n(
                if *neg { "not-in" } else { "in" },
                vec![
                    self.expr(x)?,
                    n("list", l.iter().map(|y| self.expr(y)).collect::<R<_>>()?),
                ],
            ),
            Expr::InQuery(x, q, neg) => n(
                if *neg { "not-in-q" } else { "in-q" },
                vec![self.expr(x)?, self.select(q)?],
            ),
            Expr::Between(x, lo, hi, neg) => n(
                if *neg { "not-between" } else { "between" },
                vec![self.expr(x)?, self.expr(lo)?, self.expr(hi)?],
            ),
            Expr::Like(x, p, neg, ilike) => n(
                match (neg, ilike) {
                    (false, false) => "like",
                    (true, false) => "not-like",
                    (false, true) => "ilike",
                    (true, true) => "not-ilike",
                },
                vec![self.expr(x)?, self.expr(p)?],
            ),
            Expr::Cast(x, t) => n("cast", vec![self.expr(x)?, type_term(t)]),
            Expr::Case(op, whens, els) => {
                let mut k = Vec::new();
                if let Some(o) = op {
                    k.push(n("operand", vec![self.expr(o)?]));
                }
                for (c, r) in whens {
                    k.push(n("when", vec![self.expr(c)?, self.expr(r)?]));
                }
                if let Some(e) = els {
                    k.push(n("else", vec![self.expr(e)?]));
                }
                n("case", k)
            }
            Expr::Exists(q) => n("exists", vec![self.select(q)?]),
            Expr::Subquery(q) => n("subq", vec![self.select(q)?]),
            Expr::Tuple(l) => n("tuple", l.iter().map(|y| self.expr(y)).collect::<R<_>>()?),
            Expr::Special(s) => n("special", vec![a(s.clone())]),
            Expr::Extract(f, x) => n("extract", vec![a(f.clone()), self.expr(x)?]),
        })
    }

    fn call(&mut self, c: &Call) -> R<Term> {
        if c.over {
            return Err("window functions are not lowered".into());
        }
        let mut k = vec![a(c
            .name
            .iter()
            .map(|x| lc(x))
            .collect::<Vec<_>>()
            .join("."))];
        if c.distinct {
            k.push(n("distinct", vec![]));
        }
        if c.star {
            k.push(n("star", vec![]));
        }
        for x in &c.args {
            k.push(self.expr(x)?);
        }
        if !c.order.is_empty() {
            let head = if c.within_group {
                "within-group"
            } else {
                "order"
            };
            k.push(n(head, self.order(&c.order)?));
        }
        Ok(n("call", k))
    }

    fn order(&mut self, items: &[OrderItem]) -> R<Vec<Term>> {
        items
            .iter()
            .map(|o| {
                let mut k = vec![self.expr(&o.expr)?];
                match o.nulls_first {
                    Some(true) => k.push(a("nulls-first")),
                    Some(false) => k.push(a("nulls-last")),
                    None => {}
                }
                Ok(n(if o.desc { "desc" } else { "asc" }, k))
            })
            .collect()
    }

    fn from(&mut self, f: &From) -> R<Term> {
        Ok(match f {
            From::Table { name, alias, hints } => table_term(name, alias, hints),
            From::Sub { query, alias, cols } => {
                let mut k = vec![Scope::default().select(query)?];
                if let Some(al) = alias {
                    k.push(n("alias", vec![a(al.clone())]));
                }
                if !cols.is_empty() {
                    k.push(n("cols", cols.iter().map(|c| a(c.clone())).collect()));
                }
                n("sub", k)
            }
            From::Values { rows, alias, cols } => {
                let mut k = Vec::new();
                for r in rows {
                    k.push(n("row", r.iter().map(|x| self.expr(x)).collect::<R<_>>()?));
                }
                if let Some(al) = alias {
                    k.push(n("alias", vec![a(al.clone())]));
                }
                if !cols.is_empty() {
                    k.push(n("cols", cols.iter().map(|c| a(c.clone())).collect()));
                }
                n("values-src", k)
            }
            From::DataChange { .. } => return Err("data-change table references".into()),
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                let mut k = vec![a(lc(kind)), self.from(left)?, self.from(right)?];
                if let Some(e) = on {
                    k.push(self.expr(e)?);
                }
                n("join", k)
            }
        })
    }

    fn select(&mut self, s: &Select) -> R<Term> {
        if let Some(rows) = &s.values {
            let mut k = Vec::new();
            for r in rows {
                k.push(n("row", r.iter().map(|x| self.expr(x)).collect::<R<_>>()?));
            }
            return Ok(n("values", k));
        }
        self.push_from(&s.from);
        let r = self.select_inner(s);
        self.frames.pop();
        r
    }

    fn select_inner(&mut self, s: &Select) -> R<Term> {
        let mut k = Vec::new();
        if s.distinct {
            k.push(n("distinct", vec![]));
        }
        if let Some(t) = &s.top {
            k.push(n("top", vec![self.expr(t)?]));
        }
        let mut items = Vec::new();
        for it in &s.items {
            items.push(self.item(it)?);
        }
        k.push(n("items", items));
        if !s.into.is_empty() {
            k.push(n(
                if s.into_strict { "into-strict" } else { "into" },
                s.into.iter().map(|x| self.expr(x)).collect::<R<_>>()?,
            ));
        }
        if !s.from.is_empty() {
            let f = s.from.iter().map(|x| self.from(x)).collect::<R<_>>()?;
            k.push(n("from", f));
        }
        if let Some(w) = &s.where_ {
            k.push(n("where", vec![self.expr(w)?]));
        }
        if !s.group_by.is_empty() {
            k.push(n(
                "group",
                s.group_by.iter().map(|x| self.expr(x)).collect::<R<_>>()?,
            ));
        }
        if let Some(h) = &s.having {
            k.push(n("having", vec![self.expr(h)?]));
        }
        if !s.order_by.is_empty() {
            k.push(n("order", self.order(&s.order_by)?));
        }
        if let Some(o) = &s.offset {
            k.push(n("offset", vec![self.expr(o)?]));
        }
        if let Some(l) = &s.limit {
            k.push(n("limit", vec![self.expr(l)?]));
        }
        if !s.locking.is_empty() {
            k.push(n("lock", s.locking.iter().map(|x| a(x.clone())).collect()));
        }
        Ok(n("select", k))
    }

    fn item(&mut self, it: &SelItem) -> R<Term> {
        let e = self.expr(&it.expr)?;
        if let Some(v) = &it.assign {
            return Ok(n("assign-item", vec![n("var", vec![a(v.clone())]), e]));
        }
        let mut k = vec![e];
        if let Some(al) = &it.alias {
            k.push(a(al.clone()));
        }
        Ok(n("item", k))
    }

    fn sets(&mut self, sets: &[(Vec<String>, Expr)]) -> R<Term> {
        let mut kv = Vec::new();
        for (cols, e) in sets {
            let lhs = match cols.as_slice() {
                [c] => n("col", vec![a(c.clone())]),
                cs => n("cols", cs.iter().map(|c| a(c.clone())).collect()),
            };
            kv.push(n("=", vec![lhs, self.expr(e)?]));
        }
        Ok(n("set", kv))
    }

    /// The target table of UPDATE/DELETE, which T-SQL may name by a FROM alias.
    fn target(target: &[String], alias: &Option<String>, from: &[From]) -> (Term, bool) {
        if alias.is_none() && target.len() == 1 {
            let key = lc(&target[0]);
            let mut frame = Vec::new();
            for f in from {
                collect_raw(f, &mut frame);
            }
            if let Some((name, al)) = frame
                .into_iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, n, a)| (n, a))
            {
                return (table_term(&name, &al, &[]), true);
            }
        }
        (table_term(target, alias, &[]), false)
    }

    fn stmt(&mut self, s: &Stmt) -> R<Term> {
        Ok(match s {
            Stmt::Select(q) => self.select(q)?,
            Stmt::Insert {
                target,
                cols,
                source,
                returning,
                returning_into,
                on_conflict,
            } => {
                let t = table_term(target, &None, &[]);
                self.frames.push(vec![Entry {
                    key: lc(target.last().unwrap()),
                    target: t.kids()[0].clone(),
                }]);
                let mut k = vec![t, n("cols", cols.iter().map(|c| a(c.clone())).collect())];
                k.push(match source {
                    InsertSource::Values(rows) => {
                        let mut rs = Vec::new();
                        for r in rows {
                            rs.push(n("row", r.iter().map(|x| self.expr(x)).collect::<R<_>>()?));
                        }
                        n("values", rs)
                    }
                    InsertSource::Query(q) => n("query", vec![Scope::default().select(q)?]),
                    InsertSource::Default => n("default-values", vec![]),
                });
                if !returning.is_empty() {
                    let items = returning.iter().map(|x| self.item(x)).collect::<R<_>>()?;
                    k.push(n("returning", items));
                }
                if !returning_into.is_empty() {
                    k.push(n(
                        "returning-into",
                        returning_into
                            .iter()
                            .map(|x| self.expr(x))
                            .collect::<R<_>>()?,
                    ));
                }
                if let Some((keys, action)) = on_conflict {
                    let act = match action {
                        ConflictAction::Nothing => n("do-nothing", vec![]),
                        ConflictAction::Update(sets, w) => {
                            let s: Vec<(Vec<String>, Expr)> = sets
                                .iter()
                                .map(|(c, e)| (vec![c.clone()], e.clone()))
                                .collect();
                            let mut kk = vec![self.sets(&s)?];
                            if let Some(w) = w {
                                kk.push(n("where", vec![self.expr(w)?]));
                            }
                            n("do-update", kk)
                        }
                    };
                    k.push(n(
                        "on-conflict",
                        vec![n("cols", keys.iter().map(|c| a(c.clone())).collect()), act],
                    ));
                }
                self.frames.pop();
                n("insert", k)
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => {
                let (t, in_from) = Self::target(target, alias, from);
                let mut all = vec![];
                if !in_from {
                    all.push(From::Table {
                        name: target.clone(),
                        alias: alias.clone(),
                        hints: vec![],
                    });
                }
                all.extend(from.iter().cloned());
                self.push_from(&all);
                let r: R<Term> = (|| {
                    let mut k = vec![t, self.sets(sets)?];
                    if !from.is_empty() {
                        k.push(n(
                            "from",
                            from.iter().map(|x| self.from(x)).collect::<R<_>>()?,
                        ));
                    }
                    if let Some(w) = where_ {
                        k.push(n("where", vec![self.expr(w)?]));
                    }
                    Ok(n("update", k))
                })();
                self.frames.pop();
                r?
            }
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => {
                let (t, in_from) = Self::target(target, alias, from);
                let mut all = vec![];
                if !in_from {
                    all.push(From::Table {
                        name: target.clone(),
                        alias: alias.clone(),
                        hints: vec![],
                    });
                }
                all.extend(from.iter().cloned());
                self.push_from(&all);
                let r: R<Term> = (|| {
                    let mut k = vec![t];
                    if !from.is_empty() {
                        k.push(n(
                            "from",
                            from.iter().map(|x| self.from(x)).collect::<R<_>>()?,
                        ));
                    }
                    if let Some(w) = where_ {
                        k.push(n("where", vec![self.expr(w)?]));
                    }
                    Ok(n("delete", k))
                })();
                self.frames.pop();
                r?
            }
            Stmt::Merge {
                target,
                alias,
                hints,
                source,
                on,
                whens,
            } => {
                let all = vec![
                    From::Table {
                        name: target.clone(),
                        alias: alias.clone(),
                        hints: vec![],
                    },
                    source.clone(),
                ];
                let src = self.from(source)?;
                self.push_from(&all);
                let r: R<Term> = (|| {
                    let mut k = vec![
                        table_term(target, alias, hints),
                        n("using", vec![src]),
                        n("on", vec![self.expr(on)?]),
                    ];
                    for w in whens {
                        let head = match (w.matched, w.by_source) {
                            (true, _) => "when-matched",
                            (false, false) => "when-not-matched",
                            (false, true) => "when-not-matched-by-source",
                        };
                        let mut kk = Vec::new();
                        if let Some(c) = &w.cond {
                            kk.push(n("and", vec![self.expr(c)?]));
                        }
                        kk.push(match &w.action {
                            MergeAction::Update(sets) => {
                                let s: Vec<(Vec<String>, Expr)> = sets
                                    .iter()
                                    .map(|(c, e)| (vec![c.clone()], e.clone()))
                                    .collect();
                                let set = self.sets(&s)?;
                                n("update-set", set.kids().to_vec())
                            }
                            MergeAction::Delete => n("delete-row", vec![]),
                            MergeAction::Insert(cols, vals) => n(
                                "insert-row",
                                vec![
                                    n("cols", cols.iter().map(|c| a(c.clone())).collect()),
                                    n("row", vals.iter().map(|x| self.expr(x)).collect::<R<_>>()?),
                                ],
                            ),
                        });
                        k.push(n(head, kk));
                    }
                    Ok(n("merge", k))
                })();
                self.frames.pop();
                r?
            }
            Stmt::Assign(t, e) => n("assign", vec![self.expr(t)?, self.expr(e)?]),
            Stmt::Return(None) => n("return", vec![]),
            Stmt::Return(Some(e)) => n("return", vec![self.expr(e)?]),
            Stmt::Declare { name, ty, default } => {
                let mut k = vec![n("var", vec![a(name.clone())])];
                if let Some(t) = ty {
                    k.push(type_term(t));
                }
                if let Some(e) = default {
                    k.push(self.expr(e)?);
                }
                n("declare", k)
            }
        })
    }
}

fn type_term(t: &east::Type) -> Term {
    let mut k = vec![a(lc(&t.name))];
    k.extend(t.args.iter().map(|x| a(x.clone())));
    n("type", k)
}

fn table_term(name: &[String], alias: &Option<String>, hints: &[String]) -> Term {
    let mut k = vec![name_term(name)];
    if let Some(al) = alias {
        k.push(n("alias", vec![a(al.clone())]));
    }
    if !hints.is_empty() {
        k.push(n("hints", hints.iter().map(|h| a(lc(h))).collect()));
    }
    n("table", k)
}

fn collect(f: &From, out: &mut Vec<Entry>) {
    match f {
        From::Table { name, alias, .. } => out.push(Entry {
            key: lc(alias.as_ref().unwrap_or_else(|| name.last().unwrap())),
            target: name_term(name),
        }),
        From::Sub {
            alias: Some(al), ..
        }
        | From::Values {
            alias: Some(al), ..
        } => out.push(Entry {
            key: lc(al),
            target: n("alias", vec![a(al.clone())]),
        }),
        From::DataChange {
            alias: Some(al), ..
        } => out.push(Entry {
            key: lc(al),
            target: n("alias", vec![a(al.clone())]),
        }),
        From::Join { left, right, .. } => {
            collect(left, out);
            collect(right, out);
        }
        _ => {}
    }
}

/// FROM tables as (key, name, alias).
fn collect_raw(f: &From, out: &mut Vec<(String, Vec<String>, Option<String>)>) {
    match f {
        From::Table { name, alias, .. } => out.push((
            lc(alias.as_ref().unwrap_or_else(|| name.last().unwrap())),
            name.clone(),
            alias.clone(),
        )),
        From::Join { left, right, .. } => {
            collect_raw(left, out);
            collect_raw(right, out);
        }
        _ => {}
    }
}

/// Lowers a whole artifact text: every top-level unit of every batch and
/// routine body, in order.
pub fn lower_text(text: &str, d: Dialect) -> Vec<Unit> {
    let p = sqldiff_syntax::parse(text, d);
    let root = p.syntax();
    units(&body_units(&root), d)
}

/// Statement-level units of a file: routine bodies are opened, so the units
/// of `CREATE PROCEDURE ... AS BEGIN ... END` are the statements inside.
pub fn body_units(root: &SyntaxNode) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    for u in child_units(root) {
        if u.kind() == SyntaxKind::ROUTINE {
            for b in u
                .children()
                .filter(|c| c.kind() == SyntaxKind::ROUTINE_BODY)
            {
                for x in child_units(&b) {
                    if x.kind() == SyntaxKind::BLOCK
                        && !x.children().any(|c| {
                            matches!(
                                c.kind(),
                                SyntaxKind::DECLARE_SECTION | SyntaxKind::EXCEPTION_SECTION
                            )
                        })
                    {
                        out.extend(child_units(&x));
                    } else {
                        out.push(x);
                    }
                }
            }
        } else {
            out.push(u);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(sql: &str, d: Dialect) -> String {
        let u = lower_text(sql, d);
        assert_eq!(u.len(), 1, "{u:?}");
        assert!(u[0].problem.is_none(), "{:?}", u[0].problem);
        crate::rules::sexp::write(&u[0].term)
    }

    #[test]
    fn aliases_are_alpha_normalised() {
        assert_eq!(
            one("SELECT c.name FROM dbo.customer c WHERE c.id = @Id;", Dialect::Tsql),
            "(select (items (item (col name))) (from (table (name dbo customer) (alias c))) (where (= (col id) (var @Id))))"
        );
        let s = one(
            "UPDATE item_price p SET unit_price = (SELECT f.unit_price FROM price_feed f WHERE f.item_id = p.item_id);",
            Dialect::Db2i,
        );
        assert!(
            s.contains("(= (col (name item_price) item_id)")
                || s.contains("(= (col item_id) (col (name item_price) item_id))"),
            "{s}"
        );
    }

    #[test]
    fn if_and_exec() {
        let u = lower_text(
            "IF EXISTS (SELECT 1 FROM t WHERE id = @id) UPDATE t SET x = 1 WHERE id = @id; ELSE INSERT INTO t (id, x) VALUES (@id, 1);\nEXEC @rc = sp_getapplock @Resource = 'r', @LockMode = 'Exclusive';",
            Dialect::Tsql,
        );
        assert_eq!(u.len(), 2);
        assert!(u[0].term.is("if"));
        assert_eq!(
            crate::rules::sexp::write(&u[1].term),
            "(exec (into (var @rc)) (name sp_getapplock) (arg (var @Resource) (str r)) (arg (var @LockMode) (str Exclusive)))"
        );
    }
}
