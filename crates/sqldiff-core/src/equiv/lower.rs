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
    /// A T-SQL AFTER trigger's body for one event, or the PostgreSQL
    /// statement-trigger function that event runs (`trigger.rs`).
    Trigger,
}

impl UnitKind {
    pub fn label(self) -> &'static str {
        match self {
            UnitKind::View => "view",
            UnitKind::ScalarFn => "scalar function",
            UnitKind::TableFn => "table function",
            UnitKind::Proc => "procedure",
            UnitKind::Script => "script",
            UnitKind::Trigger => "trigger",
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
    /// The type as declared.
    pub decl: Option<Type>,
    pub out: bool,
    /// Default value tokens (keywords upper-cased), when declared.
    pub default: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: Option<Ty>,
    /// The type as declared (`ty` folds precisions and lengths away).
    pub decl: Option<Type>,
    /// The declaring statement.
    pub node: SyntaxNode,
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

/// Where a call sits: after this many data statements of the body, or
/// before (`Some(false)`) or after (`Some(true)`) the file's one routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallAt {
    pub outside: Option<bool>,
    pub after: usize,
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
    /// Procedure calls (`EXEC`, `CALL`) and object comments
    /// (`sp_addextendedproperty`, `COMMENT ON`), compared call against call
    /// (`call.rs`), with where each one sits.
    pub calls: Vec<(SyntaxNode, CallAt)>,
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
    /// The statements run as one unit that stops at the first unhandled
    /// error: a routine, a PostgreSQL DO block, a Db2 for i compound
    /// statement. Plain script statements run one by one, and the client
    /// decides whether a failed one stops the script.
    pub in_block: bool,
    /// Identities of `locals` across the two sides, when they are matched by
    /// name instead of declaration order (`script::name_locals`).
    pub local_ids: Option<Vec<String>>,
    /// A trigger body lowered for one event (`trigger.rs`).
    pub trigger: Option<TrigCtx>,
    /// Name of the artifact's one routine: names the temporary table each
    /// table variable became (`classify::table_variable_table`).
    pub routine: Option<String>,
    /// T-SQL compared against PostgreSQL or Db2 for i: the other side's
    /// temporary tables (`classify::temp_table_names`). A table variable
    /// converted to one of them is named as it; otherwise `@t` keeps the key
    /// `temp:t`, which a `#t` replacing it shares.
    pub other_temps: std::collections::HashSet<String>,
    /// T-SQL `DECLARE @t TABLE`, PostgreSQL `CREATE TEMP TABLE`, Db2 for i
    /// `DECLARE GLOBAL TEMPORARY TABLE` statements, with the number of
    /// statements before each: a table variable compared against the
    /// temporary table it became (`mod.rs`).
    pub table_vars: Vec<(SyntaxNode, usize)>,
    /// PostgreSQL `DROP TABLE IF EXISTS`, placed like `table_vars`: the drop
    /// before a converted table variable's `CREATE TEMP TABLE`.
    pub temp_drops: Vec<(SyntaxNode, usize)>,
}

/// The event a trigger body is lowered for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrigCtx {
    /// The trigger's table, as written (parts unquoted).
    pub table: Vec<String>,
    /// The image the statement's rows are in: `inserted` (INSERT, UPDATE) or
    /// `deleted` (DELETE).
    pub rows: &'static str,
    /// The image the event has no rows for: T-SQL gives it empty, the
    /// PostgreSQL conversion writes `(SELECT * FROM t WHERE false)`.
    pub missing: Option<&'static str>,
    /// The body starts with the empty-image guard: T-SQL `IF @@ROWCOUNT = 0
    /// RETURN`, PostgreSQL `IF NOT EXISTS (SELECT 1 FROM rows) THEN RETURN
    /// NULL; END IF;`, or either dialect's test for rows. Only the first
    /// statement can be the guard.
    pub guard: bool,
    /// The guard reads T-SQL `@@ROWCOUNT`, which counts a MERGE's rows of
    /// every action.
    pub rowcount: bool,
    started: bool,
}

impl TrigCtx {
    pub fn new(table: Vec<String>, rows: &'static str, missing: Option<&'static str>) -> Self {
        TrigCtx {
            table,
            rows,
            missing,
            guard: false,
            rowcount: false,
            started: false,
        }
    }
}

fn empty(d: Dialect) -> Unit {
    Unit {
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
        calls: Vec::new(),
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
        in_block: false,
        local_ids: None,
        trigger: None,
        routine: None,
        other_temps: Default::default(),
        table_vars: Vec::new(),
        temp_drops: Vec::new(),
    }
}

pub fn lower(a: &Analyzed) -> Unit {
    let d = a.dialect();
    let mut u = empty(d);
    let routines: Vec<SyntaxNode> = a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .collect();
    match routines.as_slice() {
        [] => walk(&a.root, &mut u),
        [r] => {
            u.in_block = true;
            u.routine = sqldiff_syntax::ast::routine_name(r);
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
                    } else if is_call(&n, d) {
                        u.calls.push((
                            n.clone(),
                            CallAt {
                                outside: Some(after),
                                after: 0,
                            },
                        ));
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
                } else if is_comment(&n, d) {
                    // Object comments around the routines are still compared.
                    let at = CallAt {
                        outside: Some(true),
                        after: 0,
                    };
                    u.calls.push((n.clone(), at));
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
    place_txn(&mut u);
    u
}

/// One part of a script that runs as a whole: a T-SQL batch that needs
/// variables or control flow, a PostgreSQL DO block, a Db2 for i compound
/// statement (`block`), or the plain statements between them.
#[derive(Debug, Clone)]
pub struct Segment {
    pub nodes: Vec<SyntaxNode>,
    pub block: bool,
}

/// A script's segments in order; `None` when the artifact holds a routine
/// (other than a DO block) or a PostgreSQL DO block in another language.
/// Consecutive plain T-SQL batches form one segment, as do consecutive plain
/// statements on the other side: a conversion turns a T-SQL batch with
/// variables or control flow into one block and leaves the others' statements
/// as they are.
pub fn segments(a: &Analyzed) -> Option<Vec<Segment>> {
    let d = a.dialect();
    let mut out: Vec<Segment> = Vec::new();
    let mut push = |nodes: Vec<SyntaxNode>, block: bool| {
        if nodes.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if !block && !last.block => last.nodes.extend(nodes),
            _ => out.push(Segment { nodes, block }),
        }
    };
    let top: Vec<SyntaxNode> = cst::structural_children(&a.root);
    if d == Dialect::Tsql {
        if a.root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::ROUTINE)
        {
            return None;
        }
        let batches: Vec<SyntaxNode> = if top.iter().any(|n| n.kind() == SyntaxKind::BATCH) {
            top
        } else {
            vec![a.root.clone()]
        };
        for b in batches {
            let nodes = cst::structural_children(&b);
            let block = nodes.iter().any(tsql_block);
            push(nodes, block);
        }
        return Some(out);
    }
    for n in top.into_iter().flat_map(|n| {
        if n.kind() == SyntaxKind::BATCH {
            cst::structural_children(&n)
        } else {
            vec![n]
        }
    }) {
        match n.kind() {
            SyntaxKind::ROUTINE if d == Dialect::Postgres => {
                let words: Vec<String> = cst::header_tokens(&n)
                    .iter()
                    .map(|t| t.text().to_ascii_uppercase())
                    .filter(|w| w != ";")
                    .collect();
                let plpgsql = match words.as_slice() {
                    [w] => w == "DO",
                    [w, l, x] | [w, x, l] if l == "LANGUAGE" => w == "DO" && x == "PLPGSQL",
                    _ => false,
                };
                if !plpgsql {
                    return None;
                }
                let body: Vec<SyntaxNode> = n
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::ROUTINE_BODY)
                    .collect();
                if body.len() != 1 {
                    return None;
                }
                push(body, true);
            }
            SyntaxKind::ROUTINE => return None,
            SyntaxKind::BLOCK if d == Dialect::Db2i => push(vec![n], true),
            _ => push(vec![n], false),
        }
    }
    Some(out)
}

/// Whether a T-SQL statement needs a block to run elsewhere: control flow,
/// a declaration, a message or a variable (not `@@` functions, not
/// `EXEC p @name = …` parameter names).
fn tsql_block(n: &SyntaxNode) -> bool {
    if matches!(
        n.kind(),
        SyntaxKind::IF_STMT | SyntaxKind::LOOP_STMT | SyntaxKind::TRY_CATCH | SyntaxKind::BLOCK
    ) {
        return true;
    }
    let ts = tokens(n);
    let first = ts.first().map(|t| t.up.as_str()).unwrap_or("");
    if matches!(
        first,
        "DECLARE" | "PRINT" | "RETURN" | "THROW" | "BREAK" | "CONTINUE"
    ) {
        return true;
    }
    let exec = matches!(first, "EXEC" | "EXECUTE");
    ts.iter().enumerate().any(|(i, t)| {
        t.kind == K::Var
            && !t.text.starts_with("@@")
            && !(exec && ts.get(i + 1).is_some_and(|x| x.text == "="))
    })
}

/// Lowers one segment of a script.
pub fn lower_segment(d: Dialect, seg: &Segment) -> Unit {
    let mut u = empty(d);
    u.in_block = seg.block && d != Dialect::Tsql;
    let routines: Vec<SyntaxNode> = seg
        .nodes
        .iter()
        .flat_map(|n| n.descendants())
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .collect();
    if let [r] = routines.as_slice() {
        u.routine = cst::routine_name(r);
    }
    for n in &seg.nodes {
        unit(n, &mut u);
    }
    place_txn(&mut u);
    u
}

/// A trigger body lowered for one event: a T-SQL trigger's `ROUTINE_BODY`,
/// or the body of the PostgreSQL function its trigger for that event runs.
/// Both run inside the firing statement and stop at the first error (a T-SQL
/// trigger as with XACT_ABORT ON). `RETURN NULL` (PostgreSQL ignores an AFTER
/// statement trigger's result) is a plain `RETURN`, and the closing `RETURN`
/// is left out: T-SQL falls off the end, PostgreSQL must return (falling off
/// is an error, reported as not covered).
pub fn lower_trigger(d: Dialect, body: &SyntaxNode, ctx: TrigCtx) -> Unit {
    let mut u = empty(d);
    u.kind = UnitKind::Trigger;
    u.in_block = true;
    u.stops_on_error = true;
    u.trigger = Some(ctx);
    walk(body, &mut u);
    for s in &mut u.stmts {
        if matches!(s.ast, Ok(Stmt::Return(Some(super::ast::Expr::Null)))) {
            s.ast = Ok(Stmt::Return(None));
        }
    }
    let last = last_leaf(body);
    let returns_last = |u: &Unit| {
        u.stmts.last().is_some_and(|s| {
            matches!(s.ast, Ok(Stmt::Return(None))) && last.as_ref().is_some_and(|l| *l == s.node)
        })
    };
    if returns_last(&u) {
        u.stmts.pop();
    } else if d == Dialect::Postgres {
        u.uncovered.push((
            body.clone(),
            "a trigger function that does not end with RETURN NULL (falling off its end is an error)".into(),
        ));
    }
    place_txn(&mut u);
    u
}

/// The last statement-level node of a body, through its blocks.
fn last_leaf(n: &SyntaxNode) -> Option<SyntaxNode> {
    let kids = cst::structural_children(n);
    let last = kids.last()?.clone();
    match last.kind() {
        SyntaxKind::BLOCK | SyntaxKind::ROUTINE_BODY => last_leaf(&last),
        _ => Some(last),
    }
}

fn place_txn(u: &mut Unit) {
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
        decl: Some(ty),
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
    let container = matches!(
        c.kind(),
        SyntaxKind::SOURCE_FILE
            | SyntaxKind::BATCH
            | SyntaxKind::ROUTINE_BODY
            | SyntaxKind::BLOCK
            | SyntaxKind::DECLARE_SECTION
    );
    if let Some(t) = u.trigger.as_mut().filter(|t| !container && !t.started) {
        t.started = true;
        if let Some(rowcount) = (c.kind() == SyntaxKind::IF_STMT)
            .then(|| guard(c, d, t.rows))
            .flatten()
        {
            t.guard = true;
            t.rowcount = rowcount;
            return;
        }
    }
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
    if let Some(ast) = message(c, d) {
        u.stmts.push(LStmt {
            node: c.clone(),
            toks: tokens(c),
            ast,
            from_declare: false,
        });
        return;
    }
    if table_var_decl(c, d) {
        u.table_vars.push((c.clone(), u.stmts.len()));
        return;
    }
    if temp_drop(c, d) {
        u.temp_drops.push((c.clone(), u.stmts.len()));
        return;
    }
    match kind {
        // Calls and comments: compared call against call (`call.rs`).
        _ if is_call(c, d) => {
            let at = CallAt {
                outside: None,
                after: u.stmts.len(),
            };
            u.calls.push((c.clone(), at));
        }
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
                    decl: ty.clone(),
                    node: c.clone(),
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
                "SET NOCOUNT compared as rows-affected counts by the observer profile, not as a statement",
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

/// The empty-image guard of a trigger body: T-SQL `IF @@ROWCOUNT = 0
/// RETURN` (or `< 1`, or `BEGIN RETURN END`), PostgreSQL `IF NOT EXISTS
/// (SELECT 1 FROM rows) THEN RETURN [NULL]; END IF;`.
/// Some(true) when it reads `@@ROWCOUNT`.
fn guard(c: &SyntaxNode, d: Dialect, rows: &str) -> Option<bool> {
    let words: Vec<String> = tokens(c)
        .iter()
        .filter(|t| !matches!(t.kind, K::LParen | K::RParen | K::Semi))
        .map(|t| match t.kind {
            K::Word | K::Quoted => cst::unquote(&t.text).to_uppercase(),
            _ => t.text.to_uppercase(),
        })
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    let rows = rows.to_uppercase();
    let ret = |rest: &[&str]| matches!(rest, ["RETURN"] | ["BEGIN", "RETURN", "END"]);
    let ok = match (d, w.as_slice()) {
        (Dialect::Tsql, ["IF", "@@ROWCOUNT", op, n, rest @ ..]) => {
            return (matches!((*op, *n), ("=", "0") | ("<", "1")) && ret(rest)).then_some(true);
        }
        (Dialect::Tsql, ["IF", "NOT", "EXISTS", "SELECT", "1" | "*", "FROM", t, rest @ ..]) => {
            *t == rows && ret(rest)
        }
        (
            Dialect::Postgres,
            ["IF", "NOT", "EXISTS", "SELECT", "1" | "*", "FROM", t, "THEN", "RETURN", rest @ ..],
        ) => *t == rows && matches!(rest, ["END", "IF"] | ["NULL", "END", "IF"]),
        _ => false,
    };
    ok.then_some(false)
}

/// A T-SQL table variable's `DECLARE @t [AS] TABLE`, or a temporary table
/// a converted one may have become: PostgreSQL `CREATE TEMP[ORARY] TABLE`,
/// Db2 for i `DECLARE GLOBAL TEMPORARY TABLE`.
fn table_var_decl(c: &SyntaxNode, d: Dialect) -> bool {
    let words = cst::first_words(c, 4);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match d {
        Dialect::Tsql => {
            w(0) == "DECLARE"
                && w(1).starts_with('@')
                && (w(2) == "TABLE" || (w(2) == "AS" && w(3) == "TABLE"))
        }
        Dialect::Postgres => {
            w(0) == "CREATE" && matches!(w(1), "TEMP" | "TEMPORARY") && w(2) == "TABLE"
        }
        Dialect::Db2i => {
            w(0) == "DECLARE" && w(1) == "GLOBAL" && w(2) == "TEMPORARY" && w(3) == "TABLE"
        }
    }
}

/// PostgreSQL `DROP TABLE IF EXISTS`.
fn temp_drop(c: &SyntaxNode, d: Dialect) -> bool {
    d == Dialect::Postgres && cst::first_words(c, 4) == ["DROP", "TABLE", "IF", "EXISTS"]
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

/// A procedure call or an object comment: T-SQL `EXEC name …` (dynamic
/// SQL aside), PostgreSQL and Db2 for i `CALL` and `COMMENT ON`.
fn is_call(c: &SyntaxNode, d: Dialect) -> bool {
    let words = cst::first_words(c, 2);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match stmt_kind(c, d) {
        StmtKind::Call => match d {
            Dialect::Tsql => {
                matches!(w(0), "EXEC" | "EXECUTE")
                    && tokens(c)
                        .get(1)
                        .is_some_and(|t| matches!(t.kind, K::Word | K::Quoted))
            }
            _ => w(0) == "CALL",
        },
        StmtKind::Ddl => d != Dialect::Tsql && w(0) == "COMMENT" && w(1) == "ON",
        _ => false,
    }
}

/// An object comment: T-SQL `EXEC sp_addextendedproperty`, `COMMENT ON`.
pub fn is_comment(c: &SyntaxNode, d: Dialect) -> bool {
    if !is_call(c, d) {
        return false;
    }
    match d {
        Dialect::Tsql => tokens(c).iter().take(6).any(|t| {
            matches!(t.kind, K::Word | K::Quoted)
                && cst::unquote(&t.text).eq_ignore_ascii_case("sp_addextendedproperty")
        }),
        _ => cst::first_words(c, 1)
            .first()
            .is_some_and(|w| w == "COMMENT"),
    }
}

/// A top-level `CREATE … INDEX`, `CREATE SEQUENCE` or `CREATE SCHEMA`.
fn is_create_ddl(c: &SyntaxNode, d: Dialect) -> bool {
    stmt_kind(c, d) == StmtKind::Ddl
        && super::ddl::is_ddl(&cst::first_words(c, 7))
        && c.parent()
            .is_some_and(|p| matches!(p.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH))
}

/// T-SQL `PRINT e`; PostgreSQL `RAISE NOTICE '%', e` or `RAISE NOTICE 'text'`
/// (no placeholder). `None` for any other statement; an error for a message
/// whose form is not read.
fn message(c: &SyntaxNode, d: Dialect) -> Option<Result<Stmt, String>> {
    let toks: Vec<Tok> = tokens(c)
        .into_iter()
        .filter(|t| t.kind != K::Semi)
        .collect();
    let rest = match (d, toks.first().map(|t| t.up.as_str())) {
        (Dialect::Tsql, Some("PRINT")) => &toks[1..],
        (Dialect::Postgres, Some("RAISE")) if toks.get(1).is_some_and(|t| t.up == "NOTICE") => {
            &toks[2..]
        }
        _ => return None,
    };
    let expr = |ts: &[Tok]| -> Result<super::ast::Expr, String> {
        let mut p = Parser::new(ts, d);
        let e = p.expr()?;
        if p.at_end() {
            Ok(e)
        } else {
            Err("trailing tokens after the message".into())
        }
    };
    Some((|| {
        if d == Dialect::Tsql {
            return Ok(Stmt::Message(expr(rest)?));
        }
        let parts = split_commas(rest);
        let fmt = match parts.first().map(|p| expr(p)) {
            Some(Ok(super::ast::Expr::Str(f))) => f,
            _ => return Err("RAISE NOTICE without a format string".into()),
        };
        match parts.as_slice() {
            [_] if !fmt.contains('%') => Ok(Stmt::Message(super::ast::Expr::Str(fmt))),
            [_, arg] if fmt == "%" => Ok(Stmt::Message(expr(arg)?)),
            _ => Err("a RAISE NOTICE format other than '%' with one value".into()),
        }
    })())
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
        decl: Some(ty.clone()),
        node: s.clone(),
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
