//! Cursor loops: the one shape T-SQL and PL/pgSQL share, compared statement
//! by statement.
//!
//! ```text
//! T-SQL                                    PL/pgSQL
//! DECLARE c CURSOR [opts] FOR query;       (c refcursor; in DECLARE)
//! OPEN c;                                  OPEN c FOR query;
//! FETCH [NEXT] [FROM] c INTO @a, …;        FETCH [NEXT] [FROM] c INTO a, …;
//! WHILE @@FETCH_STATUS = 0                 WHILE FOUND LOOP
//! BEGIN … FETCH … INTO @a, …; END          … FETCH … INTO a, …; END LOOP;
//! CLOSE c; DEALLOCATE c;                   CLOSE c;
//! ```
//!
//! The OPEN is compared as its query (`cursor-open`), each FETCH as the
//! assignments of the row's columns to its variables (`cursor-fetch`), the
//! CLOSE as itself, and both loop tests as one `cursor-found`. That is sound
//! only where the shape holds, which `start` checks on each side on its own:
//!
//! - T-SQL reads the query's variables at DECLARE, PostgreSQL at OPEN: the
//!   statements between them (T-SQL) set none of them and leave nothing.
//! - `@@FETCH_STATUS = 0` and `FOUND` tell the same only right after a FETCH
//!   of this cursor: one comes before the loop, the body ends with the same
//!   FETCH, and nothing in the body skips to the test (CONTINUE, GOTO, a
//!   label) or fetches, opens or closes the cursor again.
//! - A FETCH that finds no row leaves T-SQL's variables as they were and sets
//!   PostgreSQL's to NULL: after the loop each variable is set again before
//!   anything reads it (or the routine ends), on a straight path (no IF,
//!   loop, CATCH or GOTO between).
//! - A T-SQL cursor that is not STATIC may see the loop's own writes, a
//!   PostgreSQL cursor never does: the loop writes no table the query reads
//!   and calls nothing that might.
//! - PostgreSQL closes a cursor at COMMIT and ROLLBACK, T-SQL does not: no
//!   transaction control in the loop.
//!
//! What no reading of the code shows (other sessions, the row order beyond
//! ORDER BY, a GLOBAL cursor left open by an earlier error) is an assumption
//! (`canon::asm::CURSOR_*`).

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{parse_stmt, tokens, Expr, Select, Stmt, Tok, K};
use super::lower::Unit;
use crate::classify::{stmt_kind, write_target, StmtKind};

/// A cursor loop of the shape above, found on one side.
#[derive(Debug, Clone)]
pub struct CursorLoop {
    /// The cursor's name without what a conversion adds: T-SQL
    /// `OrderList` and PostgreSQL `cur_order_list` both give `orderlist`.
    pub key: String,
    /// The cursor's name as written.
    pub name: String,
    /// The T-SQL DECLARE (none in PostgreSQL, where the query is the OPEN's).
    pub decl: Option<SyntaxNode>,
    pub open: SyntaxNode,
    /// The FETCH before the loop and the one that ends its body.
    pub fetches: [SyntaxNode; 2],
    pub lp: SyntaxNode,
    pub close: SyntaxNode,
    /// T-SQL DEALLOCATE (PostgreSQL frees the cursor at CLOSE).
    pub dealloc: Option<SyntaxNode>,
    pub query: Result<Box<Select>, String>,
    /// The FETCH targets, as written.
    pub targets: Vec<String>,
    /// T-SQL STATIC or INSENSITIVE (PostgreSQL: always).
    pub stat: bool,
    /// T-SQL LOCAL (PostgreSQL: always).
    pub local: bool,
}

/// A statement of a cursor loop, compared as `Canon::cursor_op` says.
#[derive(Debug, Clone)]
pub enum CursorOp {
    Open {
        key: String,
        query: Result<Box<Select>, String>,
        /// T-SQL: (STATIC, LOCAL); `None` for PostgreSQL.
        tsql: Option<(bool, bool)>,
    },
    Fetch {
        key: String,
        targets: Vec<Expr>,
    },
    Close {
        key: String,
    },
}

/// The comparable key of a cursor name.
pub fn key(d: Dialect, name: &str) -> String {
    let n = cst::unquote(name).to_lowercase();
    let n = match d {
        Dialect::Postgres => n.strip_prefix("cur_").map(str::to_string).unwrap_or(n),
        _ => n,
    };
    n.replace('_', "")
}

fn words(n: &SyntaxNode) -> Vec<Tok> {
    tokens(n)
}

fn is_word(t: Option<&Tok>, w: &str) -> bool {
    t.is_some_and(|t| t.kind == K::Word && t.up == w)
}

/// The cursor name a statement `KW name` (OPEN, CLOSE, DEALLOCATE) names.
fn named(n: &SyntaxNode, kw: &str) -> Option<String> {
    if n.kind() != SyntaxKind::STATEMENT {
        return None;
    }
    match words(n).as_slice() {
        [k, name] if k.kind == K::Word && k.up == kw && name.kind == K::Word => {
            Some(name.text.clone())
        }
        _ => None,
    }
}

/// `FETCH [NEXT] [FROM | IN] name INTO t1, t2, …`: (name, targets).
fn fetch(n: &SyntaxNode, d: Dialect) -> Option<(String, Vec<String>)> {
    if n.kind() != SyntaxKind::STATEMENT {
        return None;
    }
    let ts = words(n);
    let mut i = 1;
    if !is_word(ts.first(), "FETCH") {
        return None;
    }
    if is_word(ts.get(i), "NEXT") {
        i += 1;
    }
    if is_word(ts.get(i), "FROM") || (d == Dialect::Postgres && is_word(ts.get(i), "IN")) {
        i += 1;
    }
    let name = ts.get(i).filter(|t| t.kind == K::Word)?.text.clone();
    if !is_word(ts.get(i + 1), "INTO") {
        return None;
    }
    let mut targets: Vec<String> = Vec::new();
    for (k, t) in ts[i + 2..].iter().enumerate() {
        if k % 2 == 1 {
            if t.kind != K::Comma {
                return None;
            }
            continue;
        }
        let ok = match d {
            Dialect::Tsql => t.kind == K::Var && !t.text.starts_with("@@"),
            _ => t.kind == K::Word,
        };
        if !ok || targets.iter().any(|x| x.eq_ignore_ascii_case(&t.text)) {
            return None;
        }
        targets.push(t.text.clone());
    }
    // The list ends with a target, not a comma.
    (!targets.is_empty() && (ts.len() - (i + 2)) % 2 == 1).then_some((name, targets))
}

/// The units after `n` in its list.
fn after(n: &SyntaxNode) -> Vec<SyntaxNode> {
    let Some(p) = n.parent() else {
        return Vec::new();
    };
    cst::structural_children(&p)
        .into_iter()
        .skip_while(|c| c != n)
        .skip(1)
        .collect()
}

/// The statements of a loop body: a BEGIN … END block's, or the body's own.
fn body_units(branch: &SyntaxNode) -> Vec<SyntaxNode> {
    let kids = cst::structural_children(branch);
    match kids.as_slice() {
        [b] if b.kind() == SyntaxKind::BLOCK => cst::structural_children(b),
        _ => kids,
    }
}

/// A T-SQL cursor declaration: (name, STATIC, LOCAL, query tokens).
pub type Declared = (String, bool, bool, Vec<Tok>);

/// `DECLARE name [opts] CURSOR [opts] FOR query`: (name, STATIC, LOCAL,
/// query tokens), or why it is not one this module compares. `None` when
/// the statement is not a cursor declaration.
pub fn tsql_declaration(n: &SyntaxNode) -> Option<Result<Declared, String>> {
    let ts = words(n);
    if !is_word(ts.first(), "DECLARE") || ts.get(1).map_or(true, |t| t.kind != K::Word) {
        return None;
    }
    let for_at = ts.iter().position(|t| t.kind == K::Word && t.up == "FOR");
    let head_end = for_at.unwrap_or(ts.len());
    if !ts[2..head_end]
        .iter()
        .any(|t| t.kind == K::Word && t.up == "CURSOR")
    {
        return None;
    }
    let name = ts[1].text.clone();
    let Some(for_at) = for_at else {
        return Some(Err("a cursor declaration without FOR".into()));
    };
    let (mut stat, mut local) = (false, false);
    for t in &ts[2..for_at] {
        match t.up.as_str() {
            "CURSOR" | "GLOBAL" | "FORWARD_ONLY" | "FAST_FORWARD" | "READ_ONLY" | "KEYSET"
            | "DYNAMIC" => {}
            "LOCAL" => local = true,
            "STATIC" | "INSENSITIVE" => stat = true,
            o => return Some(Err(format!("a cursor declared {o}"))),
        }
    }
    let query: Vec<Tok> = ts[for_at + 1..].to_vec();
    Some(Ok((name, stat, local, query)))
}

/// Why the query is not one a cursor of this shape can run.
fn query_shape(query: &[Tok], d: Dialect) -> Result<(), String> {
    if !(is_word(query.first(), "SELECT") || is_word(query.first(), "WITH")) {
        return Err("a cursor over something other than a query".into());
    }
    // `Canon::stmt` folds such names for the comparison with T-SQL; the
    // OPEN's query is lowered without that step.
    if d == Dialect::Postgres
        && query
            .iter()
            .any(|t| t.kind == K::Quoted && t.text != t.text.to_lowercase())
    {
        return Err("a cursor query with quoted names that keep capitals".into());
    }
    let mut depth = 0i32;
    for (i, t) in query.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Word if depth == 0 && t.up == "FOR" => {
                if let Some(next) = query.get(i + 1) {
                    if matches!(
                        next.up.as_str(),
                        "UPDATE" | "READ" | "XML" | "JSON" | "BROWSE" | "SHARE" | "NO" | "KEY"
                    ) {
                        return Err(format!("a cursor query with FOR {}", next.up));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The cursor loop that starts at `n` (a T-SQL DECLARE CURSOR or a
/// PostgreSQL `OPEN c FOR query`), checked as the module says; `None` when
/// `n` starts none.
pub fn start(n: &SyntaxNode, u: &Unit) -> Option<Result<CursorLoop, String>> {
    let d = u.dialect;
    match d {
        Dialect::Tsql => {
            let decl = tsql_declaration(n)?;
            Some(
                decl.and_then(|(name, stat, local, query)| {
                    tsql_loop(n, u, &name, stat, local, query)
                }),
            )
        }
        Dialect::Postgres => {
            let ts = words(n);
            if n.kind() != SyntaxKind::STATEMENT
                || !is_word(ts.first(), "OPEN")
                || !is_word(ts.get(2), "FOR")
                || ts.get(1).map_or(true, |t| t.kind != K::Word)
            {
                return None;
            }
            let name = ts[1].text.clone();
            let query = ts[3..].to_vec();
            Some(shape(n, u, &name, None, n.clone(), &[], true, true, query))
        }
        Dialect::Db2i => None,
    }
}

fn tsql_loop(
    decl: &SyntaxNode,
    u: &Unit,
    name: &str,
    stat: bool,
    local: bool,
    query: Vec<Tok>,
) -> Result<CursorLoop, String> {
    let rest = after(decl);
    let is_open = |x: &SyntaxNode| named(x, "OPEN").is_some_and(|m| m.eq_ignore_ascii_case(name));
    let Some(at) = rest.iter().position(is_open) else {
        return Err("a cursor that is not opened in the same statement list".into());
    };
    let between = &rest[..at];
    shape(
        decl,
        u,
        name,
        Some(decl.clone()),
        rest[at].clone(),
        between,
        stat,
        local,
        query,
    )
}

#[allow(clippy::too_many_arguments)]
fn shape(
    first: &SyntaxNode,
    u: &Unit,
    name: &str,
    decl: Option<SyntaxNode>,
    open: SyntaxNode,
    between: &[SyntaxNode],
    stat: bool,
    local: bool,
    query: Vec<Tok>,
) -> Result<CursorLoop, String> {
    let d = u.dialect;
    let same = |m: &str| match d {
        Dialect::Tsql => m.eq_ignore_ascii_case(name),
        _ => m.to_lowercase() == name.to_lowercase(),
    };
    query_shape(&query, d)?;
    if first
        .ancestors()
        .skip(1)
        .any(|a| a.kind() == SyntaxKind::LOOP_STMT)
    {
        return Err("a cursor loop inside another loop".into());
    }
    // T-SQL reads the query's variables at DECLARE, PostgreSQL at OPEN.
    let qvars: Vec<String> = query
        .iter()
        .filter(|t| t.kind == K::Var)
        .map(|t| t.up.clone())
        .collect();
    for b in between {
        let ts = words(b);
        let leaves = ts.first().is_some_and(|t| {
            matches!(
                t.up.as_str(),
                "RETURN" | "GOTO" | "BREAK" | "CONTINUE" | "THROW" | "RAISERROR"
            )
        });
        if b.kind() != SyntaxKind::STATEMENT
            || leaves
            || ts.iter().any(|t| t.kind == K::Word && same(&t.text))
            || ts.iter().any(|t| t.kind == K::Var && qvars.contains(&t.up))
        {
            return Err("statements between the cursor's DECLARE and OPEN that may change what its query reads at DECLARE, or leave before the OPEN".into());
        }
    }
    let rest = after(&open);
    let shape_err = || {
        Err::<CursorLoop, String>(
            "a cursor not used in the standard loop (OPEN; FETCH … INTO; WHILE over the fetch status ending with the same FETCH; CLOSE)".into(),
        )
    };
    let n_after = if d == Dialect::Tsql { 4 } else { 3 };
    if rest.len() < n_after {
        return shape_err();
    }
    let (f0, lp, close) = (&rest[0], &rest[1], &rest[2]);
    let Some((fname, targets)) = fetch(f0, d) else {
        return shape_err();
    };
    if !same(&fname) || !named(close, "CLOSE").is_some_and(|m| same(&m)) {
        return shape_err();
    }
    let dealloc = if d == Dialect::Tsql {
        let x = &rest[3];
        if !named(x, "DEALLOCATE").is_some_and(|m| same(&m)) {
            return shape_err();
        }
        Some(x.clone())
    } else {
        None
    };
    // The loop: WHILE over the fetch status, no label.
    let kids: Vec<SyntaxNode> = lp.children().collect();
    let [cond, body] = kids.as_slice() else {
        return shape_err();
    };
    if lp.kind() != SyntaxKind::LOOP_STMT
        || cond.kind() != SyntaxKind::CONDITION
        || body.kind() != SyntaxKind::BRANCH
        || !cst::first_words(lp, 1)
            .first()
            .is_some_and(|w| w == "WHILE")
    {
        return shape_err();
    }
    let mut ct: Vec<String> = words(cond).iter().map(|t| t.up.clone()).collect();
    while ct.first().is_some_and(|x| x == "(") && ct.last().is_some_and(|x| x == ")") {
        ct = ct[1..ct.len() - 1].to_vec();
    }
    let want: &[&str] = match d {
        Dialect::Tsql => &["@@FETCH_STATUS", "=", "0"],
        _ => &["FOUND"],
    };
    if ct != want {
        return shape_err();
    }
    let inner = body_units(body);
    let Some(last) = inner.last() else {
        return shape_err();
    };
    match fetch(last, d) {
        Some((m, t))
            if same(&m)
                && t.len() == targets.len()
                && t.iter().zip(&targets).all(|(x, y)| x.eq_ignore_ascii_case(y)) => {}
        _ => {
            return Err("a cursor loop whose body does not end with the same FETCH as before the loop: the loop test then does not read that FETCH".into())
        }
    }
    let mut body_writes: Vec<String> = Vec::new();
    for x in body.descendants() {
        if x.kind() == SyntaxKind::LABEL {
            return Err("a label in a cursor loop".into());
        }
        if x.kind() != SyntaxKind::STATEMENT {
            continue;
        }
        let ts = words(&x);
        let w0 = ts.first().map(|t| t.up.as_str()).unwrap_or("");
        if matches!(w0, "CONTINUE" | "GOTO" | "ITERATE") {
            return Err(format!(
                "{w0} in a cursor loop: the loop test would then not follow the cursor's FETCH"
            ));
        }
        let kind = stmt_kind(&x, d);
        if kind.is_txn_control()
            || matches!(w0, "COMMIT" | "ROLLBACK" | "SAVE" | "SAVEPOINT" | "RELEASE")
        {
            return Err("transaction control in a cursor loop: PostgreSQL closes the cursor at COMMIT and ROLLBACK, T-SQL keeps it open".into());
        }
        if x != *last
            && matches!(w0, "FETCH" | "OPEN" | "CLOSE" | "DEALLOCATE" | "MOVE")
            && ts.iter().any(|t| t.kind == K::Word && same(&t.text))
        {
            return Err("a cursor fetched, opened or closed again inside its loop".into());
        }
        if !stat {
            if matches!(w0, "EXEC" | "EXECUTE" | "CALL" | "PERFORM") || super::dynamic::is_tsql(&x)
            {
                return Err("a cursor loop that calls a procedure or runs dynamic SQL, which may change the rows a cursor that is not STATIC still has to fetch".into());
            }
            if kind.is_write() || w0 == "WITH" {
                body_writes.extend(written(&x, kind));
            }
        }
    }
    if !stat {
        let reads = read_tables(&query);
        if let Some(t) = reads.iter().find(|t| body_writes.contains(t)) {
            return Err(format!("a cursor loop that writes {t}, which the cursor reads: a T-SQL cursor that is not STATIC may see the change, a PostgreSQL cursor does not"));
        }
    }
    // FETCH targets: not parameters.
    for t in &targets {
        let bare = t.trim_start_matches('@').to_lowercase();
        if u.params.iter().any(|p| p.name == bare) {
            return Err(format!("FETCH INTO the parameter {t}: PostgreSQL's last FETCH sets it to NULL, T-SQL keeps the last row's value"));
        }
    }
    let end = dealloc.clone().unwrap_or_else(|| close.clone());
    for t in &targets {
        dead_after(&end, lp, u, t)?;
    }
    let query_ast = match parse_stmt(&query, d) {
        Ok(Stmt::Select(s)) => Ok(s),
        Ok(_) => Err("a cursor query that is not a SELECT".to_string()),
        Err(e) => Err(e),
    };
    Ok(CursorLoop {
        key: key(d, name),
        name: name.to_string(),
        decl,
        open,
        fetches: [f0.clone(), last.clone()],
        lp: lp.clone(),
        close: close.clone(),
        dealloc,
        query: query_ast,
        targets,
        stat,
        local,
    })
}

/// The tables after FROM, JOIN, APPLY or USING (last name part, lower
/// case): over-approximates what a query reads.
fn read_tables(ts: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, t) in ts.iter().enumerate() {
        if !(t.kind == K::Word && matches!(t.up.as_str(), "FROM" | "JOIN" | "APPLY" | "USING")) {
            continue;
        }
        let mut last = None;
        let mut j = i + 1;
        while let Some(x) = ts.get(j) {
            if !matches!(x.kind, K::Word | K::Quoted) {
                break;
            }
            last = Some(cst::unquote(&x.text).to_lowercase());
            if ts.get(j + 1).is_some_and(|y| y.kind == K::Dot) {
                j += 2;
            } else {
                break;
            }
        }
        if let Some(l) = last {
            out.push(l);
        }
    }
    out
}

/// The tables a write may change: its target, and for UPDATE, DELETE and
/// statements with a CTE every table it names (the target may be an alias).
fn written(x: &SyntaxNode, kind: StmtKind) -> Vec<String> {
    let first = cst::first_words(x, 1);
    if matches!(kind, StmtKind::Update | StmtKind::Delete)
        || first.first().is_some_and(|w| w == "WITH")
    {
        let mut v = read_tables(&words(x));
        if let Some(t) = write_target(x).and_then(|p| p.last().cloned()) {
            v.push(cst::unquote(&t).to_lowercase());
        }
        return v;
    }
    write_target(x)
        .and_then(|p| p.last().cloned())
        .map(|t| vec![cst::unquote(&t).to_lowercase()])
        .unwrap_or_default()
}

/// Whether `name` (a FETCH target) is set again before anything reads it
/// after the loop: the first mention after `end` is `SET @v = e` / `v := e`
/// with no `v` in `e`, or the first FETCH of another cursor loop of this
/// shape (whose own check covers what follows it); or there is none. The
/// loop and that statement sit on a straight path: no IF, loop, CATCH,
/// handler or EXCEPTION section around either, and no jump past it
/// (`jumps`).
fn dead_after(end: &SyntaxNode, lp: &SyntaxNode, u: &Unit, name: &str) -> Result<(), String> {
    let d = u.dialect;
    let stale = || {
        Err(format!(
            "{name} may be read after the cursor loop: T-SQL keeps the last row's value there, PostgreSQL's last FETCH sets it to NULL"
        ))
    };
    let straight = |x: &SyntaxNode| {
        x.ancestors().skip(1).all(|a| {
            matches!(
                a.kind(),
                SyntaxKind::BLOCK
                    | SyntaxKind::ROUTINE_BODY
                    | SyntaxKind::ROUTINE
                    | SyntaxKind::BATCH
                    | SyntaxKind::SOURCE_FILE
            ) && !a
                .children()
                .any(|c| c.kind() == SyntaxKind::EXCEPTION_SECTION)
        })
    };
    if !straight(lp) {
        return stale();
    }
    let root = lp
        .ancestors()
        .find(|a| {
            matches!(
                a.kind(),
                SyntaxKind::ROUTINE_BODY | SyntaxKind::BATCH | SyntaxKind::SOURCE_FILE
            )
        })
        .unwrap_or_else(|| lp.clone());
    let end_at = end.text_range().end();
    let mentions = |t: &Tok| match d {
        Dialect::Tsql => t.kind == K::Var && t.text.eq_ignore_ascii_case(name),
        _ => t.kind == K::Word && t.text.eq_ignore_ascii_case(name),
    };
    let first = root
        .descendants()
        .filter(|x| x.kind() == SyntaxKind::STATEMENT && x.text_range().start() >= end_at)
        .find(|x| words(x).iter().any(mentions));
    jumps(&root, lp, end, first.as_ref(), d).map_err(|why| {
        format!("{name} may be read after the cursor loop ({why}): T-SQL keeps the last row's value there, PostgreSQL's last FETCH sets it to NULL")
    })?;
    let Some(st) = first else {
        return Ok(());
    };
    if !straight(&st) {
        return stale();
    }
    let ts = words(&st);
    let set = match d {
        Dialect::Tsql => {
            is_word(ts.first(), "SET")
                && ts.get(1).is_some_and(mentions)
                && ts.get(2).is_some_and(|t| t.text == "=")
                && ts.iter().filter(|t| mentions(t)).count() == 1
        }
        _ => {
            ts.first().is_some_and(mentions)
                && ts.get(1).is_some_and(|t| t.text == ":=" || t.text == "=")
                && ts.iter().filter(|t| mentions(t)).count() == 1
        }
    };
    if set {
        return Ok(());
    }
    // The first FETCH of another cursor loop of this shape.
    if fetch(&st, d).is_some_and(|(_, t)| t.iter().any(|x| x.eq_ignore_ascii_case(name))) {
        if let Some(prev) = before(&st) {
            if is_word(words(&prev).first(), "OPEN") {
                return loop_from_open(&prev, u);
            }
        }
    }
    stale()
}

/// The unit before `n` in its list.
fn before(n: &SyntaxNode) -> Option<SyntaxNode> {
    let p = n.parent()?;
    let kids = cst::structural_children(&p);
    let i = kids.iter().position(|c| c == n)?;
    i.checked_sub(1).map(|j| kids[j].clone())
}

/// The OPEN `open` starts a cursor loop of the module's shape (found from
/// its OPEN: the T-SQL DECLARE before it, or the PostgreSQL OPEN itself).
fn loop_from_open(open: &SyntaxNode, u: &Unit) -> Result<(), String> {
    let refuse = || {
        Err("a later cursor loop that reuses the variable is not of the standard shape".to_string())
    };
    match u.dialect {
        Dialect::Tsql => {
            let Some(name) = named(open, "OPEN") else {
                return refuse();
            };
            let p = open.parent().ok_or("no parent")?;
            let decl = cst::structural_children(&p)
                .into_iter()
                .take_while(|c| c != open)
                .filter(|c| {
                    tsql_declaration(c)
                        .and_then(Result::ok)
                        .is_some_and(|(m, ..)| m.eq_ignore_ascii_case(&name))
                })
                .last();
            match decl.and_then(|x| start(&x, u)) {
                Some(Ok(_)) => Ok(()),
                _ => refuse(),
            }
        }
        _ => match start(open, u) {
            Some(Ok(_)) => Ok(()),
            _ => refuse(),
        },
    }
}

/// No jump makes a read after the loop see the value its last FETCH left:
/// T-SQL, no GOTO after the loop leads back before it (statements there run
/// again after the loop), and none between the loop and the statement that
/// sets the variable again (`write`) leads past that statement. PL/pgSQL
/// has no GOTO, but EXIT of a labelled block jumps forward: no label at all.
fn jumps(
    root: &SyntaxNode,
    lp: &SyntaxNode,
    end: &SyntaxNode,
    write: Option<&SyntaxNode>,
    d: Dialect,
) -> Result<(), String> {
    let labels: Vec<(String, usize)> = root
        .descendants()
        .filter(|x| x.kind() == SyntaxKind::LABEL)
        .map(|x| {
            let name = words(&x).first().map(|t| t.up.clone()).unwrap_or_default();
            (name, usize::from(x.text_range().start()))
        })
        .collect();
    if d != Dialect::Tsql {
        return if labels.is_empty() {
            Ok(())
        } else {
            Err("a label in the routine".into())
        };
    }
    let start = usize::from(lp.text_range().start());
    let end = usize::from(end.text_range().end());
    let w = write.map(|x| usize::from(x.text_range().start()));
    for g in root.descendants().filter(|x| {
        x.kind() == SyntaxKind::STATEMENT
            && cst::first_words(x, 1).first().is_some_and(|w| w == "GOTO")
    }) {
        let at = usize::from(g.text_range().start());
        if at < end {
            continue;
        }
        let target = words(&g).get(1).map(|t| t.up.clone()).unwrap_or_default();
        let Some(&(_, to)) = labels.iter().find(|(n, _)| *n == target) else {
            return Err(format!("GOTO {target} to no label found"));
        };
        if to < start {
            return Err(format!("GOTO {target} leads back before the loop"));
        }
        if let Some(w) = w {
            if at < w && to > w {
                return Err(format!(
                    "GOTO {target} skips the statement that sets it again"
                ));
            }
        }
    }
    Ok(())
}
