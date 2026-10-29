//! Routines as abstract syntax: header, parameters and a body of
//! statements, blocks, `IF`, `WHILE` and calls (docs/verified-conversion.md,
//! C0 item 1 and phase C1), and error handling and transaction control
//! (phase C4: T-SQL `TRY`/`CATCH`, `THROW`, `BEGIN TRAN`/`COMMIT`/`ROLLBACK`;
//! PL/pgSQL `EXCEPTION` blocks, `RAISE`, `COMMIT`/`ROLLBACK`). Read from the
//! CST of T-SQL and PL/pgSQL routines; printed as PL/pgSQL by
//! `print_postgres`.
//!
//! Anything outside this shape is an error, not an approximation: a
//! routine that does not read is one whose conversion cannot be confirmed.

use crate::abstract_syntax::{parse_stmt_exact, tokens, Expr, Parser, Select, Stmt, Tok, Type, K};
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
    /// PL/pgSQL `FOR var IN query LOOP body END LOOP` (phase C3b): `var`
    /// as written, the query a plain `SELECT`. Other `FOR` loops (integer
    /// ranges, `EXECUTE`, cursors, `REVERSE`) are not read.
    ForQuery {
        var: String,
        query: Box<Select>,
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
    /// T-SQL `BEGIN TRY body END TRY BEGIN CATCH handler END CATCH` (one
    /// handler without conditions); PL/pgSQL `BEGIN body EXCEPTION WHEN
    /// ... THEN ... END` without a `DECLARE` section (a nested block; an
    /// exception section on the routine's own block is not read).
    Try {
        body: Vec<Node>,
        handlers: Vec<Handler>,
    },
    /// T-SQL `THROW` (no arguments: re-raise the error a `CATCH` handles)
    /// or `THROW number, message, state` (three arguments).
    Throw {
        args: Vec<Expr>,
    },
    /// PL/pgSQL `RAISE [level] ['format' [, arg ...]] [USING option =
    /// expr, ...]`. `level` is upper case as written (`None` when left
    /// out: a bare `RAISE` re-raises, otherwise the level is `EXCEPTION`);
    /// option names are upper case. A condition name or `SQLSTATE` after
    /// the level is not read.
    Raise {
        level: Option<String>,
        format: Option<String>,
        args: Vec<Expr>,
        using: Vec<(String, Expr)>,
    },
    /// Transaction control: T-SQL `BEGIN TRAN[SACTION] [name]`, `COMMIT
    /// [TRAN[SACTION] | WORK] [name]`, `ROLLBACK [TRAN[SACTION] | WORK]
    /// [name]`; PL/pgSQL `COMMIT`, `ROLLBACK`. `op` is `BEGIN`, `COMMIT`
    /// or `ROLLBACK`.
    Txn {
        op: String,
        name: Option<String>,
    },
}

/// An exception handler: the conditions it catches and its statements.
/// T-SQL `CATCH` has no conditions; PL/pgSQL condition names are lower
/// case (`others`, `division_by_zero`), `SQLSTATE '22012'` is
/// `sqlstate:22012`.
#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub conds: Vec<String>,
    pub body: Vec<Node>,
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
                [b] if b.kind() == BLOCK => match block(b, d)? {
                    (blk, hs) if hs.is_empty() => blk,
                    _ => {
                        return Err(
                            "an exception section on the routine's own block is not read yet"
                                .into(),
                        )
                    }
                },
                _ => return Err("a PL/pgSQL body that is not one block".into()),
            }
        }
        Dialect::Tsql => {
            let units: Vec<SyntaxNode> = body.children().filter(|c| c.kind().is_unit()).collect();
            match units.as_slice() {
                // `AS BEGIN ... END` and `AS stmt ...` run the same statements.
                [b] if b.kind() == BLOCK => block(b, d)?.0,
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

/// A block and the handlers of its exception section (PL/pgSQL).
fn block(b: &SyntaxNode, d: Dialect) -> RResult<(Block, Vec<Handler>)> {
    let mut out = Block::default();
    let mut units = Vec::new();
    let mut handlers = Vec::new();
    for c in b.children() {
        match c.kind() {
            DECLARE_SECTION => {
                for s in c.children().filter(|s| s.kind().is_unit()) {
                    out.decls.push(declaration(&s, d)?);
                }
            }
            EXCEPTION_SECTION if d == Dialect::Postgres => {
                handlers = exception_section(&c, d)?;
                if handlers.is_empty() {
                    return Err("an exception section without a handler".into());
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
    Ok((out, handlers))
}

/// The handlers of a PL/pgSQL `EXCEPTION` section: `WHEN cond [OR cond
/// ...] THEN statements`, a condition a name or `SQLSTATE 'xxxxx'`.
fn exception_section(sec: &SyntaxNode, d: Dialect) -> RResult<Vec<Handler>> {
    let own = upper_own(sec);
    if own != ["EXCEPTION"] {
        return Err(format!("an exception section written `{}`", own.join(" ")));
    }
    let mut out = Vec::new();
    for h in sec.children() {
        if h.kind() != EXCEPTION_HANDLER {
            return Err(format!("{:?} in an exception section", h.kind()));
        }
        let own = upper_own(&h);
        if own != ["WHEN", "THEN"] {
            return Err(format!("an exception handler written `{}`", own.join(" ")));
        }
        let kids: Vec<SyntaxNode> = h.children().collect();
        let [c, b] = kids.as_slice() else {
            return Err("an exception handler of an unexpected shape".into());
        };
        if c.kind() != CONDITION || b.kind() != BRANCH {
            return Err("an exception handler of an unexpected shape".into());
        }
        let toks = tokens(c);
        let mut conds = Vec::new();
        for part in toks.split(|t| t.kind == K::Word && t.up == "OR") {
            match part {
                [t] if t.kind == K::Word => conds.push(t.text.to_ascii_lowercase()),
                [s, v] if s.kind == K::Word && s.up == "SQLSTATE" && v.kind == K::Str => {
                    let code = v.text.trim_matches('\'');
                    if code.len() != 5 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
                        return Err(format!("the SQLSTATE `{}`", v.text));
                    }
                    conds.push(format!("sqlstate:{}", code.to_ascii_uppercase()));
                }
                _ => return Err("an exception condition of an unexpected shape".into()),
            }
        }
        out.push(Handler {
            conds,
            body: branch(b, d)?,
        });
    }
    Ok(out)
}

/// A node's own significant tokens (not those of any child node), upper
/// case, without semicolons.
fn upper_own(n: &SyntaxNode) -> Vec<String> {
    n.children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| cst::is_significant(t.kind()) && t.kind() != SEMICOLON)
        .map(|t| t.text().to_ascii_uppercase())
        .collect()
}

/// The units of a node (`TRY_BLOCK`, `CATCH_BLOCK`), whose own tokens
/// must be exactly `own`.
fn units_of(n: &SyntaxNode, own: &[&str], d: Dialect) -> RResult<Vec<Node>> {
    let toks = upper_own(n);
    if toks != own {
        return Err(format!("{:?} written `{}`", n.kind(), toks.join(" ")));
    }
    let units: Vec<SyntaxNode> = n.children().filter(|c| c.kind().is_unit()).collect();
    nodes(&units, d)
}

/// T-SQL `BEGIN TRY ... END TRY BEGIN CATCH ... END CATCH`.
fn try_catch(u: &SyntaxNode, d: Dialect) -> RResult<Node> {
    let kids: Vec<SyntaxNode> = u.children().collect();
    let [t, c] = kids.as_slice() else {
        return Err("a TRY without its CATCH".into());
    };
    if t.kind() != TRY_BLOCK || c.kind() != CATCH_BLOCK || !upper_own(u).is_empty() {
        return Err("a TRY/CATCH of an unexpected shape".into());
    }
    Ok(Node::Try {
        body: units_of(t, &["BEGIN", "TRY", "END", "TRY"], d)?,
        handlers: vec![Handler {
            conds: Vec::new(),
            body: units_of(c, &["BEGIN", "CATCH", "END", "CATCH"], d)?,
        }],
    })
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
    match parse_stmt_exact(&t, d)? {
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
        BLOCK => match block(u, d)? {
            (b, hs) if hs.is_empty() => Ok(Node::Block(b)),
            (b, _) if !b.decls.is_empty() => {
                Err("a block with declarations and an exception section is not read yet".into())
            }
            (b, handlers) => Ok(Node::Try {
                body: b.body,
                handlers,
            }),
        },
        TRY_CATCH if d == Dialect::Tsql => try_catch(u, d),
        IF_STMT => if_stmt(u, d),
        LOOP_STMT => {
            let first = cst::first_words(u, 1);
            if d == Dialect::Postgres && first.first().map(String::as_str) == Some("FOR") {
                return for_query(u, d);
            }
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

/// PL/pgSQL `FOR var IN SELECT ... LOOP body END LOOP`: no label, no
/// `REVERSE`, no `EXECUTE`, no integer range or cursor.
fn for_query(u: &SyntaxNode, d: Dialect) -> RResult<Node> {
    let own = upper_own(u);
    if own != ["FOR", "LOOP", "END", "LOOP"] {
        return Err(format!("a FOR loop written `{}`", own.join(" ")));
    }
    let kids: Vec<SyntaxNode> = u.children().collect();
    let [c, b] = kids.as_slice() else {
        return Err("a FOR loop of an unexpected shape".into());
    };
    if c.kind() != CONDITION || b.kind() != BRANCH {
        return Err("a FOR loop of an unexpected shape".into());
    }
    let t = tokens(c);
    let (var, rest) = match t.as_slice() {
        [v, i, rest @ ..] if v.kind == K::Word && i.kind == K::Word && i.up == "IN" => {
            (v.text.clone(), rest)
        }
        _ => return Err("only FOR loops over a query are read".into()),
    };
    if !rest.first().is_some_and(|x| x.up == "SELECT") {
        return Err("only FOR loops over a SELECT are read".into());
    }
    let mut p = Parser::ext(rest, d);
    let query = p.select()?;
    if !p.at_end() {
        return Err(p.err("unexpected tokens after the FOR query"));
    }
    whole(&p)?;
    Ok(Node::ForQuery {
        var,
        query: Box::new(query),
        body: branch(b, d)?,
    })
}

/// Refuses what the extended parser read but left out of the tree: the
/// checker sees only the tree.
fn whole(p: &Parser) -> RResult<()> {
    match p.dropped.first() {
        Some(what) => Err(format!("{what} is not kept in the syntax tree")),
        None => Ok(()),
    }
}

fn condition(c: &SyntaxNode, d: Dialect) -> RResult<Expr> {
    let t = tokens(c);
    let mut p = Parser::ext(&t, d);
    let e = p.expr()?;
    if !p.at_end() {
        return Err(p.err("unexpected tokens after the condition"));
    }
    whole(&p)?;
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
        (Dialect::Tsql, "THROW") => throw(&t, d),
        (Dialect::Postgres, "RAISE") => raise(&t, d),
        (Dialect::Tsql, "BEGIN")
            if t.len() > 1 && matches!(t[1].up.as_str(), "TRAN" | "TRANSACTION") =>
        {
            txn(&t, d)
        }
        (_, "COMMIT" | "ROLLBACK") => txn(&t, d),
        _ => Ok(Node::Stmt(parse_stmt_exact(&t, d)?)),
    }
}

/// T-SQL `THROW` or `THROW number, message, state`.
fn throw(t: &[Tok], d: Dialect) -> RResult<Node> {
    let mut p = Parser::ext(t, d);
    p.i += 1;
    let mut args = Vec::new();
    if !p.at_end() {
        loop {
            args.push(p.expr()?);
            if !p.eat_kind(K::Comma) {
                break;
            }
        }
        if args.len() != 3 {
            return Err("THROW takes no arguments or three".into());
        }
    }
    if !p.at_end() {
        return Err(p.err("unexpected tokens in THROW"));
    }
    whole(&p)?;
    Ok(Node::Throw { args })
}

/// PL/pgSQL `RAISE [level] ['format' [, expr ...]] [USING name = expr, ...]`.
fn raise(t: &[Tok], d: Dialect) -> RResult<Node> {
    const LEVELS: [&str; 6] = ["DEBUG", "LOG", "INFO", "NOTICE", "WARNING", "EXCEPTION"];
    let mut p = Parser::ext(t, d);
    p.i += 1;
    let mut level = None;
    if let Some(l) = LEVELS.iter().find(|l| p.is_kw(l)) {
        level = Some(l.to_string());
        p.i += 1;
    }
    let (mut format, mut args, mut using) = (None, Vec::new(), Vec::new());
    if p.is_kind(K::Str) {
        match p.expr()? {
            Expr::Str(s) => format = Some(s),
            _ => return Err("a RAISE format that is not a string literal".into()),
        }
        while p.eat_kind(K::Comma) {
            args.push(p.expr()?);
        }
    } else if !p.at_end() && !p.is_kw("USING") {
        return Err(p.err("a RAISE with a condition name or SQLSTATE is not read yet"));
    }
    if p.eat_kw("USING") {
        loop {
            let name = p.any_ident()?.to_ascii_uppercase();
            if !p.eat_op("=") {
                return Err(p.err("expected = in RAISE USING"));
            }
            using.push((name, p.expr()?));
            if !p.eat_kind(K::Comma) {
                break;
            }
        }
    }
    if !p.at_end() {
        return Err(p.err("unexpected tokens in RAISE"));
    }
    whole(&p)?;
    Ok(Node::Raise {
        level,
        format,
        args,
        using,
    })
}

/// Transaction control. T-SQL: `BEGIN TRAN[SACTION] [name]`, `COMMIT |
/// ROLLBACK [TRAN[SACTION] | WORK] [name]`; PL/pgSQL: `COMMIT`,
/// `ROLLBACK`. Anything else (`DISTRIBUTED`, `WITH MARK`, `AND CHAIN`,
/// delayed durability, a PL/pgSQL `WORK` or `TRANSACTION`) is not read.
fn txn(t: &[Tok], d: Dialect) -> RResult<Node> {
    let op = t[0].up.clone();
    let mut i = 1;
    let mut name = None;
    if d == Dialect::Tsql {
        if t.get(i)
            .is_some_and(|x| matches!(x.up.as_str(), "TRAN" | "TRANSACTION" | "WORK"))
        {
            i += 1;
        }
        if let Some(x) = t.get(i) {
            if matches!(x.kind, K::Word | K::Var) && x.up != "WITH" {
                name = Some(x.text.clone());
                i += 1;
            }
        }
    }
    if i != t.len() {
        let text: Vec<&str> = t.iter().map(|x| x.text.as_str()).collect();
        return Err(format!("{op} written `{}`", text.join(" ")));
    }
    Ok(Node::Txn { op, name })
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
    whole(&p)?;
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
    whole(&p)?;
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
        Node::ForQuery { var, query, body } => {
            line(
                depth,
                &format!("FOR {} IN {} LOOP", ident(var)?, print::select(query, D)?),
                out,
            );
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
        Node::Try { body, handlers } => {
            if handlers.is_empty() || handlers.iter().any(|h| h.conds.is_empty()) {
                return Err("a handler without conditions has no PL/pgSQL spelling".into());
            }
            line(depth, "BEGIN", out);
            print_nodes(body, depth + 1, out)?;
            line(depth, "EXCEPTION", out);
            for h in handlers {
                let mut cs = Vec::new();
                for c in &h.conds {
                    cs.push(match c.strip_prefix("sqlstate:") {
                        Some(code) => format!("SQLSTATE '{code}'"),
                        None if c.chars().all(|x| x.is_ascii_alphanumeric() || x == '_') => {
                            c.clone()
                        }
                        None => return Err(format!("the condition `{c}`")),
                    });
                }
                line(depth + 1, &format!("WHEN {} THEN", cs.join(" OR ")), out);
                print_nodes(&h.body, depth + 2, out)?;
            }
            line(depth, "END;", out);
        }
        Node::Throw { .. } => return Err("THROW has no PL/pgSQL spelling".into()),
        Node::Raise {
            level,
            format,
            args,
            using,
        } => {
            let mut s = String::from("RAISE");
            if let Some(l) = level {
                s.push(' ');
                s.push_str(l);
            }
            if let Some(f) = format {
                s.push(' ');
                s.push_str(&print::expr(&Expr::Str(f.clone()), D)?);
                for a in args {
                    s.push_str(", ");
                    s.push_str(&print::expr(a, D)?);
                }
            } else if !args.is_empty() {
                return Err("RAISE arguments without a format".into());
            }
            if !using.is_empty() {
                let mut u = Vec::new();
                for (n, e) in using {
                    u.push(format!("{n} = {}", print::expr(e, D)?));
                }
                s.push_str(" USING ");
                s.push_str(&u.join(", "));
            }
            line(depth, &format!("{s};"), out);
        }
        Node::Txn { op, name } => {
            if name.is_some() || !matches!(op.as_str(), "COMMIT" | "ROLLBACK") {
                return Err(format!(
                    "the transaction statement {op} has no PL/pgSQL spelling"
                ));
            }
            line(depth, &format!("{op};"), out);
        }
    }
    Ok(())
}
