//! Which conditions of two routines are the same fact about the input
//! (perf plan W6c, aligned merging, step 1: E0 and E1; design
//! `aligned-merge-design.md`).
//!
//! **Report only.** Nothing on the diff path reads the result: it is stored
//! in `Outcome::identity` and printed by the `identity` example. A later
//! step may let the behaviour explorer treat two conditions as one decision;
//! it may do so only for a condition this analysis identified.
//!
//! # What is proven
//!
//! A pair of `IF` / `WHILE` conditions is **identified** when
//!
//! 1. the two `ctl-if` / `ctl-while` items are paired and the statement
//!    level proved them equal (`compare_across` finds no mismatch, no item
//!    carries a reading error). That equality is *relational*: the terms are
//!    equal if the variables they name hold equal values;
//! 2. the term reads only values the analysis knows (`value_class`): no
//!    clock, random value, sequence, row count or other term that is not a
//!    function of the inputs; and
//! 3. every parameter and local the term reads holds the same value on both
//!    sides at the two conditions (E0, below).
//!
//! A condition that reads the database (`EXISTS`, a subquery, a column) is
//! **state-identified** at best: equal only if the databases are equal at
//! both points, which this analysis does not know. No consumer may use it.
//!
//! # E0: value-equal variables
//!
//! Reaching definitions per side, over the structured sequence of statements
//! and `IF` / `WHILE` / `TRY` markers. A variable at a point is equal to its
//! partner on the other side only if
//!
//! * each side has exactly one reaching definition, and
//! * both are the entry value of paired parameters whose declared types
//!   store values alike and whose defaults agree, or both are "declared
//!   without a value", or both are plain assignments (`SET @v = e`,
//!   `v := e`, `SELECT @v = e` without `FROM`) whose items are paired and
//!   proven equal, whose values are value terms, and whose own inputs are
//!   equal at the two assignments, recursively.
//!
//! Everything else ends the fact, and the allow-list is the only way in:
//! `SELECT @v = col FROM t`, `SELECT ... INTO`, `FETCH ... INTO`, `EXEC ...
//! OUTPUT`, `UPDATE ... SET @v = ...`, a statement the parser did not read,
//! any assignment in a CATCH or handler body, a variable a call, dynamic
//! SQL or an uncovered construct mentions anywhere. A loop-carried variable
//! has two reaching definitions and is unknown.
//!
//! **Scope: fault-free runs.** A failed assignment leaves the old value (and
//! continues) in T-SQL, and leaves for the handler in PostgreSQL. The
//! analysis says nothing about runs with a fault; the report states it.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use sqldiff_syntax::{ast as cst, Dialect};

use super::ast::{tokens, Expr, Stmt, K};
use super::canon::Item;
use super::lower::{Ctl, CtlKind, LStmt, Unit};
use super::term::Term;
use super::{canon, compare, script};
use crate::artifact::Analyzed;

/// How a pair of conditions was classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    /// Identified; the term reads only parameters, equal locals, literals
    /// and deterministic built-ins.
    Value,
    /// The term reads the database as well: equal only if the database
    /// state is equal at both points. Not usable.
    State,
    /// The test of the statement just before it (`rows-found`,
    /// `cursor-found`): identified by that statement's key.
    Rows,
    /// Not identified, and why.
    No(String),
}

impl Class {
    /// Usable by a consumer: identified by value or by the statement a row
    /// test follows. A state-identified condition is not (it is equal only
    /// if the database is equal at both points).
    pub fn identified(&self) -> bool {
        matches!(self, Class::Value | Class::Rows)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Class::Value => "value",
            Class::State => "state",
            Class::Rows => "rows",
            Class::No(_) => "no",
        }
    }
}

/// One paired condition.
#[derive(Debug, Clone)]
pub struct CondRow {
    /// `if` or `while`.
    pub kind: &'static str,
    pub a_line: usize,
    pub b_line: usize,
    /// Byte offset where the condition starts on each side (the offset of
    /// the `CONDITION` node a control-flow graph test node has).
    pub a_start: usize,
    pub b_start: usize,
    pub class: Class,
    /// Assumptions the identification rests on, sorted.
    pub assumptions: Vec<String>,
}

/// The conditions of one routine pair.
#[derive(Debug, Clone, Default)]
pub struct UnitReport {
    /// IF / WHILE conditions on each side (paired or not).
    pub conds_a: usize,
    pub conds_b: usize,
    /// Paired conditions, in the order of side A.
    pub rows: Vec<CondRow>,
    /// Why the analysis gave up for the whole routine pair, when it did.
    pub refused: Option<String>,
}

/// Names the report's scope, for every consumer to read.
pub const SCOPE: &str = "fault-free runs only";

type Var = usize;

/// A definition that reaches a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Def {
    /// The entry value of a parameter.
    Param,
    /// Declared without a value (NULL).
    Implicit,
    /// A plain assignment; the event.
    Assign(usize),
    /// Written by something the analysis does not model.
    Unknown(usize),
    /// A value an earlier iteration of the loop may have written.
    Loop(usize),
    /// The state at an error handler: any prefix of the TRY part ran.
    Handler(usize),
    /// Not declared yet.
    Undefined,
}

type Defs = Rc<Vec<Def>>;
type State = Rc<Vec<Defs>>;

fn join(a: &State, b: &State) -> State {
    if Rc::ptr_eq(a, b) {
        return a.clone();
    }
    let out: Vec<Defs> = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| {
            if Rc::ptr_eq(x, y) || x == y {
                x.clone()
            } else {
                let mut v: Vec<Def> = x.iter().chain(y.iter()).copied().collect();
                v.sort();
                v.dedup();
                Rc::new(v)
            }
        })
        .collect();
    Rc::new(out)
}

/// One step of the routine in document order.
enum Ev<'a> {
    Ctl(&'a Ctl),
    Stmt(&'a LStmt),
}

/// The structured form of the event sequence.
enum Blk {
    Ev(usize),
    If {
        cond: usize,
        then: Vec<Blk>,
        els: Option<Vec<Blk>>,
    },
    While {
        cond: usize,
        body: Vec<Blk>,
    },
    Try {
        at: usize,
        body: Vec<Blk>,
        catch: Vec<Blk>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    If,
    While,
    Try,
}

struct Frame {
    kind: Open,
    at: usize,
    /// The blocks of the enclosing part, which `cur` resumes at the END.
    parent: Vec<Blk>,
    /// The part before the ELSE / CATCH, once seen.
    first: Option<Vec<Blk>>,
}

/// The events of a unit in the order `items()` visits them.
fn events(u: &Unit) -> Vec<Ev<'_>> {
    let mut out = Vec::new();
    let mut stmts = u.stmts.iter().enumerate().peekable();
    let mut ctl = u.ctl.iter().peekable();
    loop {
        let next = stmts.peek().map_or(usize::MAX, |(i, _)| *i);
        while let Some(m) = ctl.next_if(|m| m.after <= next) {
            out.push(Ev::Ctl(m));
        }
        let Some((_, s)) = stmts.next() else { break };
        out.push(Ev::Stmt(s));
    }
    out
}

fn structure(ev: &[Ev<'_>]) -> Option<Vec<Blk>> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut cur: Vec<Blk> = Vec::new();
    for (i, e) in ev.iter().enumerate() {
        let Ev::Ctl(m) = e else {
            cur.push(Blk::Ev(i));
            continue;
        };
        let open = match &m.kind {
            CtlKind::If(_) | CtlKind::RowsIf(..) => Some(Open::If),
            CtlKind::While(_) | CtlKind::FetchWhile(_) => Some(Open::While),
            CtlKind::Try => Some(Open::Try),
            _ => None,
        };
        if let Some(kind) = open {
            stack.push(Frame {
                kind,
                at: i,
                parent: std::mem::take(&mut cur),
                first: None,
            });
            continue;
        }
        match &m.kind {
            CtlKind::Else | CtlKind::Catch => {
                let want = if matches!(m.kind, CtlKind::Else) {
                    Open::If
                } else {
                    Open::Try
                };
                let f = stack.last_mut()?;
                if f.kind != want || f.first.is_some() {
                    return None;
                }
                f.first = Some(std::mem::take(&mut cur));
            }
            CtlKind::End => {
                let f = stack.pop()?;
                let last = std::mem::take(&mut cur);
                let (first, second) = match f.first {
                    Some(first) => (first, Some(last)),
                    None => (last, None),
                };
                cur = f.parent;
                cur.push(match f.kind {
                    Open::If => Blk::If {
                        cond: f.at,
                        then: first,
                        els: second,
                    },
                    Open::While => Blk::While {
                        cond: f.at,
                        body: first,
                    },
                    Open::Try => Blk::Try {
                        at: f.at,
                        body: first,
                        catch: second.unwrap_or_default(),
                    },
                });
            }
            _ => cur.push(Blk::Ev(i)),
        }
    }
    stack.is_empty().then_some(cur)
}

/// What an event does to a variable.
enum Write {
    /// A plain assignment from the allow-list.
    Plain(Var),
    /// A declaration without a value.
    Implicit(Var),
    /// Something else: the variable's value is unknown afterwards.
    Kill(Var),
}

/// One side of the pair.
struct Side<'a> {
    unit: &'a Unit,
    items: &'a [Item],
    ev: Vec<Ev<'a>>,
    /// The item an event belongs to, and whether it is the item's first
    /// event (the rest were absorbed by it).
    item_of: Vec<Option<(usize, bool)>>,
    /// Variable names: parameters (`p1`, ...), then locals by their id.
    names: Vec<String>,
    /// Definitions that reach each event (kept for conditions and plain
    /// assignments, which are what a query asks about).
    before: Vec<Option<State>>,
    /// Variables a call, dynamic SQL or an uncovered construct mentions.
    tainted: BTreeSet<Var>,
}

fn sym_names(u: &Unit) -> Vec<String> {
    let mut v: Vec<String> = (0..u.params.len()).map(|i| format!("p{}", i + 1)).collect();
    for i in 0..u.locals.len() {
        v.push(match &u.local_ids {
            Some(ids) => ids[i].clone(),
            None => format!("l{}", i + 1),
        });
    }
    v
}

impl<'a> Side<'a> {
    /// The variable a name stands for (the rule of `Canon::sym`).
    fn var_of(&self, name: &str) -> Option<Var> {
        if name.starts_with("@@") {
            return None;
        }
        let n0 = name.trim_start_matches('@').to_lowercase();
        if let Some(i) = self.unit.params.iter().position(|p| p.name == n0) {
            return Some(i);
        }
        self.unit
            .locals
            .iter()
            .position(|l| l.name == n0)
            .map(|i| self.unit.params.len() + i)
    }

    fn var_of_sym(&self, sym: &str) -> Option<Var> {
        self.names.iter().position(|n| n == sym)
    }

    /// Every variable the tokens name.
    fn mentioned(&self, toks: &[super::ast::Tok]) -> BTreeSet<Var> {
        let mut out = BTreeSet::new();
        for t in toks {
            if matches!(t.kind, K::Var | K::Word | K::Quoted | K::Param) {
                let name = if t.kind == K::Quoted {
                    cst::unquote(&t.text).to_string()
                } else {
                    t.text.clone()
                };
                if let Some(v) = self.var_of(&name) {
                    out.insert(v);
                }
            }
        }
        out
    }

    /// The base variable of an assignment target that is not a bare name.
    fn mentioned_target(&self, e: &Expr) -> BTreeSet<Var> {
        let mut out = BTreeSet::new();
        match e {
            Expr::Name(p) => out.extend(p.first().and_then(|n| self.var_of(n))),
            Expr::Var(n) => out.extend(self.var_of(n)),
            _ => {}
        }
        out
    }

    /// Variables the tokens may write with an assignment inside a data
    /// statement (`UPDATE t SET @v = c`, `SELECT @v = c FROM t`).
    fn token_writes(&self, toks: &[super::ast::Tok]) -> BTreeSet<Var> {
        let mut out = BTreeSet::new();
        for (i, t) in toks.iter().enumerate() {
            if t.kind != K::Var || t.text.starts_with("@@") {
                continue;
            }
            let assigns = toks.get(i + 1).is_some_and(|n| {
                n.kind == K::Op
                    && matches!(
                        n.text.as_str(),
                        "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^="
                    )
            });
            let at_item = i == 0
                || toks.get(i - 1).is_some_and(|p| {
                    p.kind == K::Comma || matches!(p.up.as_str(), "SET" | "SELECT")
                });
            if assigns && at_item {
                if let Some(v) = self.var_of(&t.text) {
                    out.insert(v);
                }
            }
        }
        out
    }

    fn writes(&self, e: usize, in_catch: bool) -> Vec<Write> {
        let mut out = Vec::new();
        match &self.ev[e] {
            Ev::Ctl(m) => {
                if matches!(m.kind, CtlKind::Call) {
                    for v in self.mentioned(&tokens(&m.node)) {
                        out.push(Write::Kill(v));
                    }
                }
            }
            Ev::Stmt(s) => {
                let mut plain: Option<Var> = None;
                let mut kills: BTreeSet<Var> = self.token_writes(&s.toks);
                match &s.ast {
                    Ok(st) if s.msg.is_none() && s.cursor.is_none() => {
                        if let Some((t, _)) = st.assignment() {
                            match var_expr(&t) {
                                Ok(n) => plain = self.var_of(&n),
                                // `r.f := e`, `a[1] := e`: writes part of a
                                // variable, which is unknown afterwards.
                                Err(()) => kills.extend(self.mentioned_target(&t)),
                            }
                        }
                        match st {
                            Stmt::Declare { name, default, .. } => {
                                if let Some(v) = self.var_of(name) {
                                    if default.is_none() {
                                        // PL/pgSQL initialises a variable at
                                        // each entry to its block; a T-SQL
                                        // DECLARE does not reset it in a loop.
                                        if self.unit.dialect == Dialect::Tsql {
                                            out.push(Write::Kill(v));
                                        } else {
                                            out.push(Write::Implicit(v));
                                        }
                                    } else {
                                        plain = Some(v);
                                    }
                                }
                            }
                            Stmt::Select(q) => {
                                for it in &q.items {
                                    if let Some(a) = &it.assign {
                                        if let Some(v) = self.var_of(a) {
                                            kills.insert(v);
                                        }
                                    }
                                }
                                for x in &q.into {
                                    if let Ok(n) = var_expr(x) {
                                        if let Some(v) = self.var_of(&n) {
                                            kills.insert(v);
                                        }
                                    }
                                }
                            }
                            Stmt::Insert { returning_into, .. } => {
                                for x in returning_into {
                                    if let Ok(n) = var_expr(x) {
                                        if let Some(v) = self.var_of(&n) {
                                            kills.insert(v);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // A statement the parser did not read (or a cursor or
                    // message statement) may write any variable it names.
                    _ => kills.extend(self.mentioned(&s.toks)),
                }
                if let Some(v) = plain {
                    // Only an item the statement level paired as a plain
                    // assignment counts; the rest is decided by `plain_item`.
                    kills.remove(&v);
                    let ok = !in_catch && self.plain_item(e, v).is_some();
                    if ok {
                        out.push(Write::Plain(v));
                    } else {
                        out.push(Write::Kill(v));
                    }
                }
                for v in kills {
                    out.push(Write::Kill(v));
                }
            }
        }
        if in_catch {
            for w in &mut out {
                if let Write::Plain(v) | Write::Implicit(v) = *w {
                    *w = Write::Kill(v);
                }
            }
        }
        out
    }

    /// The assignment item of event `e` when it is a plain assignment of
    /// variable `v` that the statement level read without an error.
    fn plain_item(&self, e: usize, v: Var) -> Option<&'a Item> {
        let (k, first) = self.item_of[e]?;
        if !first {
            return None;
        }
        let it = &self.items[k];
        if it.err.is_some() || !it.term.is("assign") {
            return None;
        }
        let target = it.term.kid(0)?;
        (target.is("sym") && target.kid(0)?.atom()? == self.names[v]).then_some(it)
    }

    fn assigned_in(&self, blks: &[Blk], in_catch: bool, out: &mut BTreeSet<Var>) {
        for b in blks {
            match b {
                Blk::Ev(e) => {
                    for w in self.writes(*e, in_catch) {
                        let (Write::Plain(v) | Write::Implicit(v) | Write::Kill(v)) = w;
                        out.insert(v);
                    }
                }
                Blk::If { then, els, .. } => {
                    self.assigned_in(then, in_catch, out);
                    if let Some(e) = els {
                        self.assigned_in(e, in_catch, out);
                    }
                }
                Blk::While { body, .. } => self.assigned_in(body, in_catch, out),
                Blk::Try { body, catch, .. } => {
                    self.assigned_in(body, in_catch, out);
                    self.assigned_in(catch, true, out);
                }
            }
        }
    }

    fn walk(&mut self, blks: &[Blk], st: &mut State, in_catch: bool) {
        for b in blks {
            match b {
                Blk::Ev(e) => {
                    self.before[*e] = Some(st.clone());
                    for w in self.writes(*e, in_catch) {
                        let (v, d) = match w {
                            Write::Plain(v) => (v, Def::Assign(*e)),
                            Write::Implicit(v) => (v, Def::Implicit),
                            Write::Kill(v) => (v, Def::Unknown(*e)),
                        };
                        Rc::make_mut(st)[v] = Rc::new(vec![d]);
                    }
                }
                Blk::If { cond, then, els } => {
                    self.before[*cond] = Some(st.clone());
                    let mut a = st.clone();
                    self.walk(then, &mut a, in_catch);
                    let mut b2 = st.clone();
                    if let Some(e) = els {
                        self.walk(e, &mut b2, in_catch);
                    }
                    *st = join(&a, &b2);
                }
                Blk::While { cond, body } => {
                    let mut assigned = BTreeSet::new();
                    self.assigned_in(body, in_catch, &mut assigned);
                    let mut entry = st.clone();
                    for v in &assigned {
                        let mut d: Vec<Def> = entry[*v].to_vec();
                        d.push(Def::Loop(*cond));
                        d.sort();
                        d.dedup();
                        Rc::make_mut(&mut entry)[*v] = Rc::new(d);
                    }
                    self.before[*cond] = Some(entry.clone());
                    let mut inner = entry.clone();
                    self.walk(body, &mut inner, in_catch);
                    *st = join(&entry, &inner);
                }
                Blk::Try { at, body, catch } => {
                    let mut assigned = BTreeSet::new();
                    self.assigned_in(body, in_catch, &mut assigned);
                    let mut handler = st.clone();
                    for v in &assigned {
                        let mut d: Vec<Def> = handler[*v].to_vec();
                        d.push(Def::Handler(*at));
                        d.sort();
                        d.dedup();
                        Rc::make_mut(&mut handler)[*v] = Rc::new(d);
                    }
                    let mut ok = st.clone();
                    self.walk(body, &mut ok, in_catch);
                    self.walk(catch, &mut handler, true);
                    *st = join(&ok, &handler);
                }
            }
        }
    }
}

/// The name a target expression stands for, when it is a variable or a bare
/// name (PL/pgSQL variables are bare names).
fn var_expr(e: &Expr) -> Result<String, ()> {
    match e {
        Expr::Var(n) => Ok(n.clone()),
        Expr::Name(p) if p.len() == 1 => Ok(p[0].clone()),
        _ => Err(()),
    }
}

/// Builds one side: events, the item each belongs to, variables, taint.
fn side<'a>(u: &'a Unit, items: &'a [Item]) -> Result<Side<'a>, String> {
    let ev = events(u);
    // Items and events line up one to one, except that an item may have
    // absorbed later statements (`Canon::lift`).
    let mut item_of: Vec<Option<(usize, bool)>> = vec![None; ev.len()];
    let mut e = 0;
    for (k, it) in items.iter().enumerate() {
        let ctl = it.term.head().starts_with("ctl-");
        if e >= ev.len() {
            return Err("the items run past the statements".into());
        }
        match (&ev[e], ctl) {
            (Ev::Ctl(_), true) => {
                item_of[e] = Some((k, true));
                e += 1;
            }
            (Ev::Stmt(s), false) => {
                if !it.nodes.first().is_some_and(|n| *n == s.node) {
                    return Err("an item does not start at its statement".into());
                }
                item_of[e] = Some((k, true));
                e += 1;
                while e < ev.len() {
                    match &ev[e] {
                        Ev::Stmt(s2) if it.nodes[1..].contains(&s2.node) => {
                            item_of[e] = Some((k, false));
                            e += 1;
                        }
                        _ => break,
                    }
                }
            }
            _ => return Err("items and statements do not line up".into()),
        }
    }
    if e != ev.len() {
        return Err("statements are left without an item".into());
    }
    let names = sym_names(u);
    let n = names.len();
    let mut s = Side {
        unit: u,
        items,
        before: vec![None; ev.len()],
        ev,
        item_of,
        names,
        tainted: BTreeSet::new(),
    };
    // Calls, dynamic SQL and constructs outside the comparison may write any
    // variable they name, anywhere.
    let mut nodes: Vec<&sqldiff_syntax::SyntaxNode> = u.calls.iter().map(|(n, _)| n).collect();
    nodes.extend(u.dynamic.iter().map(|(n, _)| n));
    nodes.extend(u.uncovered.iter().map(|(n, _)| n));
    for node in nodes {
        let t = s.mentioned(&tokens(node));
        s.tainted.extend(t);
    }
    let blocks = structure(&s.ev).ok_or("IF / loop markers do not nest")?;
    let undefined: Defs = Rc::new(vec![Def::Undefined]);
    let mut st: State = Rc::new((0..n).map(|_| undefined.clone()).collect());
    for i in 0..u.params.len() {
        Rc::make_mut(&mut st)[i] = Rc::new(vec![Def::Param]);
    }
    s.walk(&blocks, &mut st, false);
    Ok(s)
}

/// The kind of term a condition or value is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Value,
    State,
    Rows,
    No(String),
}

/// Heads of terms that are functions of the inputs alone.
const VALUE_HEADS: &[&str] = &[
    "num",
    "str",
    "bytes",
    "hexlit",
    "flt",
    "bool",
    "none",
    "empty",
    "sym",
    "cmp",
    "cmp-untyped",
    "cmp-real",
    "cmp-conv",
    "cmp-fold-mixed",
    "cmp-dialect",
    "strcmp",
    "strcmp-order",
    "coll",
    "coll-eq",
    "coll-like",
    "collate",
    "arith",
    "add",
    "mul",
    "neg",
    "not",
    "and",
    "or",
    "in",
    "between",
    "case",
    "when",
    "isnull",
    "concat",
    "concat-char",
    "concat-bin",
    "substr",
    "strlen",
    "charlen",
    "strpos",
    "trim",
    "strreplace",
    "repeat",
    "tostr",
    "todate",
    "numstr",
    "datepart",
    "dpart",
    "days",
    "daydiff",
    "date-add",
    "datefmt",
    "extract",
    "sqrt",
    "list",
    "tuple",
    "likepat",
    "likeescaped",
    "booltext",
    "dneg",
    "iso126",
    "type-error",
];

/// Heads of terms that read the database.
const STATE_HEADS: &[&str] = &[
    "exists",
    "q",
    "subq",
    "scalar",
    "col",
    "inq",
    "catalog-exists",
];

/// `fn` names that are deterministic functions of their arguments.
const VALUE_FNS: &[&str] = &[
    "coalesce", "nullif", "abs", "floor", "ceil", "ceiling", "round", "lower", "upper", "ltrim",
    "rtrim", "len", "length", "left", "right", "greatest", "least", "sign", "power", "mod",
];

fn kind_of(t: &Term) -> Kind {
    if t.is("rows-found") || t.is("cursor-found") {
        return Kind::Rows;
    }
    let mut state = false;
    let mut bad: Option<String> = None;
    // Inside a query only `fresh` (a random value) is refused: the term is
    // state-identified at best, and no consumer may use it.
    fn go(t: &Term, inside: bool, state: &mut bool, bad: &mut Option<String>) {
        let Term::N(h, kids) = t else { return };
        let mut inside = inside;
        if inside {
            if h == "fresh" && bad.is_none() {
                *bad = Some("the query draws a random value".to_string());
            }
        } else if h == "fn" {
            let name = kids.first().and_then(|k| k.atom()).unwrap_or("?");
            if !VALUE_FNS.contains(&name) && bad.is_none() {
                *bad = Some(format!(
                    "the function `{name}` is not known to be deterministic"
                ));
            }
        } else if STATE_HEADS.contains(&h.as_str()) {
            *state = true;
            inside = true;
        } else if !VALUE_HEADS.contains(&h.as_str()) && bad.is_none() {
            *bad = Some(format!(
                "the term `{h}` is not a function of the inputs (clock, random value, sequence, count or an unmodelled form)"
            ));
        }
        for k in kids {
            go(k, inside, state, bad);
        }
    }
    go(t, false, &mut state, &mut bad);
    match bad {
        Some(b) => Kind::No(b),
        None if state => Kind::State,
        None => Kind::Value,
    }
}

fn syms(t: &Term, out: &mut BTreeSet<String>) {
    if t.is("sym") {
        if let Some(s) = t.kid(0).and_then(|k| k.atom()) {
            out.insert(s.to_string());
        }
        return;
    }
    for k in t.kids() {
        syms(k, out);
    }
}

type Asms = BTreeSet<String>;

struct Analysis<'a> {
    a: Side<'a>,
    b: Side<'a>,
    /// Item pairs (side A item, side B item).
    pairs: BTreeMap<usize, usize>,
    /// Per variable (by side A index): what the declared types allow.
    var_ok: Vec<Result<Asms, String>>,
    da: Dialect,
    db: Dialect,
    pair_memo: BTreeMap<(usize, usize), Result<Asms, String>>,
    memo: BTreeMap<(Var, usize, usize), Result<Asms, String>>,
}

const DEPTH: usize = 400;

impl Analysis<'_> {
    /// Whether the statement level proved items `i` (A) and `j` (B) equal,
    /// and the assumptions it did so under.
    fn pair_equal(&mut self, i: usize, j: usize) -> Result<Asms, String> {
        if self.pairs.get(&i) != Some(&j) {
            return Err("the items are not paired".into());
        }
        if let Some(r) = self.pair_memo.get(&(i, j)) {
            return r.clone();
        }
        let (x, y) = (&self.a.items[i], &self.b.items[j]);
        let r = if x.err.is_some() || y.err.is_some() {
            Err("an item was not read by the statement level".to_string())
        } else {
            let c = compare::compare_across(&x.term, &y.term, self.da, self.db);
            if c.mismatches.is_empty() {
                Ok(x.asm
                    .iter()
                    .chain(&y.asm)
                    .chain(&c.assumptions)
                    .map(|s| s.id.clone())
                    .collect())
            } else {
                Err("the statement level did not prove the items equal".to_string())
            }
        };
        self.pair_memo.insert((i, j), r.clone());
        r
    }

    /// Variable `v` holds the same value at event `ea` on side A and `eb`
    /// on side B.
    fn same(&mut self, v: Var, ea: usize, eb: usize, depth: usize) -> Result<Asms, String> {
        if let Some(r) = self.memo.get(&(v, ea, eb)) {
            return r.clone();
        }
        let r = self.same_at(v, ea, eb, depth);
        self.memo.insert((v, ea, eb), r.clone());
        r
    }

    fn same_at(&mut self, v: Var, ea: usize, eb: usize, depth: usize) -> Result<Asms, String> {
        let name = self.a.names[v].clone();
        if depth > DEPTH {
            return Err(format!("the chain of definitions of {name} is too long"));
        }
        let vb = self
            .b
            .var_of_sym(&name)
            .ok_or_else(|| format!("{name} does not exist on side B"))?;
        if self.a.tainted.contains(&v) || self.b.tainted.contains(&vb) {
            return Err(format!(
                "{name} is named by a call, dynamic SQL or a construct outside the comparison"
            ));
        }
        let mut asms = match self.var_ok.get(v) {
            Some(Ok(a)) => a.clone(),
            Some(Err(e)) => return Err(e.clone()),
            None => return Err(format!("{name} is not paired")),
        };
        let (Some(sa), Some(sb)) = (&self.a.before[ea], &self.b.before[eb]) else {
            return Err("no definitions are known at the point".into());
        };
        let (da, db) = (sa[v].clone(), sb[vb].clone());
        let (&[x], &[y]) = (&da[..], &db[..]) else {
            return Err(format!("{name} has more than one reaching definition"));
        };
        match (x, y) {
            (Def::Param, Def::Param) | (Def::Implicit, Def::Implicit) => Ok(asms),
            (Def::Assign(xa), Def::Assign(xb)) => {
                let (Some((ia, _)), Some((ib, _))) = (self.a.item_of[xa], self.b.item_of[xb])
                else {
                    return Err("an assignment has no item".into());
                };
                asms.extend(self.pair_equal(ia, ib)?);
                let (ta, tb) = (&self.a.items[ia].term, &self.b.items[ib].term);
                let (Some(va), Some(vb2)) = (ta.kid(1), tb.kid(1)) else {
                    return Err("an assignment has no value".into());
                };
                for t in [va, vb2] {
                    match kind_of(t) {
                        Kind::Value => {}
                        Kind::State => {
                            return Err(format!("{name} is assigned from the database"));
                        }
                        Kind::Rows => return Err(format!("{name} is assigned a row test")),
                        Kind::No(why) => return Err(format!("{name}: {why}")),
                    }
                }
                let mut reads = BTreeSet::new();
                syms(va, &mut reads);
                syms(vb2, &mut reads);
                for r in reads {
                    let Some(rv) = self.a.var_of_sym(&r) else {
                        return Err(format!("{r} is not a variable of side A"));
                    };
                    asms.extend(self.same(rv, xa, xb, depth + 1)?);
                }
                Ok(asms)
            }
            (Def::Unknown(_), _) | (_, Def::Unknown(_)) => Err(format!(
                "{name} is written by a statement the analysis does not model"
            )),
            (Def::Loop(_), _) | (_, Def::Loop(_)) => {
                Err(format!("{name} is changed by the loop around it"))
            }
            (Def::Handler(_), _) | (_, Def::Handler(_)) => {
                Err(format!("{name} is read after an error handler"))
            }
            (Def::Undefined, _) | (_, Def::Undefined) => Err(format!("{name} is not defined yet")),
            _ => Err(format!("{name} is defined differently on the two sides")),
        }
    }
}

fn bit_bool(x: super::schema::Ty, y: super::schema::Ty) -> bool {
    super::bit_bool(x, y)
}

/// A default that is a number, a string, NULL or a boolean: a value that does
/// not depend on when or where the routine runs.
fn literal_default(d: &str) -> bool {
    let d = d
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    let up = d.to_uppercase();
    d.parse::<f64>().is_ok()
        || (d.len() >= 2
            && d.starts_with('\'')
            && d.ends_with('\'')
            && !d[1..d.len() - 1].contains('\''))
        || matches!(up.as_str(), "NULL" | "TRUE" | "FALSE")
}

/// What the declared types and defaults of paired variables allow.
fn variable_facts(ua: &Unit, ub: &Unit, sa: &Side<'_>) -> Vec<Result<Asms, String>> {
    let mut out: Vec<Result<Asms, String>> = vec![Err("not paired".to_string()); sa.names.len()];
    let db2 = ua.dialect == Dialect::Db2i || ub.dialect == Dialect::Db2i;
    for (i, (x, y)) in ua.params.iter().zip(&ub.params).enumerate() {
        out[i] = (|| {
            if db2 {
                return Err("Db2 for i has no verified variable-type facts".to_string());
            }
            if x.out || y.out {
                return Err("an output parameter".to_string());
            }
            if x.default != y.default {
                return Err("the parameter defaults differ".to_string());
            }
            if x.default.as_deref().is_some_and(|d| !literal_default(d)) {
                return Err("a parameter default is not a literal".to_string());
            }
            if ua.dialect == ub.dialect {
                return (x.decl == y.decl && x.ty.is_some())
                    .then(Asms::new)
                    .ok_or_else(|| "the declared types differ".to_string());
            }
            let (Some(tx), Some(ty)) = (x.ty, y.ty) else {
                return Err("a parameter type was not read".to_string());
            };
            let pg = [(ua.dialect, tx), (ub.dialect, ty)]
                .into_iter()
                .find(|(d, _)| *d == Dialect::Postgres);
            if pg.is_some_and(|(_, t)| canon::untypmod(t) != t) {
                return Err(
                    "PostgreSQL ignores a parameter type's length, precision and scale".to_string(),
                );
            }
            (tx == ty || bit_bool(tx, ty))
                .then(Asms::new)
                .ok_or_else(|| "the declared parameter types store values differently".to_string())
        })();
    }
    for (x, y) in script::local_pairs(ua, ub) {
        let (Some(i), Some(j)) = (
            ua.locals.iter().position(|l| std::ptr::eq(l, x)),
            ub.locals.iter().position(|l| std::ptr::eq(l, y)),
        ) else {
            continue;
        };
        let (na, nb) = (
            &sym_names(ua)[ua.params.len() + i],
            &sym_names(ub)[ub.params.len() + j],
        );
        if na != nb {
            continue;
        }
        let k = ua.params.len() + i;
        out[k] = if db2 {
            Err("Db2 for i has no verified variable-type facts".to_string())
        } else if ua.dialect == ub.dialect {
            (x.decl == y.decl && x.decl.is_some())
                .then(Asms::new)
                .ok_or_else(|| "the declared types differ".to_string())
        } else {
            script::local_pair(ua, ub, x, y)
                .map(|v| v.into_iter().map(|(id, _)| id.to_string()).collect())
        };
    }
    out
}

/// A label or `GOTO`: control flow the structured walk does not follow.
fn jumps(u: &Unit) -> Option<String> {
    let goto = |t: &[super::ast::Tok]| t.first().is_some_and(|f| f.up == "GOTO");
    let hit = u.uncovered.iter().any(|(n, what)| {
        what.contains("label") || goto(&tokens(n)) || tokens(n).iter().any(|t| t.up == "GOTO")
    }) || u.stmts.iter().any(|s| goto(&s.toks));
    hit.then(|| "a label or GOTO makes control flow the analysis does not follow".to_string())
}

/// The analysis of one routine pair. `pairs` are the item pairs
/// `pair_items` chose.
pub(super) fn analyze(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    ia: &[Item],
    ib: &[Item],
    pairs: &[(usize, usize)],
) -> UnitReport {
    let mut rep = UnitReport::default();
    let count = |items: &[Item]| {
        items
            .iter()
            .filter(|i| matches!(i.term.head(), "ctl-if" | "ctl-while"))
            .count()
    };
    rep.conds_a = count(ia);
    rep.conds_b = count(ib);
    if rep.conds_a == 0 && rep.conds_b == 0 {
        return rep;
    }
    // A label with a GOTO is a loop the structured walk does not see.
    if let Some(why) = jumps(ua).or_else(|| jumps(ub)) {
        rep.refused = Some(why);
        return rep;
    }
    let both = (side(ua, ia), side(ub, ib));
    let (sa, sb) = match both {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => {
            rep.refused = Some(e);
            return rep;
        }
    };
    let var_ok = variable_facts(ua, ub, &sa);
    let mut an = Analysis {
        a: sa,
        b: sb,
        pairs: pairs.iter().copied().collect(),
        var_ok,
        da: ua.dialect,
        db: ub.dialect,
        pair_memo: BTreeMap::new(),
        memo: BTreeMap::new(),
    };
    // Event of each condition item, per side.
    // `item_of` is fixed when a side is built and `classify` never changes
    // it, so one index per side stays valid for every pair.
    let (first_a, first_b) = (first_event_of(&an.a.item_of), first_event_of(&an.b.item_of));
    for &(i, j) in pairs {
        let (x, y) = (&ia[i], &ib[j]);
        let kind = match x.term.head() {
            "ctl-if" => "if",
            "ctl-while" => "while",
            _ => continue,
        };
        if y.term.head() != x.term.head() {
            continue;
        }
        let (Some(&ea), Some(&eb)) = (first_a.get(&i), first_b.get(&j)) else {
            continue;
        };
        let (Ev::Ctl(ma), Ev::Ctl(mb)) = (&an.a.ev[ea], &an.b.ev[eb]) else {
            continue;
        };
        let (ma, mb): (&Ctl, &Ctl) = (ma, mb);
        let (class, assumptions) = classify(&mut an, x, y, i, j, ea, eb);
        rep.rows.push(CondRow {
            kind,
            a_line: a.line_of(&ma.node),
            b_line: b.line_of(&mb.node),
            a_start: cst::sig_range(&ma.node).0,
            b_start: cst::sig_range(&mb.node).0,
            class,
            assumptions: assumptions.into_iter().collect(),
        });
    }
    rep.rows.sort_by_key(|r| (r.a_start, r.b_start));
    rep
}

/// For each item `k`, the first event index `e` with `item_of[e] ==
/// Some((k, true))` (the same event `position` would find).
fn first_event_of(item_of: &[Option<(usize, bool)>]) -> BTreeMap<usize, usize> {
    let mut m = BTreeMap::new();
    for (e, x) in item_of.iter().enumerate() {
        if let Some((k, true)) = *x {
            m.entry(k).or_insert(e);
        }
    }
    m
}

fn classify(
    an: &mut Analysis<'_>,
    x: &Item,
    y: &Item,
    i: usize,
    j: usize,
    ea: usize,
    eb: usize,
) -> (Class, Asms) {
    let no = |s: String| (Class::No(s), Asms::new());
    let mut asms = match an.pair_equal(i, j) {
        Ok(a) => a,
        Err(e) => return no(e),
    };
    let (ka, kb) = (
        kind_of(x.term.kid(0).unwrap_or(&x.term)),
        kind_of(y.term.kid(0).unwrap_or(&y.term)),
    );
    let class = match (ka, kb) {
        (Kind::Rows, Kind::Rows) => return (Class::Rows, asms),
        (Kind::No(w), _) | (_, Kind::No(w)) => return no(w),
        (Kind::Rows, _) | (_, Kind::Rows) => return no("one side tests rows".into()),
        (Kind::State, _) | (_, Kind::State) => Class::State,
        (Kind::Value, Kind::Value) => Class::Value,
    };
    let mut reads = BTreeSet::new();
    syms(&x.term, &mut reads);
    syms(&y.term, &mut reads);
    for r in reads {
        let Some(v) = an.a.var_of_sym(&r) else {
            return no(format!("{r} is not a variable of side A"));
        };
        match an.same(v, ea, eb, 0) {
            Ok(a) => asms.extend(a),
            Err(e) => return no(e),
        }
    }
    (class, asms)
}

#[cfg(test)]
mod first_event_tests {
    use super::first_event_of;

    #[test]
    fn index_matches_position() {
        let v = vec![
            None,
            Some((2, false)),
            Some((2, true)),
            None,
            Some((0, true)),
            Some((2, true)),
            Some((0, false)),
            Some((3, false)),
            Some((0, true)),
        ];
        let m = first_event_of(&v);
        for k in 0..6 {
            assert_eq!(
                m.get(&k).copied(),
                v.iter().position(|x| *x == Some((k, true))),
                "k={k}"
            );
        }
    }
}
