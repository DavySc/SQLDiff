//! A T-SQL AFTER trigger against the PostgreSQL statement triggers it became
//! (rule T5 of the trigger conversion).
//!
//! T-SQL `CREATE TRIGGER t ON tbl AFTER INSERT, UPDATE AS body` runs `body`
//! once per statement with the statement's rows in `inserted` and `deleted`.
//! PostgreSQL has transition tables only on a trigger with one event, so the
//! conversion is one `FOR EACH STATEMENT` trigger per event, each running its
//! own function, with `REFERENCING NEW TABLE AS inserted OLD TABLE AS
//! deleted` (only the images the event has). Each event is compared on its
//! own: the T-SQL body lowered for that event (the image the event lacks is
//! empty, as `(SELECT * FROM tbl WHERE false)` is) against that event's
//! function, with both images known as copies of the table.
//!
//! Checked on SQL Server 2022 and PostgreSQL 18 (2026-10-27): both fire a
//! statement trigger once per statement, also for a statement that changes
//! no rows, with the statement's rows in the images; an error in a T-SQL
//! trigger ends the batch and dooms the transaction, in PostgreSQL it fails
//! the statement (`ASM-TRIGGER-ERROR`); a T-SQL trigger that changes its own
//! table does not fire itself again (RECURSIVE_TRIGGERS OFF, the default),
//! a PostgreSQL one does, also when the change touches no rows. That
//! re-entry is proven harmless or the pair is not proven (`reentry`).

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{tokens, Expr, From, Stmt, Tok, K};
use super::canon::asm;
use super::lower::{self, TrigCtx, Unit};
use super::schema::Schema;
use super::Outcome;
use crate::artifact::Analyzed;

pub const ERROR: (&str, &str) = (
    "ASM-TRIGGER-ERROR",
    "An error in the trigger ends the T-SQL batch and dooms the transaction; in PostgreSQL it fails the statement that fired the trigger, and a caller that catches the error carries on. Equal for callers that do not catch it",
);
pub const ORDER: (&str, &str) = (
    "ASM-TRIGGER-ORDER",
    "Several AFTER triggers for one event on the table do not depend on each other's effects: T-SQL fires them in an unspecified order (unless sp_settriggerorder sets it), PostgreSQL in name order",
);
pub const ROWCOUNT: (&str, &str) = (
    "ASM-TRIGGER-ROWCOUNT",
    "No MERGE fires the trigger, or the rest of the trigger does nothing when its image is empty: T-SQL's @@ROWCOUNT counts the rows of every MERGE action, so `IF @@ROWCOUNT = 0 RETURN` goes on with an empty image where the PostgreSQL test for rows returns",
);
pub const REPLICATION: (&str, &str) = (
    "ASM-TRIGGER-REPLICATION",
    "No replication agent writes the table: the T-SQL trigger is NOT FOR REPLICATION, so it does not fire for them, and a PostgreSQL trigger does not fire for logical replication unless enabled with ENABLE REPLICA or ENABLE ALWAYS",
);
pub const REENTRY_OTHERS: (&str, &str) = (
    "ASM-TRIGGER-REENTRY-OTHERS",
    "The other UPDATE triggers on the table do nothing when their images are empty, and no BEFORE trigger changes the columns the trigger tests for a change: PostgreSQL fires this trigger again for its own UPDATE, which then changes no rows only because those columns did not change, and that UPDATE fires every UPDATE trigger on the table once more",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Insert,
    Update,
    Delete,
}

impl Event {
    fn parse(w: &str) -> Option<Event> {
        match w {
            "INSERT" => Some(Event::Insert),
            "UPDATE" => Some(Event::Update),
            "DELETE" => Some(Event::Delete),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Event::Insert => "INSERT",
            Event::Update => "UPDATE",
            Event::Delete => "DELETE",
        }
    }

    /// The images the event has: (`NEW`/`OLD`, name).
    fn images(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Event::Insert => &[("NEW", "inserted")],
            Event::Update => &[("NEW", "inserted"), ("OLD", "deleted")],
            Event::Delete => &[("OLD", "deleted")],
        }
    }

    fn ctx(self, table: Vec<String>, tsql: bool) -> TrigCtx {
        let (rows, missing) = match self {
            Event::Insert => ("inserted", Some("deleted")),
            Event::Update => ("inserted", None),
            Event::Delete => ("deleted", Some("inserted")),
        };
        TrigCtx::new(table, rows, if tsql { missing } else { None })
    }
}

/// One PostgreSQL `CREATE TRIGGER`.
struct PgTrigger {
    event: Event,
    table: Vec<String>,
    /// (`NEW`/`OLD`, name lower-cased).
    images: Vec<(String, String)>,
    function: Vec<String>,
    node: SyntaxNode,
}

/// A trigger pair for one event: the units and the schemas that know the
/// images.
pub(super) type Pair = (Unit, Unit, Schema, Schema);

/// The per-event pairs when side A is one T-SQL trigger and side B is
/// PostgreSQL; `None` otherwise (the artifacts are compared as usual). When
/// B is not one statement trigger per event of A, nothing is paired and the
/// reason is reported.
pub(super) fn pairs(
    a: &Analyzed,
    b: &Analyzed,
    sa: &Schema,
    sb: &Schema,
    out: &mut Outcome,
) -> Option<Vec<Pair>> {
    if a.dialect() != Dialect::Tsql || b.dialect() != Dialect::Postgres {
        return None;
    }
    let routines: Vec<SyntaxNode> = a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .collect();
    let [r] = routines.as_slice() else {
        return None;
    };
    let header: Vec<Tok> = cst::header_tokens(r)
        .iter()
        .map(Tok::from_syntax)
        .filter(|t| t.kind != K::Semi)
        .collect();
    if !header.iter().take(4).any(|t| t.up == "TRIGGER") {
        return None;
    }
    let outside = a.root.descendants().any(|n| {
        n.kind() == SyntaxKind::STATEMENT && !r.text_range().contains_range(n.text_range())
    });
    if outside {
        return None;
    }
    let at = place(a, r);
    let (table, events, replication) = match tsql_header(&header) {
        Ok(x) => x,
        Err(why) => return fail(out, &at, why),
    };
    let Some(body) = r.children().find(|c| c.kind() == SyntaxKind::ROUTINE_BODY) else {
        return fail(out, &at, "the trigger has no body".into());
    };
    let (functions, triggers) = match pg_side(b) {
        Ok(x) => x,
        Err(why) => return fail(out, &at, why),
    };
    if triggers.len() != events.len() || functions.len() != events.len() {
        return fail(out, &at, format!(
            "it has {} event(s); side B has {} trigger(s) and {} function(s), where one trigger and one function per event is the conversion",
            events.len(),
            triggers.len(),
            functions.len()
        ));
    }
    let mut pairs = Vec::new();
    for &ev in &events {
        let tr: Vec<&PgTrigger> = triggers.iter().filter(|t| t.event == ev).collect();
        let [tr] = tr.as_slice() else {
            return fail(
                out,
                &at,
                format!("side B has no single AFTER {} trigger for it", ev.word()),
            );
        };
        if !same_table(&table, &tr.table) {
            return fail(
                out,
                &at,
                format!(
                    "its table is {} and the AFTER {} trigger of side B is on {}",
                    table.join("."),
                    ev.word(),
                    tr.table.join(".")
                ),
            );
        }
        let mut want: Vec<(String, String)> = ev
            .images()
            .iter()
            .map(|(w, n)| (w.to_string(), n.to_string()))
            .collect();
        let mut got = tr.images.clone();
        want.sort();
        got.sort();
        if want != got {
            return fail(out, &at, format!(
                "the AFTER {} trigger of side B (line {}) must declare exactly {} as its transition tables",
                ev.word(),
                b.line_of(&tr.node),
                ev.images()
                    .iter()
                    .map(|(w, n)| format!("{w} TABLE AS {n}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        let f: Vec<&(Vec<String>, SyntaxNode)> = functions
            .iter()
            .filter(|(name, _)| same_name(name, &tr.function))
            .collect();
        let [(_, f)] = f.as_slice() else {
            return fail(
                out,
                &at,
                format!(
                    "side B has no single function {} for the AFTER {} trigger",
                    tr.function.join("."),
                    ev.word()
                ),
            );
        };
        let Some(fbody) = f.children().find(|c| c.kind() == SyntaxKind::ROUTINE_BODY) else {
            return fail(
                out,
                &at,
                format!("the function {} has no body", tr.function.join(".")),
            );
        };
        let ua = lower::lower_trigger(Dialect::Tsql, &body, ev.ctx(table.clone(), true));
        let ub = lower::lower_trigger(Dialect::Postgres, &fbody, ev.ctx(tr.table.clone(), false));
        let (ga, gb) = (guarded(&ua), guarded(&ub));
        if ga != gb {
            let (with, without) = if ga { ("A", "B") } else { ("B", "A") };
            out.unknown_reasons.push(format!(
                "{at}the AFTER {} trigger: side {with} returns at once when its image is empty, side {without} does not",
                ev.word()
            ));
        } else if ua.trigger.as_ref().is_some_and(|t| t.rowcount) {
            out.assume(asm::make(ROWCOUNT));
        }
        let names: Vec<&str> = ev.images().iter().map(|(_, n)| *n).collect();
        let sa2 = sa.with_images(&table, &names).unwrap_or_else(|| sa.clone());
        let sb2 = sb
            .with_images(&tr.table, &names)
            .unwrap_or_else(|| sb.clone());
        pairs.push((ua, ub, sa2, sb2));
    }
    out.assume(asm::make(ERROR));
    out.assume(asm::make(ORDER));
    if replication {
        out.assume(asm::make(REPLICATION));
    }
    reentry(b, &table, &events, &pairs, out);
    Some(pairs)
}

/// Side B's trigger functions when side A is one T-SQL trigger and side B
/// holds several functions: each runs the trigger's statements for one
/// event, so writes are paired function by function.
pub fn functions(a: &Analyzed, b: &Analyzed) -> Option<Vec<SyntaxNode>> {
    if a.dialect() != Dialect::Tsql || b.dialect() != Dialect::Postgres {
        return None;
    }
    let mut ra = a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE);
    let r = ra.next()?;
    if ra.next().is_some() {
        return None;
    }
    let header: Vec<Tok> = cst::header_tokens(&r)
        .iter()
        .map(Tok::from_syntax)
        .collect();
    if !header.iter().take(4).any(|t| t.up == "TRIGGER") {
        return None;
    }
    let fns: Vec<SyntaxNode> = b
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .collect();
    (fns.len() > 1).then_some(fns)
}

fn fail(out: &mut Outcome, at: &str, why: String) -> Option<Vec<Pair>> {
    out.unknown_reasons
        .push(format!("{at}the T-SQL trigger is not compared: {why}"));
    Some(Vec::new())
}

/// `side A (T-SQL) line 3: `, the prefix that places a reason on a line.
fn place(side: &Analyzed, node: &SyntaxNode) -> String {
    format!("{} line {}: ", super::side_name(side), side.line_of(node))
}

fn guarded(u: &Unit) -> bool {
    u.trigger.as_ref().is_some_and(|t| t.guard)
}

/// A qualified name at `toks[i..]`: the parts unquoted, and the index after it.
fn qname(toks: &[Tok], mut i: usize) -> Option<(Vec<String>, usize)> {
    let mut parts = Vec::new();
    loop {
        let t = toks
            .get(i)
            .filter(|t| matches!(t.kind, K::Word | K::Quoted))?;
        parts.push(cst::unquote(&t.text).to_string());
        i += 1;
        if toks.get(i).is_some_and(|t| t.kind == K::Dot) {
            i += 1;
        } else {
            return Some((parts, i));
        }
    }
}

/// `{CREATE | ALTER | CREATE OR ALTER} TRIGGER name ON table [WITH
/// ENCRYPTION] {FOR | AFTER} events [WITH APPEND] AS`: the table and events.
/// And whether it says NOT FOR REPLICATION.
fn tsql_header(t: &[Tok]) -> Result<(Vec<String>, Vec<Event>, bool), String> {
    let up = |i: usize| t.get(i).map(|x| x.up.as_str()).unwrap_or("");
    let kw = (0..t.len().min(4))
        .find(|&i| up(i) == "TRIGGER")
        .ok_or("no TRIGGER keyword")?;
    let (_, mut i) = qname(t, kw + 1).ok_or("the trigger name is not understood")?;
    if up(i) != "ON" {
        return Err("no ON table".into());
    }
    if matches!(up(i + 1), "DATABASE" | "ALL") {
        return Err("a DDL trigger".into());
    }
    let (table, j) = qname(t, i + 1).ok_or("the table name is not understood")?;
    i = j;
    if up(i) == "WITH" && up(i + 1) == "ENCRYPTION" {
        i += 2;
    }
    match up(i) {
        "FOR" | "AFTER" => i += 1,
        "INSTEAD" => return Err("an INSTEAD OF trigger".into()),
        w => return Err(format!("`{w}` where FOR or AFTER is expected")),
    }
    let mut events = Vec::new();
    loop {
        let ev = Event::parse(up(i)).ok_or("the events are not understood")?;
        if !events.contains(&ev) {
            events.push(ev);
        }
        i += 1;
        if t.get(i).is_some_and(|x| x.kind == K::Comma) {
            i += 1;
        } else {
            break;
        }
    }
    if up(i) == "WITH" && up(i + 1) == "APPEND" {
        i += 2;
    }
    let mut replication = false;
    if up(i) == "NOT" && up(i + 1) == "FOR" && up(i + 2) == "REPLICATION" {
        replication = true;
        i += 3;
    }
    if !(up(i) == "AS" && i + 1 == t.len() || i == t.len()) {
        return Err(format!("`{}` after the events is not understood", up(i)));
    }
    Ok((table, events, replication))
}

type Functions = Vec<(Vec<String>, SyntaxNode)>;

/// Side B's trigger functions and triggers; every top-level statement must
/// be one of them.
fn pg_side(b: &Analyzed) -> Result<(Functions, Vec<PgTrigger>), String> {
    let mut functions = Vec::new();
    let mut triggers = Vec::new();
    let top = cst::structural_children(&b.root).into_iter().flat_map(|n| {
        if n.kind() == SyntaxKind::BATCH {
            cst::structural_children(&n)
        } else {
            vec![n]
        }
    });
    for n in top {
        match n.kind() {
            SyntaxKind::ROUTINE => {
                let h: Vec<Tok> = cst::header_tokens(&n)
                    .iter()
                    .map(Tok::from_syntax)
                    .filter(|t| t.kind != K::Semi)
                    .collect();
                let name = pg_function(&h).ok_or_else(|| {
                    format!(
                        "side B line {}: only `CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql` is compared with a trigger",
                        b.line_of(&n)
                    )
                })?;
                functions.push((name, n));
            }
            SyntaxKind::STATEMENT => {
                let t = tokens(&n);
                let tr = pg_trigger(&t, n.clone()).ok_or_else(|| {
                    format!(
                        "side B line {}: only `CREATE TRIGGER name AFTER event ON table REFERENCING … FOR EACH STATEMENT EXECUTE FUNCTION f()` is compared with a trigger",
                        b.line_of(&n)
                    )
                })?;
                triggers.push(tr);
            }
            _ => {
                return Err(format!(
                    "side B line {}: a construct other than a trigger function or a trigger",
                    b.line_of(&n)
                ))
            }
        }
    }
    Ok((functions, triggers))
}

/// `CREATE [OR REPLACE] FUNCTION name() RETURNS trigger [LANGUAGE plpgsql]
/// [VOLATILE] AS …`: the name.
fn pg_function(t: &[Tok]) -> Option<Vec<String>> {
    let up = |i: usize| t.get(i).map(|x| x.up.as_str()).unwrap_or("");
    let mut i = 1;
    if up(0) != "CREATE" {
        return None;
    }
    if up(1) == "OR" && up(2) == "REPLACE" {
        i = 3;
    }
    if up(i) != "FUNCTION" {
        return None;
    }
    let (name, mut i) = qname(t, i + 1)?;
    if !(t.get(i)?.kind == K::LParen && t.get(i + 1)?.kind == K::RParen) {
        return None;
    }
    i += 2;
    if !(up(i) == "RETURNS" && up(i + 1) == "TRIGGER") {
        return None;
    }
    i += 2;
    let mut plpgsql = false;
    while i < t.len() {
        match up(i) {
            "LANGUAGE" if up(i + 1) == "PLPGSQL" => {
                plpgsql = true;
                i += 2;
            }
            "VOLATILE" | "AS" => i += 1,
            _ => return None,
        }
    }
    plpgsql.then_some(name)
}

/// `CREATE [OR REPLACE] TRIGGER name AFTER event ON table [REFERENCING
/// {NEW | OLD} TABLE [AS] name …] FOR [EACH] STATEMENT EXECUTE {FUNCTION |
/// PROCEDURE} f()`.
fn pg_trigger(t: &[Tok], node: SyntaxNode) -> Option<PgTrigger> {
    let up = |i: usize| t.get(i).map(|x| x.up.as_str()).unwrap_or("");
    let mut i = 1;
    if up(0) != "CREATE" {
        return None;
    }
    if up(1) == "OR" && up(2) == "REPLACE" {
        i = 3;
    }
    if up(i) != "TRIGGER" {
        return None;
    }
    let (_, j) = qname(t, i + 1)?;
    i = j;
    if up(i) != "AFTER" {
        return None;
    }
    let event = Event::parse(up(i + 1))?;
    i += 2;
    if up(i) != "ON" {
        return None;
    }
    let (table, j) = qname(t, i + 1)?;
    i = j;
    let mut images = Vec::new();
    if up(i) == "REFERENCING" {
        i += 1;
        while matches!(up(i), "NEW" | "OLD") {
            let which = up(i).to_string();
            if up(i + 1) != "TABLE" {
                return None;
            }
            i += 2;
            if up(i) == "AS" {
                i += 1;
            }
            let x = t.get(i).filter(|x| matches!(x.kind, K::Word | K::Quoted))?;
            images.push((which, cst::unquote(&x.text).to_lowercase()));
            i += 1;
        }
    }
    if up(i) != "FOR" {
        return None;
    }
    i += 1;
    if up(i) == "EACH" {
        i += 1;
    }
    if up(i) != "STATEMENT" || up(i + 1) != "EXECUTE" {
        return None;
    }
    if !matches!(up(i + 2), "FUNCTION" | "PROCEDURE") {
        return None;
    }
    let (function, j) = qname(t, i + 3)?;
    if !(t.get(j)?.kind == K::LParen && t.get(j + 1)?.kind == K::RParen && j + 2 == t.len()) {
        return None;
    }
    Some(PgTrigger {
        event,
        table,
        images,
        function,
        node,
    })
}

/// The last name parts agree (ignoring case), and so do the schemas when
/// both name one (T-SQL `dbo` and PostgreSQL `public` are the defaults).
fn same_table(x: &[String], y: &[String]) -> bool {
    let schema = |p: &[String]| {
        (p.len() >= 2)
            .then(|| p[p.len() - 2].to_lowercase())
            .filter(|s| s != "dbo" && s != "public")
    };
    same_last(x, y)
        && match (schema(x), schema(y)) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => x.len() == 1 || y.len() == 1,
        }
}

fn same_name(x: &[String], y: &[String]) -> bool {
    same_last(x, y) && (x.len() == 1 || y.len() == 1 || x.eq_ignore_ascii_case(y))
}

fn same_last(x: &[String], y: &[String]) -> bool {
    match (x.last(), y.last()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

trait EqIgnoreCase {
    fn eq_ignore_ascii_case(&self, o: &Self) -> bool;
}

impl EqIgnoreCase for [String] {
    fn eq_ignore_ascii_case(&self, o: &Self) -> bool {
        self.len() == o.len() && self.iter().zip(o).all(|(a, b)| a.eq_ignore_ascii_case(b))
    }
}

/// PostgreSQL fires a trigger again for the statements its own function runs
/// against its table, where T-SQL (RECURSIVE_TRIGGERS OFF) does not. That is
/// harmless when the firing it causes does nothing: the only writes to the
/// table are UPDATEs, and the UPDATE trigger's function, which those UPDATEs
/// fire, starts with the empty-image guard and holds nothing but UPDATEs of
/// the table that each require a column none of the writes assigns to differ
/// between a row of `inserted` and its own row of `deleted` (matched on a
/// whole key no write assigns). These columns are stored ones on both sides:
/// a computed or generated column changes with the columns it reads. A call
/// in any of the functions may write the table out of sight: not proven. Fired by such a write, the images hold the
/// rows it changed, whose other columns did not change, so every UPDATE
/// matches no row; the UPDATE that changes no row fires the trigger once
/// more, with empty images, and the guard returns.
fn reentry(b: &Analyzed, table: &[String], events: &[Event], pairs: &[Pair], out: &mut Outcome) {
    let own = |target: &[String]| same_table(table, target);
    let fires = |e: Event| events.contains(&e);
    // A called routine may write the table, which this proof cannot see.
    if let Some((node, _)) = pairs.iter().find_map(|(_, ub, _, _)| ub.calls.first()) {
        out.unknown_reasons.push(format!(
            "{}the trigger calls a routine, which may write its table {}: PostgreSQL fires the table's trigger again for that statement and T-SQL does not (RECURSIVE_TRIGGERS OFF); no proof covers it",
            place(b, node),
            table.join(".")
        ));
        return;
    }
    let mut writes: Vec<&Stmt> = Vec::new();
    let mut first: Option<&SyntaxNode> = None;
    let mut other: Option<&SyntaxNode> = None;
    for (_, ub, _, _) in pairs {
        for s in &ub.stmts {
            match &s.ast {
                Ok(st @ Stmt::Update { target, .. }) if own(target) => {
                    writes.push(st);
                    first.get_or_insert(&s.node);
                }
                // An INSERT fires the INSERT trigger, a DELETE the DELETE
                // trigger, a MERGE any of them.
                Ok(Stmt::Insert { target, .. }) if own(target) && fires(Event::Insert) => {
                    other.get_or_insert(&s.node);
                }
                Ok(Stmt::Delete { target, .. }) if own(target) && fires(Event::Delete) => {
                    other.get_or_insert(&s.node);
                }
                Ok(Stmt::Merge { target, .. }) if own(target) => {
                    other.get_or_insert(&s.node);
                }
                _ => {}
            }
        }
    }
    if let Some(node) = other {
        out.unknown_reasons.push(format!(
            "{}the trigger writes its own table {} with INSERT, DELETE or MERGE: PostgreSQL fires its trigger again for that statement and T-SQL does not (RECURSIVE_TRIGGERS OFF); no proof covers it",
            place(b, node),
            table.join(".")
        ));
        return;
    }
    let Some(node) = first.filter(|_| fires(Event::Update)) else {
        return;
    };
    let assigned: Vec<String> = writes
        .iter()
        .flat_map(|w| match w {
            Stmt::Update { sets, .. } => sets
                .iter()
                .flat_map(|(cols, _)| cols.iter().map(|c| name_key(c)))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    let Some((ua, ub, sa, sb)) = pairs
        .iter()
        .find(|(u, _, _, _)| u.trigger.as_ref().is_some_and(|t| t.missing.is_none()))
    else {
        return;
    };
    let keys = sb
        .table("inserted")
        .map(|t| t.keys.clone())
        .unwrap_or_default();
    // Columns the database does not compute on either side: a computed or
    // generated column changes when the columns it reads are assigned.
    let stored = |s: &Schema| -> Vec<String> {
        s.table("inserted")
            .map(|t| {
                t.columns
                    .iter()
                    .filter(|c| !c.generated)
                    .map(|c| c.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    let (stored_a, stored_b) = (stored(sa), stored(sb));
    let stored: Vec<String> = stored_b
        .into_iter()
        .filter(|c| stored_a.contains(c))
        .collect();
    let proven = guarded(ub)
        && guarded(ua)
        && ub.uncovered.is_empty()
        && ub.calls.is_empty()
        && !ub.stmts.is_empty()
        && ub.stmts.iter().all(|s| match &s.ast {
            Ok(Stmt::Update {
                target,
                from,
                where_: Some(w),
                ..
            }) if own(target) => inert(from, w, &assigned, &keys, &stored),
            _ => false,
        });
    if proven {
        out.assume(asm::make(REENTRY_OTHERS));
    } else {
        out.unknown_reasons.push(format!(
            "{}the trigger updates its own table {}: PostgreSQL fires its UPDATE trigger again for that statement (and again for each UPDATE that trigger runs, also when it changes no rows), where T-SQL with RECURSIVE_TRIGGERS OFF does not. It is not shown that the second firing does nothing: start the UPDATE trigger with `IF NOT EXISTS (SELECT 1 FROM inserted) THEN RETURN NULL; END IF;` and update only rows where a column the trigger does not set changed (`i.c <> d.c`, with `deleted d` matched to `inserted i` on a key), or add `IF pg_trigger_depth() > 1 THEN RETURN NULL; END IF;` first, which also stops it when another trigger's statement fires it",
            place(b, node),
            table.join(".")
        ));
    }
}

fn name_key(s: &str) -> String {
    cst::unquote(s.trim_matches(['[', ']'])).to_lowercase()
}

/// An UPDATE of the trigger's table that matches no row when fired by one
/// of the trigger's own writes: some conjunct of WHERE is a disjunction of
/// `i.c <> d.c` (or `IS DISTINCT FROM`, or `NOT (i.c = d.c)`) for columns
/// `c` no write assigns, where `i` reads `inserted` and `d` reads `deleted`,
/// matched on every column of a key that no write assigns.
fn inert(
    from: &[From],
    w: &Expr,
    assigned: &[String],
    keys: &[Vec<String>],
    stored: &[String],
) -> bool {
    let mut tables: Vec<(String, String)> = Vec::new();
    let mut ons: Vec<&Expr> = Vec::new();
    fn walk<'e>(f: &'e From, t: &mut Vec<(String, String)>, ons: &mut Vec<&'e Expr>) -> bool {
        match f {
            From::Table { name, alias, .. } => {
                let [n] = name.as_slice() else {
                    t.push((
                        alias.clone().unwrap_or_default().to_lowercase(),
                        String::new(),
                    ));
                    return true;
                };
                let n = name_key(n);
                t.push((
                    alias.as_deref().map(name_key).unwrap_or_else(|| n.clone()),
                    n,
                ));
                true
            }
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                // An outer join can keep a row of `inserted` without its
                // `deleted` row: `<>` with NULL is not true, so that is fine;
                // only an ON that pairs the images counts as a match.
                let ok = walk(left, t, ons) && walk(right, t, ons);
                if let Some(on) = on {
                    if !kind.to_uppercase().contains("FULL") {
                        ons.push(on);
                    }
                }
                ok
            }
            _ => {
                t.push((String::new(), String::new()));
                true
            }
        }
    }
    for f in from {
        if !walk(f, &mut tables, &mut ons) {
            return false;
        }
    }
    let image = |alias: &str| {
        tables
            .iter()
            .find(|(a, _)| a == alias)
            .map(|(_, t)| t.as_str())
    };
    let col = |e: &Expr, img: &str| -> Option<(String, String)> {
        let Expr::Name(p) = e else { return None };
        let [q, c] = p.as_slice() else { return None };
        let q = name_key(q);
        (image(&q) == Some(img)).then(|| (q, name_key(c)))
    };
    let mut conj: Vec<&Expr> = Vec::new();
    and_terms(w, &mut conj);
    let mut matches: Vec<&Expr> = conj.clone();
    for on in &ons {
        and_terms(on, &mut matches);
    }
    // (inserted alias, deleted alias, column) for each `i.c = d.c`.
    let eqs: Vec<(String, String, String)> = matches
        .iter()
        .filter_map(|e| match e {
            Expr::Bin(op, x, y) if op == "=" => {
                let pair = |x: &Expr, y: &Expr| {
                    let (i, c1) = col(x, "inserted")?;
                    let (d, c2) = col(y, "deleted")?;
                    (c1 == c2).then_some((i, d, c1))
                };
                pair(x, y).or_else(|| pair(y, x))
            }
            _ => None,
        })
        .collect();
    let keyed = |i: &str, d: &str| {
        keys.iter().any(|k| {
            !k.is_empty()
                && k.iter().all(|c| {
                    !assigned.contains(c)
                        && stored.contains(c)
                        && eqs.iter().any(|(ei, ed, ec)| ei == i && ed == d && ec == c)
                })
        })
    };
    conj.iter().any(|c| {
        let mut alts: Vec<&Expr> = Vec::new();
        or_terms(c, &mut alts);
        !alts.is_empty()
            && alts.iter().all(|e| {
                let differs = |x: &Expr, y: &Expr| {
                    let pair = |x: &Expr, y: &Expr| {
                        let (i, c1) = col(x, "inserted")?;
                        let (d, c2) = col(y, "deleted")?;
                        (c1 == c2
                            && !assigned.contains(&c1)
                            && stored.contains(&c1)
                            && keyed(&i, &d))
                        .then_some(())
                    };
                    pair(x, y).or_else(|| pair(y, x)).is_some()
                };
                match e {
                    Expr::Bin(op, x, y) if op == "<>" || op == "!=" => differs(x, y),
                    Expr::Bin(op, x, y) if op.eq_ignore_ascii_case("IS DISTINCT FROM") => {
                        differs(x, y)
                    }
                    Expr::Unary(op, inner) if op.eq_ignore_ascii_case("NOT") => {
                        matches!(&**inner, Expr::Bin(op, x, y) if op == "=" && differs(x, y))
                    }
                    _ => false,
                }
            })
    })
}

fn and_terms<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match e {
        Expr::Bin(op, x, y) if op.eq_ignore_ascii_case("AND") => {
            and_terms(x, out);
            and_terms(y, out);
        }
        Expr::Tuple(v) if v.len() == 1 => and_terms(&v[0], out),
        _ => out.push(e),
    }
}

fn or_terms<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match e {
        Expr::Bin(op, x, y) if op.eq_ignore_ascii_case("OR") => {
            or_terms(x, out);
            or_terms(y, out);
        }
        Expr::Tuple(v) if v.len() == 1 => or_terms(&v[0], out),
        _ => out.push(e),
    }
}
