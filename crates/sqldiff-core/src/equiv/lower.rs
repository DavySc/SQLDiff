//! Routine shape: kind, signature, local declarations and the ordered list of
//! data statements, plus everything the statement-level comparison does not
//! cover (control flow other than the recognised transaction protocols).

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{parse_stmt, parse_stmt_output, tokens, Expr, Output, Parser, Stmt, Tok, Type, K};
use super::cursor::{CursorLoop, CursorOp};
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
    /// A message or error built from a format (`ast` then says why it is
    /// not a plain statement): compared as its text (`Canon::message`).
    pub msg: Option<Msg>,
    /// The rows the statement returns (T-SQL OUTPUT, PostgreSQL RETURNING
    /// that is not INTO variables), which `ast` leaves out.
    pub output: Option<Output>,
    /// A statement of a cursor loop (`cursor.rs`; `ast` then says why it is
    /// not a plain statement): compared as `Canon::cursor_op` says.
    pub cursor: Option<CursorOp>,
}

/// A message or raised error whose text a format builds: T-SQL
/// `RAISERROR (format, severity, state [, args])`, PL/pgSQL
/// `RAISE NOTICE | EXCEPTION 'format' [, args]`.
#[derive(Debug, Clone)]
pub struct Msg {
    /// Raised as an error (T-SQL severity 11 to 18, `RAISE EXCEPTION`);
    /// otherwise an informational message (severity 0 to 10, `RAISE NOTICE`).
    pub error: bool,
    pub parts: Vec<MsgPart>,
    /// T-SQL `RAISERROR`: the text is cut to 2,047 characters, and an empty
    /// text reaches the client as one space.
    pub tsql: bool,
    /// The format is a variable (`RAISERROR (@v, …)`): a `%` in its value
    /// is a format specification.
    pub format_var: bool,
}

#[derive(Debug, Clone)]
pub enum MsgPart {
    Text(String),
    /// A value shown as text, and as `null` when it is NULL.
    Arg {
        value: super::ast::Expr,
        null: String,
        spec: Spec,
    },
}

/// What a format argument must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spec {
    /// T-SQL `%s`, PostgreSQL `%` of a string: a character string.
    Str,
    /// T-SQL `%d` / `%i`, PostgreSQL `%` of `CAST(x AS text)`: an integer.
    Int,
}

/// A control-flow step among the data statements: IF and WHILE are lowered
/// to a flat sequence of markers around the statements they control, the
/// same sequence in every dialect. `IF c THEN x ELSIF d THEN y ELSE z END
/// IF` and T-SQL's `IF c x ELSE IF d y ELSE z` both become `if(c) x else
/// if(d) y else z end end`; `WHILE c` becomes `while(c) … end`. Comparing
/// the sequences marker against marker, with every statement in the same
/// branch as its counterpart (`equiv::pair_items`), compares the control
/// flow.
#[derive(Debug, Clone)]
pub enum CtlKind {
    /// IF, ELSIF / ELSEIF, or the IF of T-SQL's ELSE IF: its condition. The
    /// branch is taken when the condition is true (not false, not NULL) in
    /// all three dialects.
    If(Result<Expr, String>),
    /// WHILE: the body runs while the condition is true.
    While(Result<Expr, String>),
    /// The WHILE of a cursor loop (`cursor.rs`): T-SQL `@@FETCH_STATUS = 0`,
    /// PostgreSQL `FOUND`, both right after a FETCH of the cursor (its key).
    FetchWhile(String),
    /// The start of the branch taken when the conditions before are not true.
    Else,
    /// The end of an IF (one per IF and per ELSIF) or of a loop.
    End,
    /// T-SQL BREAK, PostgreSQL EXIT, Db2 for i LEAVE of the innermost loop.
    Break,
    /// T-SQL and PostgreSQL CONTINUE, Db2 for i ITERATE of the innermost loop.
    Continue,
    /// A procedure call inside an IF or a loop. The call itself is compared
    /// call against call (`call.rs`); the marker keeps its place in the
    /// branch.
    Call,
}

#[derive(Debug, Clone)]
pub struct Ctl {
    pub kind: CtlKind,
    /// The condition for IF and WHILE (the IF or loop for END and ELSE, the
    /// statement for the others), for locations.
    pub node: SyntaxNode,
    /// Where a conversion's trace finds it: the condition of the IF or loop
    /// it belongs to (an ELSE and END close the IF of the condition before
    /// them, so a T-SQL `ELSE IF` and the `ELSIF` it became sit in the same
    /// trace unit), the statement for the others.
    pub anchor: SyntaxNode,
    /// The number of data statements before it.
    pub after: usize,
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
    /// Dynamic SQL: T-SQL `EXEC (…)` / `sp_executesql`, PL/pgSQL `EXECUTE`,
    /// compared by the statement each runs (`dynamic.rs`), placed like calls.
    pub dynamic: Vec<(SyntaxNode, CallAt)>,
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
    /// PostgreSQL `DROP TABLE IF EXISTS pg_temp.t`, placed like `table_vars`:
    /// the drop before a converted table variable's `CREATE TEMP TABLE`.
    pub temp_drops: Vec<(SyntaxNode, usize)>,
    /// IF and WHILE markers, in order among `stmts` (`CtlKind`).
    pub ctl: Vec<Ctl>,
    /// Cursor loops of the shape `cursor.rs` compares.
    pub cursor_loops: Vec<CursorLoop>,
    /// PostgreSQL `name refcursor` declarations: (name, declaration).
    pub cursor_vars: Vec<(String, SyntaxNode)>,
    /// Statements compared as part of another (a T-SQL cursor's DECLARE
    /// with its OPEN, its DEALLOCATE with its CLOSE): (compared, part).
    /// What leaves the compared one undecided leaves the part so too.
    pub linked: Vec<(SyntaxNode, SyntaxNode)>,
    /// How many IFs and loops the statement being lowered sits in.
    depth: usize,
    /// The labels of the loops around it, innermost last (upper case).
    loops: Vec<Option<String>>,
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
        dynamic: Vec::new(),
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
        ctl: Vec::new(),
        cursor_loops: Vec::new(),
        cursor_vars: Vec::new(),
        linked: Vec::new(),
        depth: 0,
        loops: Vec::new(),
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
    // PostgreSQL DO blocks beside one routine are statements outside it, as
    // a Db2 for i compound statement is (a converted script's setup batch).
    let named: Vec<SyntaxNode> = routines
        .iter()
        .filter(|r| !(d == Dialect::Postgres && is_do(r)))
        .cloned()
        .collect();
    let routines = if named.len() == 1 { named } else { routines };
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
                    let after =
                        n.text_range().start() >= r.text_range().end() && !index_beside(r, &n, d);
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
        [..] => {
            // Each routine, so that the statements inside every one of them
            // (not only the first) are reported as not compared.
            for r in &routines {
                u.uncovered
                    .push((r.clone(), "several routines in one artifact".into()));
            }
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
                    .filter(|r| {
                        r.text_range().end() <= n.text_range().start() && !index_beside(r, &n, d)
                    })
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
    cursors_done(&mut u);
    place_txn(&mut u);
    u
}

/// A PL/pgSQL block that assigns variables from each row of a query in
/// turn, as the conversion writes T-SQL `SELECT @v = e FROM …`:
///
/// ```text
/// DECLARE r record;
/// BEGIN
///   FOR r IN SELECT e1 AS c1, … FROM … LOOP
///     v1 := r.c1; …
///   END LOOP;
/// END;
/// ```
///
/// Lowered as that SELECT: after the loop each variable holds the last
/// row's value, and with no row it keeps its own; each value goes through
/// the variable's assignment conversion. Only that exact shape: one record,
/// every column assigned once to a different variable, and neither the
/// record nor a variable read by the query (the loop reads the query once,
/// where T-SQL would see the values of earlier rows). The record is the
/// loop's own, so declaring it again each time the block runs changes
/// nothing, inside an IF or loop too.
fn assign_loop(c: &SyntaxNode, u: &mut Unit) -> bool {
    use SyntaxKind::{BRANCH, CONDITION, DECLARE_SECTION, LOOP_STMT, STATEMENT};
    let d = u.dialect;
    if d != Dialect::Postgres {
        return false;
    }
    let kids: Vec<SyntaxNode> = c.children().collect();
    let [decl, lp] = kids.as_slice() else {
        return false;
    };
    if decl.kind() != DECLARE_SECTION || lp.kind() != LOOP_STMT {
        return false;
    }
    // `DECLARE r record;`
    let decls: Vec<SyntaxNode> = decl.children().collect();
    let [rec] = decls.as_slice() else {
        return false;
    };
    let rt = tokens(rec);
    let r = match rt.as_slice() {
        [n, ty] if n.kind == K::Word && ty.up == "RECORD" => n.up.clone(),
        _ => return false,
    };
    // `FOR r IN <query> LOOP <assignments> END LOOP`
    let parts: Vec<SyntaxNode> = lp.children().collect();
    let [cond, body] = parts.as_slice() else {
        return false;
    };
    let first = lp
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT)
        .map(|t| t.text().to_ascii_uppercase());
    if first.as_deref() != Some("FOR") || cond.kind() != CONDITION || body.kind() != BRANCH {
        return false;
    }
    let ct = tokens(cond);
    let query = match ct.as_slice() {
        [v, kw, rest @ ..] if v.kind == K::Word && v.up == r && kw.up == "IN" => rest,
        _ => return false,
    };
    let Ok(Stmt::Select(mut sel)) = parse_stmt(query, d) else {
        return false;
    };
    // `v := r.c`, each variable and each column once.
    let mut set: Vec<(String, String)> = Vec::new();
    for st in body.children() {
        if st.kind() != STATEMENT {
            return false;
        }
        match tokens(&st).as_slice() {
            [v, op, rec, dot, col]
                if v.kind == K::Word
                    && op.text == ":="
                    && rec.kind == K::Word
                    && rec.up == r
                    && dot.kind == K::Dot
                    && col.kind == K::Word =>
            {
                if set.iter().any(|(x, y)| *x == v.up || *y == col.up) {
                    return false;
                }
                set.push((v.up.clone(), col.up.clone()));
            }
            _ => return false,
        }
    }
    if set.is_empty() || set.len() != sel.items.len() || !sel.into.is_empty() {
        return false;
    }
    // Neither the record nor an assigned variable in the query.
    if query
        .iter()
        .any(|t| t.kind == K::Word && (t.up == r || set.iter().any(|(v, _)| *v == t.up)))
    {
        return false;
    }
    for it in &mut sel.items {
        let Some(alias) = it.alias.as_ref().map(|x| x.to_ascii_uppercase()) else {
            return false;
        };
        let Some((v, _)) = set.iter().find(|(_, col)| *col == alias) else {
            return false;
        };
        if it.assign.is_some() {
            return false;
        }
        it.assign = Some(v.to_lowercase());
        it.alias = None;
    }
    u.stmts.push(LStmt {
        node: c.clone(),
        toks: tokens(c),
        ast: Ok(Stmt::Select(sel)),
        from_declare: false,
        msg: None,
        output: None,
        cursor: None,
    });
    true
}

/// Whether a PostgreSQL routine node is a DO block.
pub(super) fn is_do(r: &SyntaxNode) -> bool {
    cst::header_tokens(r)
        .first()
        .is_some_and(|t| t.text().eq_ignore_ascii_case("DO"))
}

/// Whether `n` creates an index whose place before or after routine `r`
/// does not matter: `r` is a trigger (T-SQL, Db2 for i) or a PostgreSQL
/// trigger function, and it never names the index. Creating
/// either one runs neither: a trigger body binds tables and columns (Db2 for
/// i when it is created, T-SQL and PL/pgSQL when it runs), never an index it
/// does not name, and an index changes no row a trigger sees. So a
/// conversion that adds trigger functions before a table's indexes (temporal
/// tables, Db2 for i sequence defaults) places them as the original did.
fn index_beside(r: &SyntaxNode, n: &SyntaxNode, d: Dialect) -> bool {
    let Ok(super::ddl::Ddl::Index(ix)) = super::ddl::parse(&tokens(n), d) else {
        return false;
    };
    let header: Vec<Tok> = cst::header_tokens(r).iter().map(Tok::from_syntax).collect();
    let trigger = header.iter().take(4).any(|t| t.up == "TRIGGER")
        || header
            .windows(2)
            .any(|w| w[0].up == "RETURNS" && w[1].up == "TRIGGER");
    // A name in a string counts too (dynamic SQL, an error message).
    let name = ix.name.text.to_lowercase();
    trigger
        && !tokens(r).iter().any(|t| match t.kind {
            K::Word | K::Quoted => cst::unquote(&t.text).to_lowercase() == name,
            K::Str => t.text.to_lowercase().contains(&name),
            _ => false,
        })
}

/// One part of a script that runs as a whole: a T-SQL batch that needs
/// variables or control flow, a PostgreSQL DO block, a Db2 for i compound
/// statement (`block`), a routine of a script that creates several
/// (`routine`, its one node), or the plain statements between them.
#[derive(Debug, Clone)]
pub struct Segment {
    pub nodes: Vec<SyntaxNode>,
    pub block: bool,
    pub routine: bool,
}

/// A script's segments in order; `None` when the artifact holds one routine
/// (other than a DO block), a routine that is not a statement of its own
/// (a T-SQL batch holds nothing else), or a PostgreSQL DO block in another
/// language. Consecutive plain T-SQL batches form one segment, as do
/// consecutive plain statements on the other side: a conversion turns a
/// T-SQL batch with variables or control flow into one block and leaves the
/// others' statements as they are. A script that creates several routines
/// gets one segment for each, so that each routine meets its own conversion
/// (`script::segment_pairs` pairs them by name).
pub fn segments(a: &Analyzed) -> Option<Vec<Segment>> {
    let d = a.dialect();
    let mut out: Vec<Segment> = Vec::new();
    let mut push = |nodes: Vec<SyntaxNode>, block: bool, routine: bool| {
        if nodes.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if !block && !routine && !last.block && !last.routine => {
                last.nodes.extend(nodes)
            }
            _ => out.push(Segment {
                nodes,
                block,
                routine,
            }),
        }
    };
    let routines: Vec<SyntaxNode> = a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE && !(d == Dialect::Postgres && is_do(n)))
        .collect();
    let several = routines.len() >= 2;
    let top: Vec<SyntaxNode> = cst::structural_children(&a.root);
    if d == Dialect::Tsql {
        if !routines.is_empty() && !several {
            return None;
        }
        let batches: Vec<SyntaxNode> = if top.iter().any(|n| n.kind() == SyntaxKind::BATCH) {
            top
        } else {
            vec![a.root.clone()]
        };
        let mut seen = 0;
        for b in batches {
            let nodes = cst::structural_children(&b);
            if nodes.iter().any(|n| n.kind() == SyntaxKind::ROUTINE) {
                if nodes.len() != 1 {
                    return None;
                }
                seen += 1;
                push(nodes, false, true);
                continue;
            }
            let block = nodes.iter().any(tsql_block);
            push(nodes, block, false);
        }
        // Every routine is a batch of its own (none sits inside another
        // statement).
        return (seen == routines.len()).then_some(out);
    }
    let mut seen = 0;
    for n in top.into_iter().flat_map(|n| {
        if n.kind() == SyntaxKind::BATCH {
            cst::structural_children(&n)
        } else {
            vec![n]
        }
    }) {
        match n.kind() {
            SyntaxKind::ROUTINE if d == Dialect::Postgres && is_do(&n) => {
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
                push(body, true, false);
            }
            SyntaxKind::ROUTINE if several => {
                seen += 1;
                push(vec![n], false, true);
            }
            SyntaxKind::ROUTINE => return None,
            SyntaxKind::BLOCK if d == Dialect::Db2i => push(vec![n], true, false),
            _ => push(vec![n], false, false),
        }
    }
    (seen == routines.len()).then_some(out)
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
    if let (true, [r]) = (seg.routine, seg.nodes.as_slice()) {
        // As `lower` lowers the one routine of an artifact.
        u.in_block = true;
        header(r, &mut u);
        for c in r.children() {
            if c.kind() == SyntaxKind::ROUTINE_BODY {
                walk(&c, &mut u);
            }
        }
        super::options::body_checks(r, &mut u);
        place_txn(&mut u);
        return u;
    }
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
    cursors_done(&mut u);
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
    cursors_done(&mut u);
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
    // An IF or loop condition reads data too; an END marks where its
    // construct ends.
    let data: Vec<(usize, bool)> = u
        .stmts
        .iter()
        .map(|s| {
            (
                usize::from(s.node.text_range().start()),
                matches!(s.ast, Ok(Stmt::Return(_))),
            )
        })
        .chain(u.ctl.iter().map(|m| {
            let r = m.node.text_range();
            let at = match m.kind {
                CtlKind::End => r.end(),
                _ => r.start(),
            };
            (usize::from(at), false)
        }))
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
        SyntaxKind::BLOCK if assign_loop(c, u) => {}
        SyntaxKind::BLOCK => {
            let w = cst::first_words(c, 2);
            if w.get(1).is_some_and(|x| x == "ATOMIC") {
                if u.depth > 0 {
                    return inside_flow(c, u, "an atomic block");
                }
                u.txn_control = true;
            }
            walk(c, u)
        }
        // A nested block's declarations are set again each time the block
        // runs (PostgreSQL), where a T-SQL DECLARE inside a loop keeps the
        // value: not compared inside IFs and loops.
        SyntaxKind::DECLARE_SECTION if u.depth > 0 => inside_flow(c, u, "a declaration"),
        SyntaxKind::DECLARE_SECTION => {
            for s in cst::structural_children(c) {
                pg_declaration(&s, u);
            }
        }
        SyntaxKind::STATEMENT => statement(c, u),
        SyntaxKind::IF_STMT if u.trigger.is_none() => if_stmt(c, u),
        SyntaxKind::LOOP_STMT if u.trigger.is_none() => loop_stmt(c, u),
        // A CATCH or handler decides how the whole routine goes on after an
        // error (`stops_on_error`), which one inside an IF or loop does not.
        SyntaxKind::TRY_CATCH | SyntaxKind::HANDLER if u.depth > 0 => {
            inside_flow(c, u, "error handling")
        }
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

/// A construct that is compared at the top level but not inside an IF or a
/// loop.
fn inside_flow(c: &SyntaxNode, u: &mut Unit, what: &str) {
    u.uncovered
        .push((c.clone(), format!("{what} inside an IF or a loop")));
}

fn mark(u: &mut Unit, kind: CtlKind, node: &SyntaxNode) {
    mark_at(u, kind, node, node);
}

fn mark_at(u: &mut Unit, kind: CtlKind, node: &SyntaxNode, anchor: &SyntaxNode) {
    u.ctl.push(Ctl {
        kind,
        node: node.clone(),
        anchor: anchor.clone(),
        after: u.stmts.len(),
    });
}

/// The expression of a CONDITION node: the whole of its tokens.
fn condition(c: &SyntaxNode, d: Dialect) -> Result<Expr, String> {
    expression(&tokens(c), d)
}

fn expression(toks: &[Tok], d: Dialect) -> Result<Expr, String> {
    let mut p = Parser::new(toks, d);
    let e = p.expr()?;
    if !p.at_end() {
        return Err("unexpected tokens after the condition".into());
    }
    Ok(e)
}

/// IF … [ELSIF …] [ELSE …]: `if(c) …` per condition, `else` before every
/// branch but the first, and one `end` per condition (`CtlKind`). An IF of
/// another shape stays uncovered.
fn if_stmt(c: &SyntaxNode, u: &mut Unit) {
    use SyntaxKind::{BRANCH, CONDITION};
    let kids: Vec<SyntaxNode> = c.children().collect();
    // CONDITION BRANCH (CONDITION BRANCH)* [BRANCH], nothing else.
    let shape = {
        let mut i = 0;
        let mut conds = 0;
        while i + 1 < kids.len() && kids[i].kind() == CONDITION && kids[i + 1].kind() == BRANCH {
            i += 2;
            conds += 1;
        }
        if i < kids.len() && kids[i].kind() == BRANCH {
            i += 1;
        }
        conds > 0 && i == kids.len()
    };
    if !shape {
        u.uncovered.push((c.clone(), "an IF statement".into()));
        return;
    }
    // The conditions opened so far: an ELSE closes the branch of the last.
    let mut opened: Vec<&SyntaxNode> = Vec::new();
    let mut cond: Option<&SyntaxNode> = None;
    for k in &kids {
        if k.kind() == CONDITION {
            cond = Some(k);
            continue;
        }
        if let Some(&last) = opened.last() {
            mark_at(u, CtlKind::Else, k, last);
        }
        if let Some(cn) = cond.take() {
            mark(u, CtlKind::If(condition(cn, u.dialect)), cn);
            opened.push(cn);
        }
        u.depth += 1;
        walk(k, u);
        u.depth -= 1;
    }
    for cn in opened.into_iter().rev() {
        mark_at(u, CtlKind::End, c, cn);
    }
}

/// WHILE c (T-SQL `WHILE c stmt`, PostgreSQL `WHILE c LOOP … END LOOP`,
/// Db2 for i `[label:] WHILE c DO … END WHILE [label]`): `while(c) … end`.
/// Other loops (PostgreSQL LOOP, FOR, Db2 for i REPEAT, FOR) stay uncovered.
fn loop_stmt(c: &SyntaxNode, u: &mut Unit) {
    use SyntaxKind::{BRANCH, CONDITION, LABEL};
    let kids: Vec<SyntaxNode> = c.children().collect();
    let (label, rest) = match kids.split_first() {
        Some((l, rest)) if l.kind() == LABEL => {
            let name = tokens(l)
                .first()
                .filter(|t| t.kind == K::Word)
                .map(|t| t.up.clone());
            if name.is_none() {
                u.uncovered.push((c.clone(), "a loop".into()));
                return;
            }
            (name, rest)
        }
        _ => (None, &kids[..]),
    };
    let first_word = c
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT)
        .map(|t| t.text().to_ascii_uppercase());
    let [cond, body] = rest else {
        u.uncovered.push((c.clone(), "a loop".into()));
        return;
    };
    if first_word.as_deref() != Some("WHILE") || cond.kind() != CONDITION || body.kind() != BRANCH {
        u.uncovered.push((c.clone(), "a loop".into()));
        return;
    }
    let kind = match u.cursor_loops.iter().find(|l| l.lp == *c) {
        Some(l) => CtlKind::FetchWhile(l.key.clone()),
        None => CtlKind::While(condition(cond, u.dialect)),
    };
    mark(u, kind, cond);
    u.depth += 1;
    u.loops.push(label);
    walk(body, u);
    u.loops.pop();
    u.depth -= 1;
    mark_at(u, CtlKind::End, c, cond);
}

/// BREAK / CONTINUE of the innermost loop: T-SQL `BREAK`, `CONTINUE`;
/// PostgreSQL `EXIT`, `CONTINUE` without a label, optionally `WHEN c`
/// (`if(c) break end`); Db2 for i `LEAVE l`, `ITERATE l` where `l` labels the
/// innermost loop. Returns whether the statement was one (lowered, or
/// uncovered when it leaves anything but the innermost loop).
fn jump(c: &SyntaxNode, u: &mut Unit) -> bool {
    let d = u.dialect;
    let toks: Vec<Tok> = tokens(c)
        .into_iter()
        .filter(|t| t.kind != K::Semi)
        .collect();
    let Some(first) = toks.first().filter(|t| t.kind == K::Word) else {
        return false;
    };
    let kind = match (d, first.up.as_str()) {
        (Dialect::Tsql, "BREAK") | (Dialect::Postgres, "EXIT") | (Dialect::Db2i, "LEAVE") => {
            CtlKind::Break
        }
        (Dialect::Tsql | Dialect::Postgres, "CONTINUE") | (Dialect::Db2i, "ITERATE") => {
            CtlKind::Continue
        }
        _ => return false,
    };
    let rest = &toks[1..];
    let Some(innermost) = u.loops.last().cloned() else {
        u.uncovered
            .push((c.clone(), format!("`{}` outside a loop", first.up)));
        return true;
    };
    let when = match d {
        Dialect::Tsql if rest.is_empty() => None,
        Dialect::Db2i => match rest {
            [l] if l.kind == K::Word && innermost.as_deref() == Some(l.up.as_str()) => None,
            _ => {
                u.uncovered.push((
                    c.clone(),
                    format!("`{}` of another loop or block", first.up),
                ));
                return true;
            }
        },
        Dialect::Postgres => match rest {
            [] => None,
            [w, cond @ ..] if w.kind == K::Word && w.up == "WHEN" && !cond.is_empty() => {
                Some(expression(cond, d))
            }
            _ => {
                u.uncovered.push((
                    c.clone(),
                    format!("`{}` of another loop or block", first.up),
                ));
                return true;
            }
        },
        _ => {
            u.uncovered.push((c.clone(), format!("`{}`", first.up)));
            return true;
        }
    };
    match when {
        None => mark(u, kind, c),
        Some(cond) => {
            mark(u, CtlKind::If(cond), c);
            mark(u, kind, c);
            mark(u, CtlKind::End, c);
        }
    }
    true
}

fn statement(c: &SyntaxNode, u: &mut Unit) {
    let d = u.dialect;
    let kind = stmt_kind(c, d);
    let words = cst::first_words(c, 3);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    if jump(c, u) || cursor_stmt(c, u) {
        return;
    }
    let dynamic = match d {
        Dialect::Tsql => super::dynamic::is_tsql(c),
        _ => u.in_block && super::dynamic::pg_text(c).is_some(),
    };
    // Dynamic SQL is paired by its place among the data statements only,
    // which says nothing of the IF or loop around it.
    if dynamic && u.depth > 0 {
        return inside_flow(c, u, "dynamic SQL");
    }
    if u.depth > 0 {
        let what = match kind {
            // A T-SQL variable lives from its DECLARE to the end of the
            // batch whatever branch it sits in, and a DECLARE in a loop does
            // not reset it: only the initial value is a statement, an
            // assignment in its place. PostgreSQL and Db2 for i declare at
            // the top of a block, which a block inside a loop runs again.
            StmtKind::Declare if d != Dialect::Tsql => Some("a declaration"),
            StmtKind::Ddl => Some("a DDL statement"),
            StmtKind::TxnBegin | StmtKind::TxnCommit | StmtKind::TxnSetting => {
                Some("transaction control")
            }
            StmtKind::SessionSetting => Some("a session setting"),
            _ => None,
        };
        if let Some(what) = what.filter(|_| !is_call(c, d)) {
            return inside_flow(c, u, what);
        }
    }
    if d == Dialect::Tsql && add_filegroup(c) {
        note(
            u,
            "ALTER DATABASE … ADD FILEGROUP compared as storage: where data is stored is not seen in results",
        );
        return;
    }
    if let Some((ast, msg)) = message(c, d) {
        u.stmts.push(LStmt {
            node: c.clone(),
            toks: tokens(c),
            ast,
            from_declare: false,
            msg,
            output: None,
            cursor: None,
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
        // Dynamic SQL: compared by the statement it runs (`dynamic.rs`).
        _ if dynamic => {
            let at = CallAt {
                outside: None,
                after: u.stmts.len(),
            };
            u.dynamic.push((c.clone(), at));
        }
        // Calls and comments: compared call against call (`call.rs`).
        _ if is_call(c, d) => {
            let at = CallAt {
                outside: None,
                after: u.stmts.len(),
            };
            u.calls.push((c.clone(), at));
            if u.depth > 0 {
                mark(u, CtlKind::Call, c);
            }
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
            let (mut ast, output) = match parse_stmt_output(&toks, d) {
                Ok((s, o)) => (Ok(s), o),
                Err(e) => (Err(e), None),
            };
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
                msg: None,
                output,
                cursor: None,
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
                        msg: None,
                        output: None,
                        cursor: None,
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

/// T-SQL `ALTER DATABASE name ADD FILEGROUP fg [CONTAINS
/// MEMORY_OPTIMIZED_DATA | FILESTREAM]`: a place to store data, nothing a
/// statement's result shows.
fn add_filegroup(c: &SyntaxNode) -> bool {
    let w: Vec<String> = tokens(c).iter().map(|t| t.up.clone()).collect();
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    matches!(
        w.as_slice(),
        ["ALTER", "DATABASE", _, "ADD", "FILEGROUP", _]
            | [
                "ALTER",
                "DATABASE",
                _,
                "ADD",
                "FILEGROUP",
                _,
                "CONTAINS",
                "MEMORY_OPTIMIZED_DATA" | "FILESTREAM"
            ]
    )
}

/// A statement of a cursor loop (`cursor.rs`): the T-SQL DECLARE or
/// PostgreSQL OPEN that starts one (checked there; refused with the reason
/// when the loop is not of the shape), and the OPEN, FETCH, CLOSE and
/// DEALLOCATE of a loop found so. Returns whether `c` was one.
fn cursor_stmt(c: &SyntaxNode, u: &mut Unit) -> bool {
    let d = u.dialect;
    if d == Dialect::Db2i {
        return false;
    }
    if let Some(found) = super::cursor::start(c, u) {
        match found {
            Ok(l) => {
                if let Some(decl) = &l.decl {
                    u.linked.push((l.open.clone(), decl.clone()));
                }
                if let Some(x) = &l.dealloc {
                    u.linked.push((l.close.clone(), x.clone()));
                }
                let opens = l.open == *c;
                u.cursor_loops.push(l);
                // A T-SQL DECLARE is compared with its OPEN.
                if !opens {
                    return true;
                }
            }
            Err(why) => {
                u.uncovered.push((c.clone(), format!("a cursor ({why})")));
                return true;
            }
        }
    }
    let Some(l) = u.cursor_loops.iter().find(|l| {
        l.open == *c || l.fetches.contains(c) || l.close == *c || l.dealloc.as_ref() == Some(c)
    }) else {
        return false;
    };
    let key = l.key.clone();
    let op = if l.open == *c {
        CursorOp::Open {
            key,
            query: l.query.clone(),
            tsql: (d == Dialect::Tsql).then_some((l.stat, l.local)),
        }
    } else if l.fetches.contains(c) {
        let targets = l
            .targets
            .iter()
            .map(|t| match d {
                Dialect::Tsql => Expr::Var(t.clone()),
                _ => Expr::Name(vec![t.clone()]),
            })
            .collect();
        CursorOp::Fetch { key, targets }
    } else if l.close == *c {
        CursorOp::Close { key }
    } else {
        // DEALLOCATE: compared with the CLOSE (`linked`).
        return true;
    };
    u.stmts.push(LStmt {
        node: c.clone(),
        toks: tokens(c),
        ast: Err("a cursor statement".into()),
        from_declare: false,
        msg: None,
        output: None,
        cursor: Some(op),
    });
    true
}

/// PostgreSQL: each `refcursor` variable is the cursor of one loop found,
/// and each loop's cursor is such a variable; anything else is not compared.
fn cursors_done(u: &mut Unit) {
    if u.dialect != Dialect::Postgres {
        return;
    }
    let names: Vec<String> = u
        .cursor_loops
        .iter()
        .map(|l| super::cursor::key(Dialect::Tsql, &l.name))
        .collect();
    for (name, node) in u.cursor_vars.clone() {
        let k = super::cursor::key(Dialect::Tsql, &name);
        if names.iter().filter(|x| **x == k).count() != 1 {
            u.uncovered.push((
                node,
                "a cursor variable not used by one standard cursor loop".into(),
            ));
        }
    }
    for l in u.cursor_loops.clone() {
        let k = super::cursor::key(Dialect::Tsql, &l.name);
        if !u
            .cursor_vars
            .iter()
            .any(|(n, _)| super::cursor::key(Dialect::Tsql, n) == k)
        {
            u.uncovered.push((
                l.open.clone(),
                "a cursor that is not a refcursor variable of the routine".into(),
            ));
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

/// PostgreSQL `DROP TABLE IF EXISTS pg_temp.…`. Other drops are compared
/// statement against statement (`ddl.rs`).
fn temp_drop(c: &SyntaxNode, d: Dialect) -> bool {
    d == Dialect::Postgres && cst::first_words(c, 5) == ["DROP", "TABLE", "IF", "EXISTS", "PG_TEMP"]
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
        StmtKind::Ddl => {
            (d != Dialect::Tsql && w(0) == "COMMENT" && w(1) == "ON")
                || super::call::is_switch(&tokens(c), d)
        }
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

/// What a message statement lowers to: a plain statement, or a message
/// built from a format.
type Lowered = (Result<Stmt, String>, Option<Msg>);

/// T-SQL `PRINT e` and `RAISERROR (format, severity, state [, args])`;
/// PostgreSQL `RAISE NOTICE '%', e`, `RAISE NOTICE 'text'` and
/// `RAISE NOTICE | EXCEPTION 'format', args`. `None` for any other
/// statement; an error for a message whose form is not read.
fn message(c: &SyntaxNode, d: Dialect) -> Option<Lowered> {
    use super::ast::Expr;
    let toks: Vec<Tok> = tokens(c)
        .into_iter()
        .filter(|t| t.kind != K::Semi)
        .collect();
    let level = toks.get(1).map(|t| t.up.as_str());
    let rest = match (d, toks.first().map(|t| t.up.as_str())) {
        (Dialect::Tsql, Some("PRINT")) => &toks[1..],
        (Dialect::Tsql, Some("RAISERROR")) => {
            let r = raiserror(&toks[1..]);
            let why = "a RAISERROR compared as its message".to_string();
            return Some(match r {
                Ok(m) => (Err(why), Some(m)),
                Err(e) => (Err(e), None),
            });
        }
        (Dialect::Postgres, Some("RAISE")) if matches!(level, Some("NOTICE" | "EXCEPTION")) => {
            &toks[2..]
        }
        _ => return None,
    };
    let expr = |ts: &[Tok]| -> Result<Expr, String> {
        let mut p = Parser::new(ts, d);
        let e = p.expr()?;
        if p.at_end() {
            Ok(e)
        } else {
            Err("trailing tokens after the message".into())
        }
    };
    if d == Dialect::Tsql {
        return Some((expr(rest).map(Stmt::Message), None));
    }
    let error = level == Some("EXCEPTION");
    // `RAISE EXCEPTION USING MESSAGE = …` (THROW's conversion) and other
    // forms without a format string stay uncovered, as before.
    if !rest.first().is_some_and(|t| t.kind == K::Str) {
        return if error {
            None
        } else {
            Some((Err("RAISE NOTICE without a format string".into()), None))
        };
    }
    // `USING ERRCODE = 'P0001'` (RAISE EXCEPTION's own default) is the one
    // option read: the rest change what the caller sees.
    let using = rest
        .iter()
        .position(|t| t.kind == K::Word && t.up == "USING")
        .unwrap_or(rest.len());
    let opts: Vec<&str> = rest[using..]
        .iter()
        .map(|t| {
            if t.kind == K::Word {
                t.up.as_str()
            } else {
                t.text.as_str()
            }
        })
        .collect();
    let errcode = matches!(opts.as_slice(), ["USING", "ERRCODE", "=", "'P0001'"]);
    // A RAISE EXCEPTION without it is no RAISERROR's conversion (the
    // converter's stand-in for a statement it did not convert is one): it
    // stays uncovered, as before.
    if error && !errcode {
        return None;
    }
    let opts_ok = opts.is_empty() || errcode;
    let parts = split_commas(&rest[..using]);
    let fmt = match parts.first().map(|p| expr(p)) {
        Some(Ok(Expr::Str(f))) => f,
        _ => return Some((Err("RAISE without a format string".into()), None)),
    };
    let lowered =
        (|| -> Result<Lowered, String> {
            if !opts_ok {
                return Err("RAISE with USING options other than ERRCODE = 'P0001'".into());
            }
            let args = parts[1..]
                .iter()
                .map(|p| expr(p))
                .collect::<Result<Vec<_>, _>>()?;
            if !error {
                match (args.as_slice(), fmt.as_str()) {
                    ([], f) if !f.contains('%') => {
                        return Ok((Ok(Stmt::Message(Expr::Str(fmt.clone()))), None))
                    }
                    ([arg], "%") if pg_shown(arg).is_none() => {
                        return Ok((Ok(Stmt::Message(arg.clone())), None))
                    }
                    _ => {}
                }
            }
            let mut args = args.into_iter();
            let mut parts = Vec::new();
            let mut text = String::new();
            let mut cs = fmt.chars().peekable();
            while let Some(ch) = cs.next() {
                if ch != '%' {
                    text.push(ch);
                    continue;
                }
                if cs.peek() == Some(&'%') {
                    cs.next();
                    text.push('%');
                    continue;
                }
                let arg = args.next().ok_or("too few parameters for RAISE")?;
                match pg_shown(&arg) {
                    Some(MsgPart::Text(s)) => text.push_str(&s),
                    Some(p) => {
                        parts.push(MsgPart::Text(std::mem::take(&mut text)));
                        parts.push(p);
                    }
                    None => return Err(
                        "a RAISE value that may be NULL (PostgreSQL shows <NULL>, T-SQL (null))"
                            .into(),
                    ),
                }
            }
            if args.next().is_some() {
                return Err("too many parameters for RAISE".into());
            }
            parts.push(MsgPart::Text(text));
            let msg = Msg {
                error,
                parts,
                tsql: false,
                format_var: false,
            };
            Ok((Err("a RAISE compared as its message".into()), Some(msg)))
        })();
    Some(lowered.unwrap_or_else(|e| (Err(e), None)))
}

/// A PL/pgSQL `RAISE` argument that is never NULL, as a message part: a
/// string literal, `COALESCE(x, 'text')` (a string `x`) or
/// `COALESCE(CAST(x AS text), 'text')` (an integer `x`).
fn pg_shown(e: &super::ast::Expr) -> Option<MsgPart> {
    use super::ast::Expr;
    match e {
        Expr::Str(s) => Some(MsgPart::Text(s.clone())),
        Expr::Call(c)
            if c.name.len() == 1
                && c.name[0].eq_ignore_ascii_case("COALESCE")
                && !c.distinct
                && !c.over
                && !c.star
                && c.order.is_empty() =>
        {
            let [x, Expr::Str(null)] = c.args.as_slice() else {
                return None;
            };
            let (value, spec) = match x {
                Expr::Cast(v, t) if t.name.eq_ignore_ascii_case("TEXT") && t.args.is_empty() => {
                    ((**v).clone(), Spec::Int)
                }
                v => (v.clone(), Spec::Str),
            };
            Some(MsgPart::Arg {
                value,
                null: null.clone(),
                spec,
            })
        }
        _ => None,
    }
}

/// `RAISERROR (format, severity, state [, args]) [WITH NOWAIT]`, from the
/// token after the keyword. Read only with a literal severity of 0 to 18 and
/// a format that is a string literal (`%s`, `%d`, `%i`, `%%` and as many
/// arguments as it takes) or a variable without arguments. Verified on SQL
/// Server 2022: a NULL argument shows as `(null)`, a NULL format as an
/// empty message; severity 0 to 10 is an informational message that
/// TRY/CATCH does not catch.
fn raiserror(toks: &[Tok]) -> Result<Msg, String> {
    use super::ast::Expr;
    let close = match toks.first() {
        Some(t) if t.kind == K::LParen => {
            let mut depth = 0;
            toks.iter()
                .position(|t| {
                    match t.kind {
                        K::LParen => depth += 1,
                        K::RParen => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })
                .ok_or("RAISERROR without its closing parenthesis")?
        }
        _ => return Err("RAISERROR without parentheses".into()),
    };
    // WITH NOWAIT only sends the message at once; LOG and SETERROR change
    // what is recorded.
    match toks[close + 1..]
        .iter()
        .map(|t| t.up.as_str())
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["WITH", "NOWAIT"] => {}
        _ => return Err("RAISERROR WITH LOG or SETERROR".into()),
    }
    let expr = |ts: &[Tok]| -> Result<Expr, String> {
        let mut p = Parser::new(ts, Dialect::Tsql);
        let e = p.expr()?;
        if p.at_end() {
            Ok(e)
        } else {
            Err("trailing tokens in a RAISERROR argument".into())
        }
    };
    let args = split_commas(&toks[1..close])
        .into_iter()
        .map(expr)
        .collect::<Result<Vec<_>, _>>()?;
    let [fmt, sev, _state, args @ ..] = args.as_slice() else {
        return Err("RAISERROR with fewer than three arguments".into());
    };
    let error = match sev {
        Expr::Num(s) => match s.parse::<u32>() {
            Ok(0..=10) => false,
            Ok(11..=18) => true,
            _ => return Err("a RAISERROR severity outside 0 to 18".into()),
        },
        _ => return Err("a RAISERROR severity that is not a literal".into()),
    };
    let null = "(null)".to_string();
    let mut parts = Vec::new();
    let format_var = match fmt {
        Expr::Var(v) if !v.starts_with("@@") => {
            if !args.is_empty() {
                return Err("a RAISERROR format variable with arguments".into());
            }
            parts.push(MsgPart::Arg {
                value: fmt.clone(),
                null: String::new(),
                spec: Spec::Str,
            });
            true
        }
        Expr::Str(f) => {
            let mut args = args.iter();
            let mut text = String::new();
            let mut cs = f.chars();
            while let Some(ch) = cs.next() {
                if ch != '%' {
                    text.push(ch);
                    continue;
                }
                let spec = match cs.next() {
                    Some('%') => {
                        text.push('%');
                        continue;
                    }
                    Some('s') => Spec::Str,
                    Some('d' | 'i') => Spec::Int,
                    _ => {
                        return Err(
                            "a RAISERROR format specification other than %s, %d, %i or %%".into(),
                        )
                    }
                };
                let value = args
                    .next()
                    .ok_or("a RAISERROR format with more specifications than arguments")?;
                // A constant shows as its text (the substitution is not read
                // as a format again).
                match (value, spec) {
                    (Expr::Str(x), Spec::Str) => {
                        text.push_str(x);
                        continue;
                    }
                    (Expr::Num(x), Spec::Int)
                        if x.parse::<i32>().is_ok_and(|v| v.to_string() == *x) =>
                    {
                        text.push_str(x);
                        continue;
                    }
                    _ => {}
                }
                parts.push(MsgPart::Text(std::mem::take(&mut text)));
                parts.push(MsgPart::Arg {
                    value: value.clone(),
                    null: null.clone(),
                    spec,
                });
            }
            if args.next().is_some() {
                return Err("a RAISERROR with more arguments than its format uses".into());
            }
            parts.push(MsgPart::Text(text));
            false
        }
        _ => return Err("a RAISERROR message number".into()),
    };
    Ok(Msg {
        error,
        parts,
        tsql: true,
        format_var,
    })
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
    if ty.name.eq_ignore_ascii_case("REFCURSOR") && at == toks.len() {
        u.cursor_vars.push((name, s.clone()));
        return;
    }
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
        msg: None,
        output: None,
        cursor: None,
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
