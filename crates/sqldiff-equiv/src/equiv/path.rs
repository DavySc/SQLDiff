//! Path summaries for loop-free T-SQL procedures (T-SQL against T-SQL
//! equivalence, PR 2a; `tsql-tsql/pr2-spec.md` v2).
//!
//! The structural diff and the statement lowering compare statements that
//! are paired by their text or their place. A guard rewrite (a nested IF
//! flattened into AND, a guard clause turned into IF/ELSE, a statement moved
//! out of both branches of an IF) leaves unpaired statements although every
//! execution does the same thing. This module proves such a pair equal by
//! comparing what each execution does, never by claiming a difference:
//!
//! - A routine is read as a tree of IFs over **atoms**. An atom is a
//!   comparison of two plain operands (`ord(x, y)` in LT, EQ, GT, UNKNOWN,
//!   shared by every comparison of the same ordered pair) or `x IS NULL`.
//!   Every atom must be total (`Canon::raise_free`), so a guard never raises
//!   and the order guards are evaluated in does not matter.
//! - A **symbolic store** maps each variable to the term it holds: every
//!   local starts as NULL, a parameter as itself, and an assignment
//!   (`SET @v = e`, `DECLARE @v t = e`) puts the term of `e`. An assignment
//!   must be total as well, including the conversion into the variable's
//!   declared type, so it can neither fail nor be missed when it is dead.
//!   Names never appear in a term, only the terms.
//! - An **outcome** is the ordered list of events (writes, result sets,
//!   messages, each the lowering's term under the store) and the exit
//!   (fall-through or `RETURN`, with the final terms of the OUTPUT
//!   parameters). Two routines are equal when for **every** valuation of
//!   the atoms both produce the same outcome. The valuations are explored
//!   jointly and exhaustively, metered at `PathNodes`; past the cap the
//!   routine is not proven.
//! - Atoms are independent in the exploration, which only adds valuations
//!   that cannot happen, so it can lose a proof and never make one up.
//!
//! The proof never says "different": it returns `Ok` and the caller removes
//! structural findings, or a reason (`path:...`) and the findings stay. What
//! the proof does not model is refused by name (`refuse`): loops, cursors,
//! TRY/CATCH, dynamic SQL, calls, transactions, SET options, anything that
//! reads or writes session state, and a statement the lowering could not
//! read. As a backstop until the audit of normalizations of the cross-dialect
//! lowering is complete (spec 8), the string and numeric literals of the
//! conditions, the text of every assignment and the text of every paired
//! event must be token-identical with variable names masked.

use std::collections::BTreeMap;

use sqldiff_syntax::{Dialect, SyntaxKind};

use super::ast::{tokens, Expr, Stmt, Type, K};
use super::canon::{asm, Canon};
use super::lower::{CtlKind, LStmt, Unit, UnitKind};
use super::schema::Schema;
use super::term::{a, n, Term};
use crate::artifact::Analyzed;
use crate::coverage::{Part, Receipt};
use crate::model::{Finding, Verdict};

/// The most tree nodes the joint exploration of one routine pair may visit
/// (`Site::PathNodes`): every run under a partial valuation visits the
/// nodes of the two trees it passes, and the count is the work, not the
/// number of valuations, so the cap bounds the time however large the
/// trees. Past it the routine is not proven (`path:over-cap`).
pub const NODE_CAP: u64 = 1 << 20;

/// An atom of the guards, over terms under the store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum AtomKey {
    /// The ordered pair `(x, y)` with `x < y` as terms, when neither is
    /// NULL: LT, EQ, GT. Whether an operand is NULL is its own atom
    /// (`Null`), so a NULL operand makes the comparison UNKNOWN (or, with
    /// ANSI_NULLS OFF, what that setting says) in the same way everywhere.
    Ord(Term, Term),
    /// `x IS NULL`.
    Null(Term),
}

impl AtomKey {
    /// Number of values the atom can take.
    fn domain(&self) -> u8 {
        match self {
            AtomKey::Ord(..) => 3,
            AtomKey::Null(_) => 2,
        }
    }
}

const LT: u8 = 0;
const EQ: u8 = 1;
const GT: u8 = 2;

/// Three-valued truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    T,
    F,
    U,
}

impl Tri {
    fn not(self) -> Tri {
        match self {
            Tri::T => Tri::F,
            Tri::F => Tri::T,
            Tri::U => Tri::U,
        }
    }
}

/// A condition as a boolean skeleton over comparisons of terms.
#[derive(Debug, Clone)]
enum Cond {
    /// `l op r` with `op` one of `= <> < > <= >=`.
    Cmp(&'static str, Term, Term),
    /// `x IS [NOT] NULL`: the flag is "NOT".
    IsNull(Term, bool),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
}

#[derive(Debug, Clone)]
enum Node {
    /// A write, a result set or a message, with the masked text it was
    /// lowered from.
    Event(Term, String),
    /// A variable (its `sym` id) takes the term.
    Assign(String, Term),
    If(Cond, Vec<Node>, Vec<Node>),
    /// `RETURN e`, or a bare `RETURN`.
    Return(Option<Term>),
}

/// A routine as a tree, with what the backstop compares.
struct Tree {
    nodes: Vec<Node>,
    /// The `sym` ids of the OUTPUT parameters.
    outs: Vec<String>,
    /// The `sym` ids of the locals, with the declared type each starts with.
    locals: Vec<(String, Term)>,
    /// The masked text of every assignment, sorted.
    assigns: Vec<String>,
    /// The literals of every condition, sorted.
    literals: Vec<String>,
}

fn variable(t: &super::ast::Tok) -> bool {
    t.kind == K::Var && !t.text.starts_with("@@")
}

/// The statement's tokens with variable names masked, keywords upper-cased
/// and literals as written (the prefix of `N'..'` is part of a literal).
fn masked(s: &LStmt) -> String {
    let toks = tokens(&s.node);
    toks.get(s.tok_from..)
        .unwrap_or_default()
        .iter()
        .map(|t| {
            if variable(t) {
                "@".to_string()
            } else if t.kind == K::Word {
                t.up.clone()
            } else {
                t.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why `u` is outside what a path summary models, or `None`.
fn refuse(u: &Unit) -> Option<String> {
    let no = |why: &str| Some(why.to_string());
    if u.kind != UnitKind::Proc || u.trigger.is_some() || !u.in_block || u.dialect != Dialect::Tsql
    {
        return no("path:unit-kind");
    }
    if u.local_ids.is_some() {
        return no("path:unit-kind");
    }
    if !u.uncovered.is_empty() || !u.noted.is_empty() || !u.notes.is_empty() {
        return no("path:uncompared-statement");
    }
    if !u.calls.is_empty() {
        return no("path:call");
    }
    if !u.dynamic.is_empty() {
        return no("path:dynamic-sql");
    }
    if !u.cursor_loops.is_empty()
        || !u.cursor_vars.is_empty()
        || u.stmts.iter().any(|s| s.cursor.is_some())
        || u.results.iter().any(|r| r.cursor.is_some())
    {
        return no("path:loop-or-jump");
    }
    if u.catches || u.stops_on_error {
        return no("path:error-handler");
    }
    if u.txn_control
        || !u.txn_stmts.is_empty()
        || !u.tables.is_empty()
        || !u.ddl.is_empty()
        || !u.table_vars.is_empty()
        || !u.temp_tables.is_empty()
        || !u.temp_drops.is_empty()
        || !u.temp_table_drops.is_empty()
        || !u.local_tables.is_empty()
        || !u.linked.is_empty()
    {
        return no("path:session-state");
    }
    for c in &u.ctl {
        match &c.kind {
            CtlKind::If(Ok(_)) | CtlKind::Else | CtlKind::End => {}
            CtlKind::If(Err(_)) => return no("path:uncompared-statement"),
            CtlKind::RowsIf(..) => return no("path:reads-failure-state"),
            CtlKind::Try | CtlKind::Catch | CtlKind::Rethrow => return no("path:error-handler"),
            CtlKind::Call => return no("path:call"),
            CtlKind::Block | CtlKind::Exit(_) => return no("path:goto"),
            CtlKind::While(_) | CtlKind::FetchWhile(_) | CtlKind::Break | CtlKind::Continue => {
                return no("path:loop-or-jump")
            }
        }
    }
    for s in &u.stmts {
        let toks = tokens(&s.node);
        if toks.iter().any(|t| {
            t.up.starts_with("@@")
                || matches!(
                    t.up.as_str(),
                    "XACT_STATE" | "SCOPE_IDENTITY" | "IDENT_CURRENT" | "GOTO"
                )
        }) {
            return no("path:reads-failure-state");
        }
        if assigns_in_place(&toks) {
            return no("path:unmodelled-write");
        }
    }
    None
}

/// Whether the tokens assign a variable other than as `SET @v = e`:
/// `SELECT @v = ...`, `UPDATE t SET @v = c`, `@v += 1`, `a, @v = b`.
fn assigns_in_place(toks: &[super::ast::Tok]) -> bool {
    for (i, t) in toks.iter().enumerate() {
        if !variable(t) {
            continue;
        }
        let Some(next) = toks.get(i + 1) else {
            continue;
        };
        if next.kind == K::Op
            && matches!(
                next.text.as_str(),
                "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^="
            )
        {
            return true;
        }
        if next.kind == K::Op && next.text == "=" && i > 0 {
            let prev = &toks[i - 1];
            if prev.kind == K::Comma || matches!(prev.up.as_str(), "SELECT" | "SET") {
                // `SET @v = e` as a whole statement is an assignment; one
                // after UPDATE's table is not.
                if prev.up == "SET" && i == 1 {
                    continue;
                }
                return true;
            }
        }
    }
    false
}

/// The declared type of a local or parameter named `v` (`@x`).
fn declared<'u>(u: &'u Unit, v: &str) -> Option<&'u Type> {
    let name = v.trim_start_matches('@').to_lowercase();
    if let Some(l) = u.locals.iter().find(|l| l.name == name) {
        return l.decl.as_ref();
    }
    u.params
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.decl.as_ref())
}

fn int_range(ty: &str) -> Option<(i128, i128)> {
    match ty {
        "tinyint" => Some((0, 255)),
        "smallint" => Some((-(1 << 15), (1 << 15) - 1)),
        "int" => Some((-(1 << 31), (1 << 31) - 1)),
        "bigint" => Some((-(1 << 63), (1 << 63) - 1)),
        _ => None,
    }
}

/// Whether storing `e` into the variable `target` cannot raise: NULL, a
/// variable of exactly the same declared type, an integer literal in the
/// range of an integer type, or a string literal that fits a character type
/// of known length. Anything else (arithmetic, CAST, a call, a subquery, a
/// narrowing or unknown type) is refused: the assignment would be a hidden
/// way for the routine to fail.
fn total_assignment(u: &Unit, target: &str, e: &Expr, national: bool) -> bool {
    let Some(dst) = declared(u, target) else {
        return false;
    };
    let dst_name = dst.name.to_lowercase();
    match e {
        Expr::Null => true,
        Expr::Var(w) if !w.starts_with("@@") => declared(u, w) == Some(dst),
        Expr::Num(s) => match (int_range(&dst_name), s.parse::<i128>()) {
            (Some((lo, hi)), Ok(v)) => lo <= v && v <= hi,
            _ => false,
        },
        Expr::Str(s) => {
            let wide = matches!(dst_name.as_str(), "nvarchar" | "nchar");
            if !wide && !matches!(dst_name.as_str(), "varchar" | "char") {
                return false;
            }
            // What a non-ASCII character becomes in a varchar (or in an
            // N-less literal) depends on the database's code page, and a
            // varchar length counts bytes of the collation's encoding: only
            // ASCII is read for those. A national literal into a wide type
            // is counted in UTF-16 code units, which is what the length of
            // nvarchar counts.
            if !s.is_ascii() && (!wide || !national) {
                return false;
            }
            if national && !wide && !s.is_ascii() {
                return false;
            }
            let units = s.encode_utf16().count();
            match dst.args.as_slice() {
                [len] if len.eq_ignore_ascii_case("max") => true,
                [len] => len.parse::<usize>().is_ok_and(|l| units <= l),
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether `RETURN e` yields an int status without raising.
fn total_return(u: &Unit, e: &Expr) -> bool {
    match e {
        Expr::Num(s) => s
            .parse::<i128>()
            .is_ok_and(|v| (-(1 << 31)..(1 << 31)).contains(&v)),
        Expr::Var(w) if !w.starts_with("@@") => declared(u, w).is_some_and(|t| {
            matches!(
                t.name.to_lowercase().as_str(),
                "int" | "smallint" | "tinyint"
            )
        }),
        _ => false,
    }
}

struct Frame {
    cond: Cond,
    then: Vec<Node>,
    els: Vec<Node>,
    in_else: bool,
}

/// Whether control cannot continue past these nodes: the last one returns,
/// or is an IF whose two branches both do.
fn exits(nodes: &[Node]) -> bool {
    match nodes.last() {
        Some(Node::Return(_)) => true,
        Some(Node::If(_, t, e)) => !e.is_empty() && exits(t) && exits(e),
        _ => false,
    }
}

/// Lowers a condition to its skeleton. Every comparison must be total
/// (`Canon::raise_free` on the whole condition) and between plain operands.
fn cond_of(c: &mut Canon, e: &Expr, nat: &[bool]) -> Result<Cond, String> {
    if !c.raise_free(e) {
        return Err("path:raising-guard".into());
    }
    let mut it = Flags {
        nat: nat.iter().copied(),
        used: 0,
    };
    let cond = cond_go(c, e, &mut it)?;
    // A condition the lowering made up (the severity test of a RAISERROR)
    // has no literal of its own: the tokens are the statement's.
    if it.used != 0 && it.used != nat.len() {
        return Err("path:raising-guard".into());
    }
    Ok(cond)
}

/// The string literals of a condition in source order, each with whether it
/// was written `N'..'`: the term of a literal does not keep the prefix, so
/// the operand carries it.
struct Flags<I: Iterator<Item = bool>> {
    nat: I,
    used: usize,
}

fn operand<I: Iterator<Item = bool>>(
    c: &mut Canon,
    x: &Expr,
    f: &mut Flags<I>,
) -> Result<Term, String> {
    match x {
        Expr::Var(v) if !v.starts_with("@@") => {}
        Expr::Num(_) | Expr::Str(_) | Expr::Null => {}
        _ => return Err("path:raising-guard".into()),
    }
    let t = c
        .expr(x)
        .map(|(t, _)| t)
        .map_err(|_| "path:raising-guard".to_string())?;
    if matches!(x, Expr::Str(_)) {
        f.used += 1;
        let Some(national) = f.nat.next() else {
            return Err("path:raising-guard".into());
        };
        return Ok(flag(national, t));
    }
    Ok(t)
}

fn cond_go<I: Iterator<Item = bool>>(
    c: &mut Canon,
    e: &Expr,
    f: &mut Flags<I>,
) -> Result<Cond, String> {
    match e {
        Expr::Bin(o, l, r) if o.eq_ignore_ascii_case("AND") => Ok(Cond::And(
            Box::new(cond_go(c, l, f)?),
            Box::new(cond_go(c, r, f)?),
        )),
        Expr::Bin(o, l, r) if o.eq_ignore_ascii_case("OR") => Ok(Cond::Or(
            Box::new(cond_go(c, l, f)?),
            Box::new(cond_go(c, r, f)?),
        )),
        Expr::Unary(o, x) if o.eq_ignore_ascii_case("NOT") => {
            Ok(Cond::Not(Box::new(cond_go(c, x, f)?)))
        }
        Expr::IsNull(x, neg) => Ok(Cond::IsNull(operand(c, x, f)?, *neg)),
        Expr::Bin(o, l, r) => {
            let op = match o.as_str() {
                "=" => "=",
                "<>" | "!=" => "<>",
                "<" => "<",
                ">" => ">",
                "<=" => "<=",
                ">=" => ">=",
                _ => return Err("path:raising-guard".into()),
            };
            let l = operand(c, l, f)?;
            let r = operand(c, r, f)?;
            Ok(Cond::Cmp(op, l, r))
        }
        _ => Err("path:raising-guard".into()),
    }
}

/// The tree of one routine's unit, or the reason it is not read.
fn tree(u: &Unit, schema: &Schema) -> Result<Tree, String> {
    if let Some(why) = refuse(u) {
        return Err(why);
    }
    let mut c = Canon::new(u, schema);
    c.other = Some(Dialect::Tsql);
    let mut root: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut assigns = Vec::new();
    let mut literals = Vec::new();
    // Every lowering's assumptions: only the ownership chain, which the
    // caller drops for a header without EXECUTE AS, may be recorded.
    let mut assumed: Vec<String> = Vec::new();
    let push = |root: &mut Vec<Node>, stack: &mut Vec<Frame>, node: Node| -> Result<(), String> {
        let cur: &mut Vec<Node> = match stack.last_mut() {
            Some(f) if f.in_else => &mut f.els,
            Some(f) => &mut f.then,
            None => root,
        };
        if exits(cur) {
            return Err("path:dead-code".into());
        }
        cur.push(node);
        Ok(())
    };
    let mut stmts = u.stmts.iter().enumerate().peekable();
    let mut ctl = u.ctl.iter().peekable();
    loop {
        let next = stmts.peek().map_or(usize::MAX, |(i, _)| *i);
        while let Some(m) = ctl.next_if(|m| m.after <= next) {
            match &m.kind {
                CtlKind::If(Ok(e)) => {
                    c.reset();
                    let toks = tokens(&m.node);
                    let cond = cond_of(&mut c, e, &str_flags(&toks))?;
                    assumed.extend(c.asm.drain(..).map(|x| x.id.to_string()));
                    for t in toks {
                        if matches!(t.kind, K::Num | K::Str) {
                            literals.push(t.text.clone());
                        }
                    }
                    stack.push(Frame {
                        cond,
                        then: Vec::new(),
                        els: Vec::new(),
                        in_else: false,
                    });
                }
                CtlKind::Else => match stack.last_mut() {
                    Some(f) if !f.in_else => f.in_else = true,
                    _ => return Err("path:uncompared-statement".into()),
                },
                CtlKind::End => {
                    let Some(f) = stack.pop() else {
                        return Err("path:uncompared-statement".into());
                    };
                    push(&mut root, &mut stack, Node::If(f.cond, f.then, f.els))?;
                }
                _ => return Err("path:uncompared-statement".into()),
            }
        }
        let Some((at, s)) = stmts.next() else { break };
        c.reset();
        let node = match (&s.msg, &s.ast) {
            (Some(m), _) => {
                if m.severity.is_some() {
                    return Err("path:raiserror-form".into());
                }
                let t = c
                    .message(m)
                    .map_err(|_| "path:uncompared-statement".to_string())?;
                Node::Event(t, masked(s))
            }
            (None, Err(_)) => return Err("path:uncompared-statement".into()),
            (None, Ok(st)) => match st {
                Stmt::Assign(target, e) => {
                    let Expr::Var(v) = target else {
                        return Err("path:unmodelled-write".into());
                    };
                    let toks = tokens(&s.node);
                    let flags = str_flags(toks.get(s.tok_from..).unwrap_or_default());
                    if flags.len() > 1 || !total_assignment(u, v, e, flags == [true]) {
                        return Err("path:raising-assignment".into());
                    }
                    let t = c
                        .stmt(st)
                        .map_err(|_| "path:uncompared-statement".to_string())?;
                    if !t.is("assign") || !t.kid(0).is_some_and(|k| k.is("sym")) {
                        return Err("path:uncompared-statement".into());
                    }
                    assigns.push(masked(s));
                    let id = t
                        .kid(0)
                        .and_then(|k| k.kid(0))
                        .and_then(Term::atom)
                        .unwrap_or("");
                    let mut val = t.kids()[1].clone();
                    if matches!(e, Expr::Str(_)) {
                        val = flag(flags == [true], val);
                    }
                    let tag = declared(u, v).map_or_else(|| a("none"), type_tag);
                    Node::Assign(id.to_string(), n("typed", vec![tag, val]))
                }
                Stmt::Return(None) => Node::Return(None),
                Stmt::Return(Some(e)) => {
                    if !total_return(u, e) {
                        return Err("path:uncompared-statement".into());
                    }
                    let t = c
                        .stmt(st)
                        .map_err(|_| "path:uncompared-statement".to_string())?;
                    Node::Return(Some(t))
                }
                Stmt::Declare { .. } | Stmt::Message(_) => {
                    return Err("path:uncompared-statement".into())
                }
                Stmt::Select(q) => {
                    if !q.into.is_empty()
                        || q.items.iter().any(|i| i.assign.is_some())
                        || !u.results.iter().any(|r| r.at == at)
                    {
                        return Err("path:unmodelled-write".into());
                    }
                    let t = c
                        .result_stmt(st, s.output.as_ref())
                        .map_err(|_| "path:uncompared-statement".to_string())?;
                    Node::Event(t, masked(s))
                }
                Stmt::Update { sets, .. }
                    if sets
                        .iter()
                        .any(|(cols, _)| cols.iter().any(|x| x.starts_with('@'))) =>
                {
                    return Err("path:unmodelled-write".into());
                }
                Stmt::Insert { .. }
                | Stmt::Update { .. }
                | Stmt::Delete { .. }
                | Stmt::Merge { .. } => {
                    if s.output.as_ref().is_some_and(|o| o.into.is_some()) {
                        return Err("path:unmodelled-write".into());
                    }
                    let t = match &s.output {
                        Some(o) => c.output_stmt(st, o),
                        None => c.stmt(st),
                    }
                    .map_err(|_| "path:uncompared-statement".to_string())?;
                    Node::Event(t, masked(s))
                }
            },
        };
        assumed.extend(c.asm.drain(..).map(|x| x.id.to_string()));
        push(&mut root, &mut stack, node)?;
    }
    if !stack.is_empty() {
        return Err("path:uncompared-statement".into());
    }
    if assumed.iter().any(|x| x != asm::OWNERSHIP_CHAIN.0) {
        return Err("path:assumption".into());
    }
    assigns.sort();
    literals.sort();
    let sym = |prefix: &str, k: usize| format!("{prefix}{}", k + 1);
    Ok(Tree {
        nodes: root,
        outs: u
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.out)
            .map(|(k, _)| sym("p", k))
            .collect(),
        locals: u
            .locals
            .iter()
            .enumerate()
            .map(|(k, l)| {
                let tag = l.decl.as_ref().map_or_else(|| a("none"), type_tag);
                (sym("l", k), n("typed", vec![tag, a("null")]))
            })
            .collect(),
        assigns,
        literals,
    })
}

/// What one run does.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    events: Vec<(Term, String)>,
    exit: Term,
}

type Store = BTreeMap<String, Term>;
type Valuation = BTreeMap<AtomKey, u8>;

/// The term with every variable replaced by what the store holds.
fn subst(t: &Term, st: &Store) -> Term {
    t.map(&|x| {
        if x.is("sym") {
            x.kid(0)
                .and_then(Term::atom)
                .and_then(|id| st.get(id))
                .cloned()
        } else {
            None
        }
    })
}

/// The term under the wrappers the store puts around a value: the declared
/// type of the variable it went through (`typed`), and whether a string
/// literal was national (`nat`) or not (`narrow`).
fn core(t: &Term) -> &Term {
    let mut t = t;
    while matches!(t.head(), "typed" | "nat" | "narrow") {
        match t.kids().last() {
            Some(k) => t = k,
            None => break,
        }
    }
    t
}

/// The declared type as the store tags a value with: name and arguments,
/// lower case.
fn type_tag(t: &Type) -> Term {
    a(format!(
        "{}({})",
        t.name.to_lowercase(),
        t.args
            .iter()
            .map(|x| x.to_lowercase())
            .collect::<Vec<_>>()
            .join(",")
    ))
}

/// A string value marked national (`N'..'`) or not.
fn flag(national: bool, t: Term) -> Term {
    n(if national { "nat" } else { "narrow" }, vec![t])
}

/// The tokens of a statement or condition that are string literals, each
/// with whether it is national.
fn str_flags(toks: &[super::ast::Tok]) -> Vec<bool> {
    toks.iter()
        .filter(|t| t.kind == K::Str)
        .map(|t| t.text.starts_with(['N', 'n']))
        .collect()
}

fn is_literal(t: &Term) -> bool {
    matches!(core(t).head(), "num" | "str")
}

/// The truth of a condition under the store and a partial valuation, or the
/// first atom it needs that the valuation does not decide.
fn eval(c: &Cond, st: &Store, v: &Valuation, ansi: bool) -> Result<Tri, AtomKey> {
    match c {
        Cond::Not(x) => eval(x, st, v, ansi).map(Tri::not),
        Cond::And(l, r) => match eval(l, st, v, ansi)? {
            Tri::F => Ok(Tri::F),
            lt => match (lt, eval(r, st, v, ansi)?) {
                (_, Tri::F) => Ok(Tri::F),
                (Tri::T, Tri::T) => Ok(Tri::T),
                _ => Ok(Tri::U),
            },
        },
        Cond::Or(l, r) => match eval(l, st, v, ansi)? {
            Tri::T => Ok(Tri::T),
            lt => match (lt, eval(r, st, v, ansi)?) {
                (_, Tri::T) => Ok(Tri::T),
                (Tri::F, Tri::F) => Ok(Tri::F),
                _ => Ok(Tri::U),
            },
        },
        Cond::IsNull(x, neg) => {
            let is_null = nullness(&subst(x, st), v)?;
            Ok(if is_null != *neg { Tri::T } else { Tri::F })
        }
        Cond::Cmp(op, l, r) => {
            let (l, r) = (subst(l, st), subst(r, st));
            let (nl, nr) = (nullness(&l, v)?, nullness(&r, v)?);
            if nl || nr {
                // ANSI_NULLS ON: UNKNOWN. OFF changes only `=` and `<>`.
                return Ok(if ansi {
                    Tri::U
                } else {
                    match *op {
                        "=" => {
                            if nl && nr {
                                Tri::T
                            } else {
                                Tri::F
                            }
                        }
                        "<>" => {
                            if nl && nr {
                                Tri::F
                            } else {
                                Tri::T
                            }
                        }
                        _ => Tri::U,
                    }
                });
            }
            let swapped = l > r;
            let k = if swapped {
                AtomKey::Ord(r, l)
            } else {
                AtomKey::Ord(l, r)
            };
            let mut x = match v.get(&k) {
                Some(&x) => x,
                None => return Err(k),
            };
            if swapped && x != EQ {
                x = if x == LT { GT } else { LT };
            }
            let yes = match *op {
                "=" => x == EQ,
                "<>" => x != EQ,
                "<" => x == LT,
                ">" => x == GT,
                "<=" => x != GT,
                _ => x != LT,
            };
            Ok(if yes { Tri::T } else { Tri::F })
        }
    }
}

/// Whether the term is NULL: known for a literal, otherwise an atom.
fn nullness(t: &Term, v: &Valuation) -> Result<bool, AtomKey> {
    if core(t).atom() == Some("null") {
        return Ok(true);
    }
    if is_literal(t) {
        return Ok(false);
    }
    let k = AtomKey::Null(t.clone());
    match v.get(&k) {
        Some(&x) => Ok(x == 1),
        None => Err(k),
    }
}

/// Runs nodes under the store; `Ok(true)` when the routine returned.
fn run(
    nodes: &[Node],
    st: &mut Store,
    v: &Valuation,
    ansi: bool,
    out: &mut Vec<(Term, String)>,
    exit: &mut Option<Term>,
    visits: &mut u64,
) -> Result<bool, AtomKey> {
    for node in nodes {
        *visits += 1;
        match node {
            Node::Event(t, text) => out.push((subst(t, st), text.clone())),
            Node::Assign(id, t) => {
                let val = subst(t, st);
                st.insert(id.clone(), val);
            }
            Node::Return(t) => {
                *exit = Some(match t {
                    Some(t) => n("ret", vec![subst(t, st)]),
                    None => a("fall"),
                });
                return Ok(true);
            }
            Node::If(cond, then, els) => {
                let branch = if eval(cond, st, v, ansi)? == Tri::T {
                    then
                } else {
                    els
                };
                if run(branch, st, v, ansi, out, exit, visits)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn trace(t: &Tree, v: &Valuation, ansi: bool, visits: &mut u64) -> Result<Outcome, AtomKey> {
    let mut st = Store::new();
    for (l, init) in &t.locals {
        st.insert(l.clone(), init.clone());
    }
    let mut events = Vec::new();
    let mut exit = None;
    run(&t.nodes, &mut st, v, ansi, &mut events, &mut exit, visits)?;
    let mut exit = exit.unwrap_or_else(|| a("fall"));
    // The final value of each OUTPUT parameter belongs to the exit.
    let finals: Vec<Term> = t
        .outs
        .iter()
        .map(|id| {
            st.get(id)
                .cloned()
                .unwrap_or_else(|| n("sym", vec![a(id.clone())]))
        })
        .collect();
    exit = n("exit", std::iter::once(exit).chain(finals).collect());
    Ok(Outcome { events, exit })
}

/// Whether the two trees do the same under every valuation of the atoms.
fn explore(ta: &Tree, tb: &Tree, ansi: bool) -> Result<(), String> {
    let mut nodes = 0u64;
    let mut work: Vec<Valuation> = vec![Valuation::new()];
    while let Some(v) = work.pop() {
        let before = nodes;
        let (ra, rb) = (
            trace(ta, &v, ansi, &mut nodes),
            trace(tb, &v, ansi, &mut nodes),
        );
        sqldiff_model::work::spend(sqldiff_model::work::Site::PathNodes, nodes - before);
        if nodes > NODE_CAP {
            return Err("path:over-cap".into());
        }
        let need = match (ra, rb) {
            (Ok(oa), Ok(ob)) => {
                if oa != ob {
                    return Err("path:unproven".into());
                }
                continue;
            }
            (Err(x), Err(y)) => x.min(y),
            (Err(x), _) | (_, Err(x)) => x,
        };
        // Pushed in reverse so the smallest value is explored first: the
        // order is a function of the trees only.
        for val in (0..need.domain()).rev() {
            let mut w = v.clone();
            w.insert(need.clone(), val);
            work.push(w);
        }
    }
    Ok(())
}

/// Proves the two units equal on every execution, or says why not. `ua` and
/// `ub` are the lowering of one routine of each side.
pub fn prove(ua: &Unit, ub: &Unit, sa: &Schema, sb: &Schema) -> Result<(), String> {
    let (ta, tb) = (tree(ua, sa)?, tree(ub, sb)?);
    if ta.assigns != tb.assigns || ta.literals != tb.literals {
        return Err("path:literal-or-assignment-differs".into());
    }
    // `=` and `<>` read NULL operands as the module's ANSI_NULLS setting
    // says, which the pair does not show: it must hold under both.
    explore(&ta, &tb, true)?;
    explore(&ta, &tb, false)
}

/// Byte ranges of the routines of `a`, in order.
pub fn routine_ranges(a: &Analyzed) -> Vec<(usize, usize)> {
    a.root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .map(|n| {
            let r = n.text_range();
            (usize::from(r.start()), usize::from(r.end()))
        })
        .collect()
}

/// Proves routine `i` of `a` equal to routine `i` of `b` (the routines pair
/// by position when the headers are token-identical): every other routine is
/// blanked on both sides and the one left is lowered, so its unit holds only
/// its own statements.
pub fn prove_routine(
    a: &Analyzed,
    b: &Analyzed,
    i: usize,
    sa: &Schema,
    sb: &Schema,
) -> Result<(), String> {
    let (ra, rb) = (routine_ranges(a), routine_ranges(b));
    if ra.len() != rb.len() || i >= ra.len() {
        return Err("path:unit-kind".into());
    }
    let others = |r: &[(usize, usize)]| -> Vec<(usize, usize)> {
        r.iter()
            .enumerate()
            .filter(|(k, _)| *k != i)
            .map(|(_, &x)| x)
            .collect()
    };
    let (xa, xb) = (
        super::blanked(a.artifact, &others(&ra)),
        super::blanked(b.artifact, &others(&rb)),
    );
    let (la, lb) = (
        Analyzed::new(&xa, a.side, a.excerpt_lines),
        Analyzed::new(&xb, b.side, b.excerpt_lines),
    );
    let (ua, ub) = (
        super::lower::lower_against(&la, b.dialect()),
        super::lower::lower_against(&lb, a.dialect()),
    );
    prove(&ua, &ub, sa, sb)
}

/// Removes the structural findings of every routine pair the path summary
/// proves equal, with a receipt each, and returns the receipts. Only
/// routines (paired by position, headers and declarations token-identical,
/// no `EXECUTE AS`) are read; a finding outside every routine, or that names
/// two different routines, stays. A routine that holds a witnessed
/// difference of another analysis, and every routine when such a difference
/// has no place in one, keeps its findings (merge rule F6: this analysis
/// never overrides a witness).
pub fn clear(
    a: &Analyzed,
    b: &Analyzed,
    sa: &Schema,
    sb: &Schema,
    findings: &mut Vec<Finding>,
) -> Vec<Receipt> {
    use super::second::{declarations, has_execute_as, removable, same_headers};
    if !same_headers(a, b) || declarations(a) != declarations(b) {
        return Vec::new();
    }
    if has_execute_as(a) || has_execute_as(b) {
        return Vec::new();
    }
    let (ra, rb) = (routine_ranges(a), routine_ranges(b));
    if ra.is_empty() || ra.len() != rb.len() {
        return Vec::new();
    }
    let at = |rs: &[(usize, usize)], l: &crate::model::Location| {
        rs.iter()
            .position(|&(s, e)| l.byte_start >= s && l.byte_start < e)
    };
    // The routine pair a finding names: None when it names none, or two.
    let routine_of = |f: &Finding| -> Option<usize> {
        let ia = f.a.as_ref().map(|l| at(&ra, l));
        let ib = f.b.as_ref().map(|l| at(&rb, l));
        match (ia, ib) {
            (Some(None), _) | (_, Some(None)) | (None, None) => None,
            (Some(Some(i)), Some(Some(j))) if i != j => None,
            (Some(Some(i)), _) | (_, Some(Some(i))) => Some(i),
        }
    };
    let mut blocked = vec![false; ra.len()];
    for f in findings
        .iter()
        .filter(|f| !removable(f) && f.verdict == Verdict::Different)
    {
        match routine_of(f) {
            Some(i) => blocked[i] = true,
            None => return Vec::new(),
        }
    }
    let mut proved: Vec<Option<bool>> = vec![None; ra.len()];
    let mut receipts = Vec::new();
    let mut kept = Vec::with_capacity(findings.len());
    for f in std::mem::take(findings) {
        let routine = removable(&f).then(|| routine_of(&f)).flatten();
        let clear = match routine {
            Some(i) if !blocked[i] => {
                *proved[i].get_or_insert_with(|| prove_routine(a, b, i, sa, sb).is_ok())
            }
            _ => false,
        };
        if clear {
            receipts.push(Receipt::Judged {
                rule: "path summary proved the routine equal on every execution (same dialect)",
                a: f.a.as_ref().map(Part::at),
                b: f.b.as_ref().map(Part::at),
            });
        } else {
            kept.push(f);
        }
    }
    *findings = kept;
    receipts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Artifact;
    use crate::model::Side;

    fn proc_(body: &str) -> String {
        format!("CREATE PROCEDURE dbo.P @a int, @b int, @o int OUTPUT AS BEGIN {body} END")
    }

    fn prove_sql(a: &str, b: &str) -> Result<(), String> {
        let (xa, xb) = (
            Artifact::new("a.sql", Dialect::Tsql, proc_(a)),
            Artifact::new("b.sql", Dialect::Tsql, proc_(b)),
        );
        let (la, lb) = (
            Analyzed::new(&xa, Side::A, 0),
            Analyzed::new(&xb, Side::B, 0),
        );
        prove_routine(&la, &lb, 0, &Schema::default(), &Schema::default())
    }

    const O: &str = "INSERT INTO dbo.Orders (Qty) VALUES (1);";
    const S: &str = "INSERT INTO dbo.Stock (ItemId, Qty) VALUES (1, 2);";

    #[test]
    fn rewrites_with_the_same_outcome_on_every_valuation_are_proved() {
        let pairs = [
            (
                format!("IF @a > 0 BEGIN IF @b > 0 {O} END"),
                format!("IF @a > 0 AND @b > 0 {O}"),
            ),
            (
                format!("IF @a IS NULL OR @a <= 0 BEGIN RAISERROR('x', 16, 1); RETURN; END {O}"),
                format!("IF @a > 0 {O} ELSE RAISERROR('x', 16, 1);"),
            ),
            (
                format!("IF @a > 0 BEGIN {O} RETURN; END {S}"),
                format!("IF @a > 0 {O} ELSE {S}"),
            ),
            (
                format!("IF @a > 0 {O} ELSE IF @a < 0 {S}"),
                format!("IF @a < 0 {S} ELSE IF @a > 0 {O}"),
            ),
            (
                format!("DECLARE @x int = @a; IF @x > 0 BEGIN IF @b > 0 {O} END"),
                format!("DECLARE @x int = @a; IF @x > 0 AND @b > 0 {O}"),
            ),
        ];
        for (a, b) in pairs {
            assert_eq!(prove_sql(&a, &b), Ok(()), "{a} / {b}");
            assert_eq!(prove_sql(&b, &a), Ok(()), "{b} / {a}");
        }
    }

    #[test]
    fn a_different_outcome_on_one_valuation_is_not_proved() {
        let pairs = [
            (format!("IF @a > 0 {O}"), format!("IF @a >= 0 {O}")),
            (format!("IF @a > 0 {O}"), format!("IF @a > 0 {S}")),
            (format!("IF @a > 0 {O}"), format!("IF @a > 1 {O}")),
            (format!("{O} {S}"), format!("{S} {O}")),
            (format!("IF @a > 0 {O}"), O.to_string()),
            (
                format!("IF @a > 0 BEGIN {O} RETURN; END {S}"),
                format!("IF @a > 0 {O} {S}"),
            ),
            (
                "RAISERROR('x', 16, 1);".to_string(),
                "RAISERROR('x', 15, 1);".to_string(),
            ),
            (format!("IF @a = @b {O}"), format!("IF @a <> @b {O}")),
            // NULL: `IS NULL` against a comparison that is UNKNOWN for NULL.
            (
                format!("IF @a IS NULL {O} ELSE {S}"),
                format!("IF @a > 0 {S} ELSE {O}"),
            ),
            ("SET @o = @a;".to_string(), "SET @o = @b;".to_string()),
            (format!("{O} RETURN 1;"), format!("{O} RETURN 2;")),
            (O.to_string(), format!("{O} RETURN 0;")),
        ];
        for (a, b) in pairs {
            assert!(prove_sql(&a, &b).is_err(), "{a} / {b}");
            assert!(prove_sql(&b, &a).is_err(), "{b} / {a}");
        }
    }

    #[test]
    fn a_dropped_output_assignment_is_not_proved() {
        let r = prove_sql(&format!("SET @o = 1; {O}"), O);
        assert!(r.is_err());
        let r = prove_sql(&format!("SET @o = @a; {O}"), &format!("SET @o = @a; {O}"));
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn what_the_proof_does_not_model_is_refused_by_name() {
        let base = format!("IF @a > 0 BEGIN IF @b > 0 {O} END");
        let flat = format!("IF @a > 0 AND @b > 0 {O}");
        let rows = [
            (
                "WHILE @a > 0 BEGIN SET @a = @a - 1; END",
                "path:loop-or-jump",
            ),
            ("SET NOCOUNT ON;", "path:uncompared-statement"),
            (
                "BEGIN TRY SELECT 1; END TRY BEGIN CATCH SELECT 2; END CATCH",
                "path:",
            ),
            ("EXEC dbo.Q;", "path:call"),
            ("EXEC ('SELECT 1');", "path:dynamic-sql"),
            ("DECLARE @t TABLE (x int); ", "path:session-state"),
            ("CREATE TABLE #t (x int);", "path:"),
            ("IF @@ROWCOUNT = 0 SELECT 1;", "path:"),
            (
                "DECLARE @v int; SELECT @v = Qty FROM dbo.Stock;",
                "path:unmodelled-write",
            ),
            ("DECLARE @v int = @a / @b;", "path:raising-assignment"),
            ("DECLARE @v tinyint = @a;", "path:raising-assignment"),
            (
                "DECLARE @v int = @a; SET @v = @a + 1;",
                "path:raising-assignment",
            ),
            ("IF @a / @b > 1 SELECT 1;", "path:raising-guard"),
            ("IF @a LIKE '1%' SELECT 1;", "path:raising-guard"),
            ("IF @a IN (1, 2) SELECT 1;", "path:raising-guard"),
            (
                "IF EXISTS (SELECT 1 FROM dbo.Stock) SELECT 1;",
                "path:raising-guard",
            ),
            ("IF @a > GETDATE() SELECT 1;", "path:raising-guard"),
            ("RAISERROR('x', @a, 1);", "path:raiserror-form"),
            ("RETURN; SELECT 1;", "path:dead-code"),
            ("THROW 50000, 'x', 1;", "path:uncompared-statement"),
            ("UPDATE dbo.Stock SET @b = Qty;", "path:"),
        ];
        for (extra, reason) in rows {
            let (a, b) = (format!("{extra} {base}"), format!("{extra} {flat}"));
            let r = prove_sql(&a, &b);
            let why = r.expect_err(extra);
            assert!(why.starts_with(reason), "{extra}: {why}");
        }
    }

    /// A literal assigned to a variable is read only where its value does
    /// not depend on the code page, the prefix or the declared length.
    #[test]
    fn string_assignments_are_read_only_when_their_value_is_certain() {
        let ins = "INSERT INTO dbo.Orders (Note) VALUES (@x);";
        let flat =
            |decl: &str, lit: &str| format!("{decl} SET @x = {lit}; IF @a > 0 AND @b > 0 {ins}");
        let nested = |decl: &str, lit: &str| {
            format!("{decl} SET @x = {lit}; IF @a > 0 BEGIN IF @b > 0 {ins} END")
        };
        // ASCII into a long enough variable: proved.
        let ok = |d: &str, l: &str| prove_sql(&nested(d, l), &flat(d, l));
        assert_eq!(ok("DECLARE @x varchar(5);", "'abc'"), Ok(()));
        assert_eq!(ok("DECLARE @x nvarchar(3);", "N'abc'"), Ok(()));
        assert_eq!(ok("DECLARE @x nvarchar(2);", "N'😀'"), Ok(()));
        // Code page or length dependent: refused.
        for (d, l) in [
            ("DECLARE @x varchar(5);", "'ж'"),
            ("DECLARE @x varchar(5);", "N'ж'"),
            ("DECLARE @x nvarchar(5);", "'ж'"),
            ("DECLARE @x nvarchar(1);", "N'😀'"),
            ("DECLARE @x varchar(2);", "'abc'"),
        ] {
            let why = ok(d, l).expect_err(l);
            assert_eq!(why, "path:raising-assignment", "{d} {l}");
        }
    }

    #[test]
    fn thirty_independent_ifs_are_over_the_cap_twice_alike() {
        let ifs = |rev: bool| -> String {
            let mut v: Vec<String> = (0..30)
                .map(|i| format!("IF @a > {i} INSERT INTO dbo.Orders (Qty) VALUES ({i});"))
                .collect();
            if rev {
                v.reverse();
            }
            v.join(" ")
        };
        let (a, b) = (ifs(false), ifs(false));
        let first = prove_sql(&a, &b);
        let second = prove_sql(&a, &b);
        assert_eq!(first, second);
        // The same routine: not reported as an over-cap (equal trees explore
        // the whole space, so the cap is reached).
        assert_eq!(first, Err("path:over-cap".to_string()));
    }
}
