//! Routine shape: kind, signature, local declarations and the ordered list of
//! data statements, plus everything the statement-level comparison does not
//! cover (control flow other than the recognised transaction protocols).

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{parse_stmt, tokens, Parser, Stmt, Tok, Type, K};
use super::schema::Ty;
use crate::artifact::Analyzed;
use crate::classify::{stmt_kind, StmtKind};
use crate::model::Assumption;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    View,
    ScalarFn,
    TableFn,
    Proc,
    Script,
}

impl UnitKind {
    pub fn label(self) -> &'static str {
        match self {
            UnitKind::View => "view",
            UnitKind::ScalarFn => "scalar function",
            UnitKind::TableFn => "table function",
            UnitKind::Proc => "procedure",
            UnitKind::Script => "script",
        }
    }
}

/// Whose privileges the routine's statements are checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// The caller's: T-SQL `EXECUTE AS CALLER` (the default), PostgreSQL
    /// `SECURITY INVOKER` (the default), Db2 for i `USRPRF = *USER`.
    Caller,
    /// The routine owner's: T-SQL `EXECUTE AS OWNER`, PostgreSQL
    /// `SECURITY DEFINER`, Db2 for i `USRPRF = *OWNER`.
    Owner,
    /// Db2 for i without `SET OPTION USRPRF`: `*NAMING`, decided by the naming
    /// convention the routine was created with.
    Naming,
}

impl Security {
    pub fn label(self) -> &'static str {
        match self {
            Security::Caller => "the caller's privileges",
            Security::Owner => "the owner's privileges",
            Security::Naming => "the privileges its naming convention picks",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    /// Lower case, without `@`.
    pub name: String,
    pub ty: Option<Ty>,
    pub out: bool,
    /// Default value tokens (keywords upper-cased), when declared.
    pub default: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: Option<Ty>,
}

/// One data statement of the body.
#[derive(Debug, Clone)]
pub struct LStmt {
    pub node: SyntaxNode,
    pub toks: Vec<Tok>,
    pub ast: Result<Stmt, String>,
    /// PostgreSQL declaration-section initializers are lowered as assignments.
    pub from_declare: bool,
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub dialect: Dialect,
    pub kind: UnitKind,
    pub params: Vec<Param>,
    pub returns: Option<Ty>,
    /// Declared RETURNS TABLE columns and their types.
    pub ret_cols: Vec<(String, Option<Ty>)>,
    /// PostgreSQL `LANGUAGE sql`: the last query is the result.
    pub sql_body: bool,
    pub locals: Vec<Local>,
    pub stmts: Vec<LStmt>,
    /// Constructs outside the comparison: (node, description).
    pub uncovered: Vec<(SyntaxNode, String)>,
    /// A script's top-level `CREATE TABLE` statements, with the number of
    /// data statements before each.
    pub tables: Vec<(SyntaxNode, usize)>,
    /// A script's top-level `CREATE INDEX`, `CREATE SEQUENCE` and `CREATE
    /// SCHEMA` statements (`ddl.rs`), placed like `tables`.
    pub ddl: Vec<(SyntaxNode, usize)>,
    /// Explicit transaction control or a recognised rollback-and-rethrow protocol.
    pub txn_control: bool,
    /// T-SQL: SET XACT_ABORT ON or a CATCH that rethrows, so a failing
    /// statement ends the routine instead of letting it continue.
    pub stops_on_error: bool,
    /// Top-level BEGIN TRAN / COMMIT statements, checked for position.
    pub txn_stmts: Vec<(SyntaxNode, StmtKind)>,
    pub notes: Vec<String>,
    /// Security context of a routine (scripts and views: the caller's).
    pub security: Security,
    /// The header text that sets it and its byte range; `None` for the default.
    pub security_cite: Option<(String, (usize, usize))>,
    /// Assumptions the routine options need whatever the other side is.
    pub assumptions: Vec<Assumption>,
    /// PostgreSQL routine-level `SET` clause, which (like SECURITY DEFINER)
    /// forbids transaction control in a procedure.
    pub sets_config: bool,
    /// PostgreSQL STABLE/IMMUTABLE, Db2 for i DETERMINISTIC.
    pub non_volatile: Option<String>,
    /// Byte range of the routine header up to its name, for findings.
    pub header_at: (usize, usize),
}

pub fn lower(a: &Analyzed) -> Unit {
    let d = a.dialect();
    let mut u = Unit {
        dialect: d,
        kind: UnitKind::Script,
        params: Vec::new(),
        returns: None,
        ret_cols: Vec::new(),
        sql_body: false,
        locals: Vec::new(),
        stmts: Vec::new(),
        uncovered: Vec::new(),
        tables: Vec::new(),
        ddl: Vec::new(),
        txn_control: false,
        stops_on_error: false,
        txn_stmts: Vec::new(),
        notes: Vec::new(),
        security: Security::Caller,
        security_cite: None,
        assumptions: Vec::new(),
        sets_config: false,
        non_volatile: None,
        header_at: (0, 0),
    };
    let routines: Vec<SyntaxNode> = a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .collect();
    match routines.as_slice() {
        [] => walk(&a.root, &mut u),
        [r] => {
            header(r, &mut u);
            // Statements outside the routine (other than batch separators) are
            // not covered, except the tables it creates, compared table against
            // table before (0) or after (1) the routine.
            for n in a.root.descendants() {
                if n.kind() == SyntaxKind::STATEMENT
                    && !r.text_range().contains_range(n.text_range())
                {
                    let after = n.text_range().start() >= r.text_range().end();
                    if is_create_table(&n, d) {
                        u.tables.push((n.clone(), usize::from(after)));
                    } else if is_create_ddl(&n, d) {
                        u.ddl.push((n.clone(), usize::from(after)));
                    } else {
                        u.uncovered
                            .push((n.clone(), "a statement outside the routine".into()));
                    }
                }
            }
            for c in r.children() {
                if c.kind() == SyntaxKind::ROUTINE_BODY {
                    walk(&c, &mut u);
                }
            }
        }
        [first, ..] => {
            u.uncovered
                .push((first.clone(), "several routines in one artifact".into()));
            // Every statement outside the routines is reported on its own
            // line (or compared, for tables, indexes, sequences and schemas),
            // so none counts as compared because the routines were not.
            for n in a.root.descendants() {
                if n.kind() != SyntaxKind::STATEMENT
                    || routines
                        .iter()
                        .any(|r| r.text_range().contains_range(n.text_range()))
                {
                    continue;
                }
                let before = routines
                    .iter()
                    .filter(|r| r.text_range().end() <= n.text_range().start())
                    .count();
                if is_create_table(&n, d) {
                    u.tables.push((n.clone(), before));
                } else if is_create_ddl(&n, d) {
                    u.ddl.push((n.clone(), before));
                } else {
                    u.uncovered
                        .push((n.clone(), "a statement outside the routines".into()));
                }
            }
        }
    }
    if let [r] = routines.as_slice() {
        super::options::body_checks(r, &mut u);
    }
    // The transaction protocol is understood only as BEGIN before the first
    // data statement and COMMIT after the last one (RETURN aside).
    let data: Vec<(usize, bool)> = u
        .stmts
        .iter()
        .map(|s| {
            (
                usize::from(s.node.text_range().start()),
                matches!(s.ast, Ok(Stmt::Return(_))),
            )
        })
        .collect();
    for (node, kind) in std::mem::take(&mut u.txn_stmts) {
        let at = usize::from(node.text_range().start());
        let misplaced = match kind {
            StmtKind::TxnBegin => data.iter().any(|(o, _)| *o < at),
            _ => data.iter().any(|(o, is_return)| *o > at && !is_return),
        };
        if misplaced {
            u.uncovered
                .push((node, format!("{} between data statements", kind.label())));
        }
    }
    u
}

fn header(r: &SyntaxNode, u: &mut Unit) {
    let d = u.dialect;
    let toks: Vec<Tok> = cst::header_tokens(r)
        .iter()
        .map(Tok::from_syntax)
        .filter(|t| t.kind != K::Semi)
        .collect();
    let Some(kw) = toks.iter().position(|t| {
        matches!(
            t.up.as_str(),
            "VIEW" | "FUNCTION" | "PROCEDURE" | "PROC" | "TRIGGER"
        )
    }) else {
        u.uncovered
            .push((r.clone(), "an unrecognised routine header".into()));
        return;
    };
    let mut p = Parser::new(&toks[kw + 1..], d);
    if p.qname().is_err() {
        u.uncovered
            .push((r.clone(), "an unrecognised routine name".into()));
        return;
    }
    let mut i = kw + 1 + p.i;
    if let (Some(first), Some(last)) = (toks.first(), toks.get(i - 1)) {
        u.header_at = (first.off, last.off + last.text.len());
    }
    u.kind = match toks[kw].up.as_str() {
        "VIEW" => UnitKind::View,
        "FUNCTION" => UnitKind::ScalarFn,
        "PROCEDURE" | "PROC" => UnitKind::Proc,
        _ => {
            u.uncovered.push((r.clone(), "a trigger".into()));
            return;
        }
    };
    if u.kind == UnitKind::View {
        if toks.get(i).is_some_and(|t| t.kind == K::LParen) {
            u.uncovered.push((r.clone(), "a view column list".into()));
        }
        return;
    }
    // Parameter list.
    let param_toks: &[Tok] = if toks.get(i).is_some_and(|t| t.kind == K::LParen) {
        let close = matching(&toks, i);
        let s = &toks[i + 1..close];
        i = close + 1;
        s
    } else if d == Dialect::Tsql {
        let end = tsql_params_end(&toks, i);
        let s = &toks[i..end];
        i = end;
        s
    } else {
        &[]
    };
    if !param_toks.is_empty() {
        for pt in split_commas(param_toks) {
            match param(pt, d) {
                Some(p) => u.params.push(p),
                None => u
                    .uncovered
                    .push((r.clone(), "an unrecognised parameter".into())),
            }
        }
    }
    super::options::options(r, &toks, i, u);
}

/// End of a T-SQL parameter list written without parentheses, starting at
/// `start`: the `AS`, `WITH`, `RETURNS` or `FOR` that ends the header part.
/// The `AS` in `@x AS int` belongs to the parameter.
pub(crate) fn tsql_params_end(toks: &[Tok], start: usize) -> usize {
    let mut depth = 0;
    for (j, t) in toks.iter().enumerate().skip(start) {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Word
                if depth == 0
                    && matches!(t.up.as_str(), "AS" | "WITH" | "RETURNS" | "FOR")
                    && !(t.up == "AS" && j > 0 && toks[j - 1].kind == K::Var) =>
            {
                return j;
            }
            _ => {}
        }
    }
    toks.len()
}

pub(crate) fn matching(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0;
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

pub(crate) fn split_commas(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
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
    if start < toks.len() {
        out.push(&toks[start..]);
    }
    out
}

pub(crate) fn param(toks: &[Tok], d: Dialect) -> Option<Param> {
    let mut i = 0;
    let mut out = false;
    while let Some(t) = toks.get(i) {
        match t.up.as_str() {
            "IN" => i += 1,
            "OUT" | "INOUT" => {
                out = true;
                i += 1;
            }
            "VARIADIC" => return None,
            _ => break,
        }
    }
    let name_tok = toks.get(i)?;
    let name = match name_tok.kind {
        K::Var => name_tok.text.trim_start_matches('@').to_lowercase(),
        K::Word | K::Quoted if toks.len() > i + 1 => cst::unquote(&name_tok.text).to_lowercase(),
        _ => return None,
    };
    i += 1;
    if toks.get(i).is_some_and(|t| t.up == "AS") {
        i += 1;
    }
    let mut p = Parser::new(&toks[i..], d);
    let ty: Type = p.ty().ok()?;
    let mut default: Option<Vec<String>> = None;
    for t in &toks[i + p.i..] {
        match t.up.as_str() {
            "OUTPUT" | "OUT" => out = true,
            "READONLY" => return None,
            "=" | "DEFAULT" if default.is_none() => default = Some(Vec::new()),
            _ => {
                // Anything else after the type (VARYING, NOT NULL, a type
                // modifier the type parser left over) is not understood.
                let v = default.as_mut()?;
                v.push(if t.kind == K::Word {
                    t.up.clone()
                } else {
                    t.text.clone()
                });
            }
        }
    }
    Some(Param {
        name,
        ty: Some(Ty::from_type(&ty, d, false)),
        out,
        default: default.map(|v| v.join(" ")),
    })
}

fn walk(node: &SyntaxNode, u: &mut Unit) {
    for c in cst::structural_children(node) {
        unit(&c, u);
    }
}

fn unit(c: &SyntaxNode, u: &mut Unit) {
    let d = u.dialect;
    match c.kind() {
        SyntaxKind::SOURCE_FILE
        | SyntaxKind::BATCH
        | SyntaxKind::ROUTINE_BODY
        | SyntaxKind::TRY_BLOCK => walk(c, u),
        SyntaxKind::BLOCK => {
            let w = cst::first_words(c, 2);
            if w.get(1).is_some_and(|x| x == "ATOMIC") {
                u.txn_control = true;
            }
            walk(c, u)
        }
        SyntaxKind::DECLARE_SECTION => {
            for s in cst::structural_children(c) {
                pg_declaration(&s, u);
            }
        }
        SyntaxKind::STATEMENT => statement(c, u),
        SyntaxKind::TRY_CATCH => {
            for part in c.children() {
                match part.kind() {
                    SyntaxKind::TRY_BLOCK => walk(&part, u),
                    SyntaxKind::CATCH_BLOCK => {
                        if is_rollback_rethrow(&part, d) {
                            u.txn_control = true;
                            u.stops_on_error = true;
                        } else {
                            u.uncovered.push((part.clone(), "a CATCH block".into()));
                        }
                    }
                    _ => {}
                }
            }
        }
        SyntaxKind::HANDLER => {
            let words = cst::first_words(c, 5).join(" ");
            if words.starts_with("DECLARE EXIT HANDLER FOR SQLEXCEPTION")
                && is_rollback_rethrow(c, d)
            {
                u.txn_control = true;
            } else {
                u.uncovered.push((c.clone(), "an error handler".into()));
            }
        }
        k => {
            let what = match k {
                SyntaxKind::IF_STMT => "an IF statement",
                SyntaxKind::LOOP_STMT => "a loop",
                SyntaxKind::CASE_STMT => "a CASE statement",
                SyntaxKind::EXCEPTION_SECTION => "an EXCEPTION section",
                SyntaxKind::LABEL => "a label",
                SyntaxKind::ROUTINE => "a nested routine",
                SyntaxKind::ERROR => "unparsed text",
                _ => "an unsupported construct",
            };
            u.uncovered.push((c.clone(), what.into()));
        }
    }
}

fn statement(c: &SyntaxNode, u: &mut Unit) {
    let d = u.dialect;
    let kind = stmt_kind(c, d);
    let words = cst::first_words(c, 3);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match kind {
        StmtKind::Select
        | StmtKind::Insert
        | StmtKind::Update
        | StmtKind::Delete
        | StmtKind::Merge
        | StmtKind::Assign
        | StmtKind::Return
        | StmtKind::Declare => {
            let toks = tokens(c);
            let mut ast = parse_stmt(&toks, d);
            let mut from_declare = false;
            if let Ok(Stmt::Declare { name, ty, default }) = &ast {
                u.locals.push(Local {
                    name: name.trim_start_matches('@').to_lowercase(),
                    ty: ty.as_ref().map(|t| Ty::from_type(t, d, false)),
                });
                let Some(init) = default else { return };
                // `DECLARE @x T = v` is an assignment of the initial value.
                let target = if name.starts_with('@') {
                    super::ast::Expr::Var(name.clone())
                } else {
                    super::ast::Expr::Name(vec![name.clone()])
                };
                ast = Ok(Stmt::Assign(target, init.clone()));
                from_declare = true;
            }
            u.stmts.push(LStmt {
                node: c.clone(),
                toks,
                ast,
                from_declare,
            });
        }
        // A script's own `CREATE TABLE`: compared table against table
        // (`table.rs`), in its place among the data statements.
        StmtKind::Ddl if u.kind == UnitKind::Script && is_create_table(c, d) => {
            u.tables.push((c.clone(), u.stmts.len()));
        }
        // Indexes, sequences and schemas: compared statement against
        // statement (`ddl.rs`).
        StmtKind::Ddl if u.kind == UnitKind::Script && is_create_ddl(c, d) => {
            u.ddl.push((c.clone(), u.stmts.len()));
        }
        StmtKind::Ddl if u.kind == UnitKind::Script && u.stmts.is_empty() => {
            // `CREATE [OR REPLACE] VIEW name AS query` parsed as one statement.
            let toks = tokens(c);
            let mut i = 1;
            if toks.get(i).is_some_and(|t| t.up == "OR")
                && toks.get(i + 1).is_some_and(|t| t.up == "REPLACE")
            {
                i += 2;
            }
            let as_at = toks.iter().position(|t| t.kind == K::Word && t.up == "AS");
            match (toks.get(i).map(|t| t.up.as_str()), as_at) {
                (Some("VIEW"), Some(at))
                    if at == i + 2
                        || (at > i + 2
                            && toks[i + 2..at].iter().all(|t| {
                                matches!(t.kind, K::Dot | K::Word | K::Quoted) || t.text == "/"
                            })) =>
                {
                    let body: Vec<Tok> = toks[at + 1..].to_vec();
                    let ast = parse_stmt(&body, d);
                    u.kind = UnitKind::View;
                    u.stmts.push(LStmt {
                        node: c.clone(),
                        toks: body,
                        ast,
                        from_declare: false,
                    });
                }
                _ => u
                    .uncovered
                    .push((c.clone(), format!("a DDL statement `{}`", words.join(" ")))),
            }
        }
        StmtKind::SessionSetting if d == Dialect::Tsql && w(1) == "NOCOUNT" => {
            note(
                u,
                "SET NOCOUNT ON ignored (it only suppresses row-count messages)",
            );
        }
        StmtKind::TxnBegin | StmtKind::TxnCommit => {
            if kind == StmtKind::TxnCommit && w(0) == "END" {
                u.uncovered.push((c.clone(), "END".into()));
            } else {
                u.txn_control = true;
                u.txn_stmts.push((c.clone(), kind));
            }
        }
        StmtKind::TxnSetting if w(1) == "XACT_ABORT" && w(2) == "ON" => {
            u.txn_control = true;
            u.stops_on_error = true;
        }
        _ => {
            let what = format!(
                "{} `{}`",
                crate::treediff::article(kind.label()),
                words.join(" ")
            );
            u.uncovered.push((c.clone(), what));
        }
    }
}

/// A top-level `CREATE [prefix] TABLE` statement.
fn is_create_table(c: &SyntaxNode, d: Dialect) -> bool {
    let words = cst::first_words(c, 3);
    stmt_kind(c, d) == StmtKind::Ddl
        && words.first().is_some_and(|w| w == "CREATE")
        && (words.get(1).is_some_and(|w| w == "TABLE")
            || words.get(2).is_some_and(|w| w == "TABLE"))
        && c.parent()
            .is_some_and(|p| matches!(p.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH))
}

/// A top-level `CREATE … INDEX`, `CREATE SEQUENCE` or `CREATE SCHEMA`.
fn is_create_ddl(c: &SyntaxNode, d: Dialect) -> bool {
    stmt_kind(c, d) == StmtKind::Ddl
        && super::ddl::is_ddl(&cst::first_words(c, 7))
        && c.parent()
            .is_some_and(|p| matches!(p.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH))
}

fn note(u: &mut Unit, s: &str) {
    if !u.notes.iter().any(|x| x == s) {
        u.notes.push(s.to_string());
    }
}

/// PL/pgSQL declaration: `name type [:= | = | DEFAULT expr]`.
fn pg_declaration(s: &SyntaxNode, u: &mut Unit) {
    let toks = tokens(s);
    let d = u.dialect;
    let mut p = Parser::new(&toks, d);
    let ok = (|| -> Result<(String, Type, usize), String> {
        let n = p.qname()?;
        if n.len() != 1 {
            return Err("qualified".into());
        }
        let ty = p.ty()?;
        Ok((n[0].to_lowercase(), ty, p.i))
    })();
    let Ok((name, ty, at)) = ok else {
        u.uncovered.push((s.clone(), "a declaration".into()));
        return;
    };
    if matches!(ty.name.as_str(), "CONSTANT" | "CURSOR" | "RECORD" | "ALIAS") {
        u.uncovered.push((s.clone(), "a declaration".into()));
        return;
    }
    u.locals.push(Local {
        name: name.clone(),
        ty: Some(Ty::from_type(&ty, d, false)),
    });
    let rest = &toks[at..];
    if rest.is_empty() {
        return;
    }
    let is_init = rest[0].text == ":=" || rest[0].text == "=" || rest[0].up == "DEFAULT";
    if !is_init {
        u.uncovered.push((s.clone(), "a declaration".into()));
        return;
    }
    let mut p = Parser::new(&rest[1..], d);
    let ast = p.expr().and_then(|e| {
        if p.at_end() {
            Ok(Stmt::Assign(super::ast::Expr::Name(vec![name.clone()]), e))
        } else {
            Err("trailing tokens".into())
        }
    });
    u.stmts.push(LStmt {
        node: s.clone(),
        toks,
        ast,
        from_declare: true,
    });
}

/// `[IF @@TRANCOUNT > 0] ROLLBACK; THROW;` (T-SQL CATCH) or
/// `BEGIN ROLLBACK; RESIGNAL; END` (Db2 EXIT handler).
fn is_rollback_rethrow(node: &SyntaxNode, d: Dialect) -> bool {
    let stmts: Vec<SyntaxNode> = node
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        .collect();
    let texts: Vec<String> = stmts
        .iter()
        .map(|s| {
            tokens(s)
                .iter()
                .map(|t| t.up.clone())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let ifs: Vec<SyntaxNode> = node
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::IF_STMT)
        .collect();
    let others = node.descendants().any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::LOOP_STMT
                | SyntaxKind::CASE_STMT
                | SyntaxKind::TRY_CATCH
                | SyntaxKind::HANDLER
        ) && &n != node
    });
    if others {
        return false;
    }
    let rollback = |t: &str| {
        matches!(
            t,
            "ROLLBACK" | "ROLLBACK TRAN" | "ROLLBACK TRANSACTION" | "ROLLBACK WORK"
        )
    };
    match d {
        Dialect::Tsql => {
            let if_ok = match ifs.as_slice() {
                [] => true,
                [i] => {
                    let cond = i
                        .children()
                        .find(|c| c.kind() == SyntaxKind::CONDITION)
                        .map(|c| {
                            tokens(&c)
                                .iter()
                                .map(|t| t.up.clone())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    cond == "@@TRANCOUNT > 0" || cond == "XACT_STATE ( ) <> 0"
                }
                _ => false,
            };
            if_ok && texts.len() == 2 && rollback(&texts[0]) && texts[1] == "THROW"
        }
        Dialect::Db2i => {
            ifs.is_empty() && texts.len() == 2 && rollback(&texts[0]) && texts[1] == "RESIGNAL"
        }
        Dialect::Postgres => false,
    }
}
