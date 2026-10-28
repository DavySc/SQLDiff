//! Routines as abstract syntax: header, parameters and a body of
//! statements, blocks, `IF`, `WHILE` and calls (docs/verified-conversion.md,
//! C0 item 1 and phase C1). Read from the CST of T-SQL and PL/pgSQL
//! routines; printed as PL/pgSQL by `print_postgres`.
//!
//! Anything outside this shape is an error, not an approximation: a
//! routine that does not read is one whose conversion cannot be confirmed.

use crate::abstract_syntax::{parse_stmt_ext, tokens, Expr, Parser, Stmt, Tok, Type, K};
use crate::{ast as cst, print, Dialect, SyntaxKind, SyntaxNode};
use SyntaxKind::*;

pub type RResult<T> = Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Procedure,
    Function,
}

/// How a parameter passes its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    In,
    /// PostgreSQL `OUT`. T-SQL has none: its `OUTPUT` parameters are `InOut`.
    Out,
    InOut,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// As written; T-SQL names keep their `@`.
    pub name: String,
    pub ty: Type,
    pub mode: Mode,
    pub default: Option<Expr>,
}

/// Whose privileges the body runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Caller,
    Owner,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Routine {
    pub kind: RoutineKind,
    pub name: Vec<String>,
    pub params: Vec<Param>,
    pub returns: Option<Type>,
    pub security: Security,
    /// PostgreSQL `STABLE` / `IMMUTABLE` (`VOLATILE` is the default, `None`).
    pub volatility: Option<String>,
    pub body: Block,
}

/// A block: PL/pgSQL `[DECLARE ...] BEGIN ... END`, T-SQL `BEGIN ... END`.
/// T-SQL declarations are statements of the body (`Stmt::Declare`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block {
    pub decls: Vec<Stmt>,
    pub body: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    /// `@p = value` (T-SQL), `p => value` (PostgreSQL).
    pub name: Option<String>,
    pub value: Expr,
    /// T-SQL `OUTPUT`: the argument receives the parameter's final value.
    pub output: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Stmt(Stmt),
    Block(Block),
    /// `IF c THEN ... ELSE ...`; `ELSE IF` and `ELSIF` are an `If` alone
    /// in `els`.
    If {
        cond: Expr,
        then: Vec<Node>,
        els: Vec<Node>,
    },
    While {
        cond: Expr,
        body: Vec<Node>,
    },
    /// `EXEC [@rc =] proc args` (T-SQL), `CALL proc(args)` (PostgreSQL).
    Call {
        name: Vec<String>,
        args: Vec<Arg>,
        /// T-SQL `EXEC @rc = proc`: the variable that receives the return code.
        ret: Option<Expr>,
    },
    /// A T-SQL session option: `SET NOCOUNT ON`, `SET XACT_ABORT OFF`.
    /// Upper case.
    Setting {
        name: String,
        on: bool,
    },
}

// ---- reading -------------------------------------------------------------

/// The routine a `ROUTINE` node holds.
pub fn read(n: &SyntaxNode, d: Dialect) -> RResult<Routine> {
    if n.kind() != ROUTINE {
        return Err(format!("{:?} is not a routine", n.kind()));
    }
    // Header tokens: everything outside the body.
    let toks: Vec<Tok> = cst::sig_tokens(n)
        .filter(|t| !t.parent_ancestors().any(|p| p.kind() == ROUTINE_BODY))
        .map(|t| Tok::from_syntax(&t))
        .filter(|t| t.kind != K::Semi)
        .collect();
    let body = n
        .children()
        .find(|c| c.kind() == ROUTINE_BODY)
        .ok_or("a routine without a body")?;
    let mut r = header(&toks, d)?;
    r.body = match d {
        Dialect::Postgres => {
            let units: Vec<SyntaxNode> = body.children().filter(|c| c.kind().is_unit()).collect();
            match units.as_slice() {
                [b] if b.kind() == BLOCK => block(b, d)?,
                _ => return Err("a PL/pgSQL body that is not one block".into()),
            }
        }
        Dialect::Tsql => {
            let units: Vec<SyntaxNode> = body.children().filter(|c| c.kind().is_unit()).collect();
            match units.as_slice() {
                // `AS BEGIN ... END` and `AS stmt ...` run the same statements.
                [b] if b.kind() == BLOCK => block(b, d)?,
                _ => Block {
                    decls: Vec::new(),
                    body: nodes(&units, d)?,
                },
            }
        }
        Dialect::Db2i => return Err("Db2 for i routines are not read yet".into()),
    };
    Ok(r)
}

fn header(t: &[Tok], d: Dialect) -> RResult<Routine> {
    let mut p = Parser::ext(t, d);
    p.expect_kw("CREATE")?;
    if p.eat_kw("OR") && !(p.eat_kw("REPLACE") || p.eat_kw("ALTER")) {
        return Err(p.err("expected REPLACE or ALTER"));
    }
    let kind = if p.eat_kw("PROCEDURE") || (d == Dialect::Tsql && p.eat_kw("PROC")) {
        RoutineKind::Procedure
    } else if p.eat_kw("FUNCTION") {
        RoutineKind::Function
    } else {
        return Err(p.err("expected PROCEDURE or FUNCTION"));
    };
    let name = p.qname()?;
    let mut params = Vec::new();
    let paren = p.eat_kind(K::LParen);
    if !(paren && p.eat_kind(K::RParen)) && !(d == Dialect::Tsql && !paren && at_body(&p)) {
        loop {
            params.push(param(&mut p, d)?);
            if !p.eat_kind(K::Comma) {
                break;
            }
        }
        if paren {
            p.expect_kind(K::RParen)?;
        }
    }
    let mut returns = None;
    let mut security = Security::Caller;
    let mut language = None;
    let mut volatility = None;
    loop {
        if p.eat_kw("RETURNS") {
            if p.is_kw("TABLE") || p.is_kw("SETOF") || p.is_kind(K::Var) {
                return Err("table-valued results are not read yet".into());
            }
            returns = Some(p.ty()?);
        } else if d == Dialect::Postgres
            && (p.is_kw("STABLE") || p.is_kw("IMMUTABLE") || p.is_kw("VOLATILE"))
        {
            let v = p.peek().unwrap().up.clone();
            p.i += 1;
            volatility = (v != "VOLATILE").then_some(v);
        } else if d == Dialect::Postgres && p.eat_kw("LANGUAGE") {
            language = Some(p.any_ident()?.to_lowercase());
        } else if d == Dialect::Postgres && p.eat_kw("SECURITY") {
            security = if p.eat_kw("DEFINER") {
                Security::Owner
            } else {
                p.expect_kw("INVOKER")?;
                Security::Caller
            };
        } else if d == Dialect::Tsql && p.eat_kw("WITH") {
            loop {
                if p.eat_kws(&["EXECUTE", "AS"]) || p.eat_kws(&["EXEC", "AS"]) {
                    security = if p.eat_kw("OWNER") {
                        Security::Owner
                    } else {
                        p.expect_kw("CALLER")?;
                        Security::Caller
                    };
                } else {
                    return Err(p.err("unsupported routine option"));
                }
                if !p.eat_kind(K::Comma) {
                    break;
                }
            }
        } else if p.eat_kw("AS") {
            // The body follows (T-SQL), or the dollar-quoted body was left
            // out of the header tokens (PostgreSQL).
        } else if p.at_end() {
            break;
        } else {
            return Err(p.err("unsupported routine option"));
        }
    }
    if d == Dialect::Postgres && language.as_deref() != Some("plpgsql") {
        return Err("only LANGUAGE plpgsql routines are read".into());
    }
    if (kind == RoutineKind::Function) != returns.is_some() {
        return Err("a function needs RETURNS and a procedure has none".into());
    }
    Ok(Routine {
        kind,
        name,
        params,
        returns,
        security,
        volatility,
        body: Block::default(),
    })
}

/// T-SQL: the header ends without parameters (`CREATE PROCEDURE p AS`).
fn at_body(p: &Parser) -> bool {
    p.is_kw("AS") || p.is_kw("WITH") || p.at_end()
}

fn param(p: &mut Parser, d: Dialect) -> RResult<Param> {
    let mut mode = Mode::In;
    if d == Dialect::Postgres {
        if p.eat_kw("INOUT") {
            mode = Mode::InOut;
        } else if p.eat_kw("OUT") {
            mode = Mode::Out;
        } else {
            p.eat_kw("IN");
        }
    }
    let name = match p.peek() {
        Some(t) if d == Dialect::Tsql && t.kind == K::Var => {
            let v = t.text.clone();
            p.i += 1;
            v
        }
        _ if d == Dialect::Tsql => return Err(p.err("expected a parameter")),
        _ => p.ident()?,
    };
    if d == Dialect::Tsql {
        p.eat_kw("AS");
    }
    let ty = p.ty()?;
    let default = if p.eat_kw("DEFAULT") || p.eat_op("=") {
        Some(p.expr()?)
    } else {
        None
    };
    if d == Dialect::Tsql {
        if p.eat_kw("OUTPUT") || p.eat_kw("OUT") {
            mode = Mode::InOut;
        }
        if p.is_kw("READONLY") || p.is_kw("VARYING") {
            return Err(p.err("unsupported parameter option"));
        }
    }
    Ok(Param {
        name,
        ty,
        mode,
        default,
    })
}

fn block(b: &SyntaxNode, d: Dialect) -> RResult<Block> {
    let mut out = Block::default();
    let mut units = Vec::new();
    for c in b.children() {
        match c.kind() {
            DECLARE_SECTION => {
                for s in c.children().filter(|s| s.kind().is_unit()) {
                    out.decls.push(declaration(&s, d)?);
                }
            }
            EXCEPTION_SECTION => return Err("exception handlers are not read yet".into()),
            k if k.is_unit() => units.push(c),
            _ => {}
        }
    }
    // A label or anything else in the block's own tokens changes its meaning.
    let own: Vec<String> = cst::header_tokens(b)
        .iter()
        .filter(|t| t.kind() != SEMICOLON)
        .map(|t| t.text().to_ascii_uppercase())
        .collect();
    let plain = matches!(
        own.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["BEGIN", "END"] | ["DECLARE", "BEGIN", "END"]
    );
    if !plain {
        return Err(format!("a block written `{}`", own.join(" ")));
    }
    out.body = nodes(&units, d)?;
    Ok(out)
}

/// A PL/pgSQL declaration (`x integer := 1`), read as `DECLARE x ...`.
fn declaration(s: &SyntaxNode, d: Dialect) -> RResult<Stmt> {
    let mut t = tokens(s);
    let first = t.first().ok_or("an empty declaration")?.off;
    t.insert(
        0,
        Tok {
            kind: K::Word,
            text: "DECLARE".into(),
            up: "DECLARE".into(),
            off: first,
        },
    );
    if t.iter().any(|x| x.up == "CONSTANT" || x.up == "NOT") {
        return Err("CONSTANT and NOT NULL declarations are not read yet".into());
    }
    match parse_stmt_ext(&t, d)? {
        s @ Stmt::Declare { .. } => Ok(s),
        _ => Err("not a declaration".into()),
    }
}

fn nodes(units: &[SyntaxNode], d: Dialect) -> RResult<Vec<Node>> {
    units.iter().map(|u| node(u, d)).collect()
}

/// The units of a `BRANCH`.
fn branch(b: &SyntaxNode, d: Dialect) -> RResult<Vec<Node>> {
    let units: Vec<SyntaxNode> = b.children().filter(|c| c.kind().is_unit()).collect();
    nodes(&units, d)
}

fn node(u: &SyntaxNode, d: Dialect) -> RResult<Node> {
    match u.kind() {
        STATEMENT => statement(u, d),
        BLOCK => Ok(Node::Block(block(u, d)?)),
        IF_STMT => if_stmt(u, d),
        LOOP_STMT => {
            let first = cst::first_words(u, 1);
            if first.first().map(String::as_str) != Some("WHILE") {
                return Err("only WHILE loops are read".into());
            }
            let kids: Vec<SyntaxNode> = u.children().collect();
            let (Some(c), Some(b)) = (
                kids.iter().find(|k| k.kind() == CONDITION),
                kids.iter().find(|k| k.kind() == BRANCH),
            ) else {
                return Err("a WHILE without a condition or body".into());
            };
            Ok(Node::While {
                cond: condition(c, d)?,
                body: branch(b, d)?,
            })
        }
        k => Err(format!("{k:?} is not read yet")),
    }
}

fn condition(c: &SyntaxNode, d: Dialect) -> RResult<Expr> {
    let t = tokens(c);
    let mut p = Parser::ext(&t, d);
    let e = p.expr()?;
    if !p.at_end() {
        return Err(p.err("unexpected tokens after the condition"));
    }
    Ok(e)
}

fn if_stmt(u: &SyntaxNode, d: Dialect) -> RResult<Node> {
    // (CONDITION BRANCH)+ [BRANCH]: ELSIF pairs, then ELSE.
    let kids: Vec<SyntaxNode> = u
        .children()
        .filter(|k| matches!(k.kind(), CONDITION | BRANCH))
        .collect();
    let mut arms = Vec::new();
    let mut els = Vec::new();
    let mut i = 0;
    while i < kids.len() {
        match (kids[i].kind(), kids.get(i + 1).map(|k| k.kind())) {
            (CONDITION, Some(BRANCH)) => {
                arms.push((condition(&kids[i], d)?, branch(&kids[i + 1], d)?));
                i += 2;
            }
            (BRANCH, None) if !arms.is_empty() => {
                els = branch(&kids[i], d)?;
                i += 1;
            }
            _ => return Err("an IF of an unexpected shape".into()),
        }
    }
    let mut out: Option<Node> = None;
    for (cond, then) in arms.into_iter().rev() {
        let e = match out.take() {
            Some(n) => vec![n],
            None => std::mem::take(&mut els),
        };
        out = Some(Node::If { cond, then, els: e });
    }
    out.ok_or_else(|| "an IF without a condition".into())
}

fn statement(u: &SyntaxNode, d: Dialect) -> RResult<Node> {
    let t = tokens(u);
    let first = t.first().map(|x| x.up.as_str()).unwrap_or("");
    match (d, first) {
        (Dialect::Tsql, "EXEC" | "EXECUTE") => exec(&t, d),
        (Dialect::Tsql, "SET")
            if t.len() == 3
                && t[1].kind == K::Word
                && matches!(t[2].up.as_str(), "ON" | "OFF")
                && matches!(
                    t[1].up.as_str(),
                    "NOCOUNT"
                        | "XACT_ABORT"
                        | "ANSI_NULLS"
                        | "QUOTED_IDENTIFIER"
                        | "ARITHABORT"
                        | "ANSI_WARNINGS"
                        | "CONCAT_NULL_YIELDS_NULL"
                ) =>
        {
            Ok(Node::Setting {
                name: t[1].up.clone(),
                on: t[2].up == "ON",
            })
        }
        (Dialect::Postgres, "CALL") => call(&t, d),
        _ => Ok(Node::Stmt(parse_stmt_ext(&t, d)?)),
    }
}

/// T-SQL `EXEC [@rc =] proc [[@p =] value [OUTPUT], ...]`.
fn exec(t: &[Tok], d: Dialect) -> RResult<Node> {
    let mut p = Parser::ext(t, d);
    p.i += 1;
    let ret = if p.is_kind(K::Var) && p.peek_at(1).is_some_and(|x| x.text == "=") {
        let v = p.peek().unwrap().text.clone();
        p.i += 2;
        Some(Expr::Var(v))
    } else {
        None
    };
    if p.is_kind(K::LParen) || p.is_kind(K::Str) {
        return Err("dynamic SQL is not read".into());
    }
    let name = p.qname()?;
    let mut args = Vec::new();
    if !p.at_end() {
        loop {
            let named = if p.is_kind(K::Var) && p.peek_at(1).is_some_and(|x| x.text == "=") {
                let v = p.peek().unwrap().text.clone();
                p.i += 2;
                Some(v)
            } else {
                None
            };
            let value = p.expr()?;
            let output = p.eat_kw("OUTPUT") || p.eat_kw("OUT");
            args.push(Arg {
                name: named,
                value,
                output,
            });
            if !p.eat_kind(K::Comma) {
                break;
            }
        }
    }
    if !p.at_end() {
        return Err(p.err("unexpected tokens in EXEC"));
    }
    Ok(Node::Call { name, args, ret })
}

/// PostgreSQL `CALL proc([name =>] value, ...)`.
fn call(t: &[Tok], d: Dialect) -> RResult<Node> {
    let mut p = Parser::ext(t, d);
    p.i += 1;
    let name = p.qname()?;
    p.expect_kind(K::LParen)?;
    let mut args = Vec::new();
    if !p.eat_kind(K::RParen) {
        loop {
            let named = if p.peek_at(1).is_some_and(|x| x.text == "=>") {
                let n = p.ident()?;
                p.i += 1;
                Some(n)
            } else {
                None
            };
            args.push(Arg {
                name: named,
                value: p.expr()?,
                output: false,
            });
            if !p.eat_kind(K::Comma) {
                break;
            }
        }
        p.expect_kind(K::RParen)?;
    }
    if !p.at_end() {
        return Err(p.err("unexpected tokens in CALL"));
    }
    Ok(Node::Call {
        name,
        args,
        ret: None,
    })
}

// ---- printing --------------------------------------------------------------

const D: Dialect = Dialect::Postgres;

/// The routine as a PL/pgSQL `CREATE` statement, with its terminator.
pub fn print_postgres(r: &Routine) -> RResult<String> {
    let mut head = format!(
        "CREATE {} {}(",
        match r.kind {
            RoutineKind::Procedure => "PROCEDURE",
            RoutineKind::Function => "FUNCTION",
        },
        name(&r.name)?
    );
    let mut ps = Vec::new();
    for p in &r.params {
        let mut s = String::new();
        match p.mode {
            Mode::In => {}
            Mode::Out => s.push_str("OUT "),
            Mode::InOut => s.push_str("INOUT "),
        }
        s.push_str(&ident(&p.name)?);
        s.push(' ');
        s.push_str(&ty(&p.ty)?);
        if let Some(e) = &p.default {
            s.push_str(&format!(" DEFAULT {}", print::expr(e, D)?));
        }
        ps.push(s);
    }
    head.push_str(&ps.join(", "));
    head.push(')');
    if let Some(t) = &r.returns {
        head.push_str(&format!(" RETURNS {}", ty(t)?));
    }
    head.push_str("\nLANGUAGE plpgsql\n");
    if let Some(v) = &r.volatility {
        if v != "STABLE" && v != "IMMUTABLE" {
            return Err(format!("the volatility `{v}`"));
        }
        head.push_str(&format!("{v}\n"));
    }
    if r.security == Security::Owner {
        head.push_str("SECURITY DEFINER\n");
    }
    let mut body = String::new();
    print_block(&r.body, 0, &mut body)?;
    let tag = if body.contains("$$") { "$body$" } else { "$$" };
    if body.contains(tag) {
        return Err("the body contains its own quote".into());
    }
    Ok(format!("{head}AS {tag}\n{body}{tag};\n"))
}

fn name(parts: &[String]) -> RResult<String> {
    print::expr(&Expr::Name(parts.to_vec()), D)
}

fn ident(s: &str) -> RResult<String> {
    name(&[s.to_string()])
}

fn ty(t: &Type) -> RResult<String> {
    // A cast prints the type the way the parser reads it back.
    let s = print::expr(&Expr::Cast(Box::new(Expr::Null), t.clone()), D)?;
    Ok(s["CAST(NULL AS ".len()..s.len() - 1].to_string())
}

fn line(depth: usize, s: &str, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(s);
    out.push('\n');
}

fn print_block(b: &Block, depth: usize, out: &mut String) -> RResult<()> {
    if !b.decls.is_empty() {
        line(depth, "DECLARE", out);
        for s in &b.decls {
            let t = print::stmt(s, D)?;
            let t = t.strip_prefix("DECLARE ").ok_or("not a declaration")?;
            line(depth + 1, &format!("{t};"), out);
        }
    }
    line(depth, "BEGIN", out);
    print_nodes(&b.body, depth + 1, out)?;
    line(depth, "END;", out);
    Ok(())
}

fn print_nodes(v: &[Node], depth: usize, out: &mut String) -> RResult<()> {
    if v.is_empty() {
        // PL/pgSQL needs a statement where T-SQL may have none.
        return Err("an empty statement list".into());
    }
    for n in v {
        print_node(n, depth, out)?;
    }
    Ok(())
}

fn print_node(n: &Node, depth: usize, out: &mut String) -> RResult<()> {
    match n {
        Node::Stmt(Stmt::Declare { .. }) => {
            return Err("a declaration inside a PL/pgSQL body".into());
        }
        Node::Stmt(s) => line(depth, &format!("{};", print::stmt(s, D)?), out),
        Node::Block(b) => print_block(b, depth, out)?,
        Node::Setting { name, .. } => {
            return Err(format!(
                "the T-SQL session option {name} has no PL/pgSQL statement"
            ));
        }
        Node::If { cond, then, els } => {
            line(depth, &format!("IF {} THEN", print::expr(cond, D)?), out);
            print_nodes(then, depth + 1, out)?;
            let mut els = els;
            // `ELSIF` for an `If` alone in the else branch.
            while let [Node::If { cond, then, els: e }] = els.as_slice() {
                line(depth, &format!("ELSIF {} THEN", print::expr(cond, D)?), out);
                print_nodes(then, depth + 1, out)?;
                els = e;
            }
            if !els.is_empty() {
                line(depth, "ELSE", out);
                print_nodes(els, depth + 1, out)?;
            }
            line(depth, "END IF;", out);
        }
        Node::While { cond, body } => {
            line(depth, &format!("WHILE {} LOOP", print::expr(cond, D)?), out);
            print_nodes(body, depth + 1, out)?;
            line(depth, "END LOOP;", out);
        }
        Node::Call { name: n, args, ret } => {
            if ret.is_some() {
                return Err("a return code has no PL/pgSQL CALL form".into());
            }
            let mut a = Vec::new();
            for x in args {
                if x.output {
                    return Err("an OUTPUT argument has no PL/pgSQL CALL form".into());
                }
                let v = print::expr(&x.value, D)?;
                a.push(match &x.name {
                    Some(p) => format!("{} => {v}", ident(p)?),
                    None => v,
                });
            }
            line(depth, &format!("CALL {}({});", name(n)?, a.join(", ")), out);
        }
    }
    Ok(())
}
