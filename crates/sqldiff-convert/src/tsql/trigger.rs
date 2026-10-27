//! T-SQL AFTER triggers → PostgreSQL statement triggers (rules T1-T3).
//!
//! A T-SQL trigger fires once per statement and sees the statement's rows in
//! the pseudo-tables `inserted` (new images) and `deleted` (old images).
//! PostgreSQL's counterpart is a `FOR EACH STATEMENT` AFTER trigger whose
//! transition tables are named `inserted` and `deleted` by `REFERENCING`.
//! PostgreSQL allows transition tables only on a trigger with one event, so
//! `FOR INSERT, UPDATE` becomes one function and one trigger per event, and
//! only the images an event has may be declared (NEW TABLE for INSERT and
//! UPDATE, OLD TABLE for UPDATE and DELETE). An image the event lacks, which
//! T-SQL gives as an empty table, becomes `(SELECT * FROM t WHERE false)`
//! (T2): the same columns and types, no rows.
//!
//! Checked on SQL Server 2022 and PostgreSQL 18 (2026-10-27, project files
//! eval/triggers/): both fire a statement trigger for a statement that
//! changes no rows; `SET NOCOUNT ON` resets `@@ROWCOUNT` to 0; a MERGE runs
//! T-SQL's INSERT trigger with the MERGE's row count and an empty
//! `inserted`, and PostgreSQL's INSERT statement trigger with an empty
//! transition table; an error in a T-SQL trigger ends the batch and dooms
//! the transaction, in PostgreSQL it fails the statement; a T-SQL trigger
//! that changes its own table does not fire itself again (RECURSIVE_TRIGGERS
//! is OFF by default), a PostgreSQL one does, also for zero rows.
//!
//! The engine does not compare triggers yet, so every converted trigger is
//! "not proven" until it does.

use super::*;

/// Why transaction control is refused in a trigger.
pub(super) const TXN: &str = "transaction control in a trigger: T-SQL's ROLLBACK there ends the whole transaction and the batch, which a PostgreSQL trigger function cannot do; raise an error instead";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    Insert,
    Update,
    Delete,
}

impl Event {
    fn word(self) -> &'static str {
        match self {
            Event::Insert => "INSERT",
            Event::Update => "UPDATE",
            Event::Delete => "DELETE",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Event::Insert => "_insert",
            Event::Update => "_update",
            Event::Delete => "_delete",
        }
    }

    /// The image the event has no rows for in T-SQL and no transition
    /// table in PostgreSQL.
    fn missing(self) -> Option<&'static str> {
        match self {
            Event::Insert => Some("deleted"),
            Event::Update => None,
            Event::Delete => Some("inserted"),
        }
    }

    /// The image whose rows are the statement's rows.
    fn rows(self) -> &'static str {
        match self {
            Event::Delete => "deleted",
            _ => "inserted",
        }
    }
}

/// The trigger a body is converted for.
#[derive(Debug, Clone)]
pub(super) struct Trig {
    /// The table's name parts, unquoted and lower case.
    pub table: Vec<String>,
    /// The table as the output names it.
    pub out: String,
    pub event: Event,
    /// A statement of the body was seen (the row-count guard must be first).
    pub started: bool,
}

/// The parts of a trigger header.
struct Header {
    alter: bool,
    /// Function name: schema part (with its dot) and last part, as output.
    schema: String,
    name: String,
    table: Vec<String>,
    table_out: String,
    events: Vec<Event>,
}

impl Cx<'_> {
    /// `CREATE TRIGGER name ON table {FOR|AFTER} events AS body` (rule T1).
    pub(super) fn trigger(&mut self, r: &SyntaxNode) -> R<Option<String>> {
        if self.db2() {
            return no("triggers: Db2 for i triggers are row or statement triggers with REFERENCING NEW/OLD (TABLE); inserted/deleted need a rewrite");
        }
        let all = tokens(r);
        let body = r.children().find(|c| c.kind() == ROUTINE_BODY).ok_or(No(
            "trigger without a body: the statement is cut short or malformed".into(),
        ))?;
        let body_at = all.len() - tokens(&body).len();
        let h = self.trigger_header(&all[..body_at])?;
        let body_toks = tokens(&body);
        let bs = sig(&body_toks);
        for (k, &i) in bs.iter().enumerate() {
            let t = &body_toks[i];
            let call = bs.get(k + 1).is_some_and(|&j| body_toks[j].k == L_PAREN);
            if t.is("COLUMNS_UPDATED")
                || t.is("UPDATE") && call && !(k > 0 && body_toks[bs[k - 1]].is("ON"))
            {
                return no("UPDATE(column) / COLUMNS_UPDATED(): PostgreSQL statement triggers do not say which columns the statement set; compare inserted with deleted instead, or keep a separate trigger with UPDATE OF (which then has no transition tables)");
            }
            if t.is("TRIGGER_NESTLEVEL") || t.is("EVENTDATA") || t.is("@@NESTLEVEL") {
                return no(format!(
                    "{}: no PostgreSQL counterpart in a trigger (pg_trigger_depth() counts differently)",
                    t.up()
                ));
            }
        }
        let header_notes = self.take_notes();
        let single = h.events.len() == 1;
        let mut text = String::new();
        let mut notes: Vec<Note> = Vec::new();
        for &ev in &h.events {
            let suffix = if single { "" } else { ev.suffix() };
            let (fname, tname) = trigger_names(&h.schema, &h.name, suffix)?;
            self.reset();
            self.ret_bit = false;
            self.return_code = false;
            self.txn_control = false;
            self.dynamic = dynamic_sql_vars(&all);
            self.mode = Mode::Func;
            self.trigger = Some(Trig {
                table: h.table.clone(),
                out: h.table_out.clone(),
                event: ev,
                started: false,
            });
            let mut b = Out::default();
            let block = body.children().find(|c| c.kind() == BLOCK);
            match block {
                Some(blk) if body.children().count() == 1 => {
                    self.body_elements(&mut b, &blk, INDENT)
                }
                _ => self.body_elements(&mut b, &body, INDENT),
            }
            let mut decls = Out::default();
            self.write_decls(&mut decls);
            self.trigger = None;
            self.mode = Mode::Top;
            let leftover = self.take_notes();
            for n in b
                .units
                .iter()
                .chain(&decls.units)
                .flat_map(|u| u.notes.iter())
                .chain(&leftover)
            {
                if n.status == Status::NotConverted {
                    // A partly converted trigger would fail every statement on
                    // the table: refuse the whole trigger.
                    return Err(No(n.text.clone()));
                }
                if !notes.iter().any(|x| x.rule == n.rule) {
                    notes.push(n.clone());
                }
            }
            let images = match ev {
                Event::Insert => "NEW TABLE AS inserted",
                Event::Update => "NEW TABLE AS inserted OLD TABLE AS deleted",
                Event::Delete => "OLD TABLE AS deleted",
            };
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "CREATE OR REPLACE FUNCTION {fname}() RETURNS trigger\nLANGUAGE plpgsql\nAS $$\n{}BEGIN\n{}  RETURN NULL;\nEND;\n$$;\n\nCREATE OR REPLACE TRIGGER {tname}\nAFTER {} ON {}\nREFERENCING {images}\nFOR EACH STATEMENT\nEXECUTE FUNCTION {fname}();\n",
                decls.text,
                b.text,
                ev.word(),
                h.table_out,
            ));
            if self.writes_own_table(&body, &h)
                && !notes.iter().any(|x| x.rule == "pg.trigger-reentry")
            {
                notes.push(Note { status: Status::Decision, rule: "pg.trigger-reentry".into(), text: format!(
                    "The trigger changes its own table {}: PostgreSQL fires it again for that statement (and for every statement that changes no rows), where T-SQL does not with RECURSIVE_TRIGGERS OFF, the default. Unless the trigger stops when it has nothing to do, it fires until the stack depth limit. Make sure the second firing does nothing, or add `IF pg_trigger_depth() > 1 THEN RETURN NULL; END IF;` first, which also stops it when another trigger's statement fires it.",
                    h.table_out
                ) });
            }
        }
        self.notes = header_notes;
        for n in notes {
            if !self.notes.iter().any(|x| x.rule == n.rule) {
                self.notes.push(n);
            }
        }
        if !single {
            let text = format!(
                "PostgreSQL allows transition tables only on a trigger with one event, so the trigger becomes one function and one trigger per event ({}), named with the event's suffix.",
                h.events.iter().map(|e| e.word()).collect::<Vec<_>>().join(", ")
            );
            self.note(Status::Converted, "pg.trigger-per-event", &text);
        }
        self.note(Status::Assumptions, "ASM-TRIGGER-ERROR", "An error in the trigger (THROW, RAISERROR of severity 11 or more, a failing statement) ends the T-SQL batch and dooms the transaction, as XACT_ABORT ON does; in PostgreSQL it fails the statement that fired the trigger, and a caller that catches the error (an EXCEPTION block) carries on. Equal for callers that do not catch it.");
        self.note(Status::Assumptions, "ASM-TRIGGER-ORDER", "Several AFTER triggers for one event on a table: T-SQL fires them in an unspecified order (unless sp_settriggerorder sets the first and last), PostgreSQL in name order. Equal if the triggers do not depend on each other's effects.");
        if h.alter {
            self.note(Status::Assumptions, "ASM-TRIGGER-REPLACE", "ALTER TRIGGER becomes CREATE OR REPLACE FUNCTION and CREATE OR REPLACE TRIGGER: a PostgreSQL trigger for an event the T-SQL trigger no longer has stays; drop it first.");
        }
        Ok(Some(text))
    }

    fn trigger_header(&mut self, ts: &[T]) -> R<Header> {
        let s = sig(ts);
        let up = |k: usize| s.get(k).map(|&i| ts[i].up()).unwrap_or_default();
        let kw = (0..s.len())
            .find(|&k| up(k) == "TRIGGER")
            .ok_or(No("trigger without TRIGGER".into()))?;
        let alter = (0..kw).any(|k| up(k) == "ALTER");
        let on = (kw + 1..s.len())
            .find(|&k| up(k) == "ON")
            .ok_or(No("trigger without ON".into()))?;
        if on == kw + 1 {
            return no("trigger without a name");
        }
        if matches!(up(on + 1).as_str(), "DATABASE" | "ALL") {
            return no("DDL triggers (ON DATABASE / ON ALL SERVER): PostgreSQL has event triggers, with other events and no rule here");
        }
        let name_toks = &ts[s[kw + 1]..=s[on - 1]];
        name_parts(name_toks).ok_or(No("trigger name not understood".into()))?;
        let tend = (on + 1..s.len())
            .find(|&k| matches!(up(k).as_str(), "WITH" | "FOR" | "AFTER" | "INSTEAD"))
            .ok_or(No("trigger without FOR, AFTER or INSTEAD OF".into()))?;
        if tend == on + 1 {
            return no("trigger without a table");
        }
        let table_toks = &ts[s[on + 1]..=s[tend - 1]];
        let table = name_parts(table_toks).ok_or(No("trigger table not understood".into()))?;
        let mut k = tend;
        if up(k) == "WITH" {
            let end = (k + 1..s.len())
                .find(|&j| matches!(up(j).as_str(), "FOR" | "AFTER" | "INSTEAD"))
                .ok_or(No("trigger options not understood".into()))?;
            for opt in split_commas(&ts[s[k] + 1..s[end]]) {
                let words: Vec<String> = sig(opt).iter().map(|&j| opt[j].up()).collect();
                match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                    ["ENCRYPTION"] => self.note(Status::Converted, "tsql.routine-options-dropped", "WITH ENCRYPTION was dropped: it does not change results."),
                    _ => return no(format!("trigger option WITH {}: no rule (EXECUTE AS changes whose privileges the statements use)", words.join(" "))),
                }
            }
            k = end;
        }
        match up(k).as_str() {
            "INSTEAD" => return no(
                "INSTEAD OF triggers: PostgreSQL allows them on views only, and per row; no rule",
            ),
            "FOR" | "AFTER" => k += 1,
            _ => return no("trigger without FOR or AFTER"),
        }
        let mut events = Vec::new();
        while k < s.len() {
            match up(k).as_str() {
                "INSERT" => events.push(Event::Insert),
                "UPDATE" => events.push(Event::Update),
                "DELETE" => events.push(Event::Delete),
                _ if ts[s[k]].k == COMMA => {}
                _ => break,
            }
            k += 1;
        }
        events.dedup();
        if events.is_empty() {
            return no("trigger without events");
        }
        // Order as T-SQL lists them does not matter; keep insert, update, delete.
        events.sort_by_key(|e| *e as u8);
        events.dedup();
        if up(k) == "WITH" && up(k + 1) == "APPEND" {
            k += 2;
        }
        if up(k) == "NOT" && up(k + 1) == "FOR" && up(k + 2) == "REPLICATION" {
            self.note(Status::Decision, "tsql.routine-options", "NOT FOR REPLICATION was dropped: the T-SQL trigger does not fire for replication agents; a PostgreSQL trigger fires for logical replication only when enabled with ENABLE REPLICA or ENABLE ALWAYS.");
            k += 3;
        }
        if up(k) != "AS" || k + 1 != s.len() {
            return no("trigger header not understood");
        }
        let fname = self.expr(name_toks).trim().to_string();
        let (schema, name) = match split_last(&fname) {
            Some((a, b)) => (format!("{a}."), b.to_string()),
            None => (String::new(), fname.clone()),
        };
        let table_out = self.expr(table_toks).trim().to_string();
        Ok(Header {
            alter,
            schema,
            name,
            table,
            table_out,
            events,
        })
    }

    /// `IF @@ROWCOUNT = 0 RETURN` as the first statement of a trigger body
    /// (rule T3): the statement changed no rows, so its image is empty. Only
    /// first: any statement before it (SET NOCOUNT ON too) resets
    /// @@ROWCOUNT, and later ones refuse @@ROWCOUNT as usual.
    pub(super) fn trigger_guard(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) -> bool {
        let Some(t) = self.trigger.as_mut() else {
            return false;
        };
        if std::mem::replace(&mut t.started, true) {
            return false;
        }
        let ev = t.event;
        if n.kind() != IF_STMT {
            return false;
        }
        let ts = tokens(n);
        let words: Vec<String> = sig(&ts)
            .into_iter()
            .filter(|&i| !matches!(ts[i].k, L_PAREN | R_PAREN | SEMICOLON))
            .map(|i| ts[i].up())
            .collect();
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        let guard = matches!(
            w.as_slice(),
            ["IF", "@@ROWCOUNT", "=", "0", "RETURN"]
                | ["IF", "@@ROWCOUNT", "<", "1", "RETURN"]
                | ["IF", "@@ROWCOUNT", "=", "0", "BEGIN", "RETURN", "END"]
                | ["IF", "@@ROWCOUNT", "<", "1", "BEGIN", "RETURN", "END"]
        );
        if !guard {
            return false;
        }
        self.note(Status::Assumptions, "ASM-TRIGGER-ROWCOUNT", "IF @@ROWCOUNT = 0 RETURN, first in the trigger, becomes a test for rows in the statement's image. They differ when a MERGE fires the trigger: T-SQL's @@ROWCOUNT counts the rows of every MERGE action, so the trigger runs with an empty image, where PostgreSQL returns. Equal unless the rest of the trigger does something when its image is empty.");
        let notes = self.take_notes();
        out.unit(
            Self::range(n),
            indent,
            &format!(
                "IF NOT EXISTS (SELECT 1 FROM {}) THEN\n  RETURN NULL;\nEND IF;",
                ev.rows()
            ),
            notes,
        );
        true
    }

    /// The image the trigger's event lacks, named at `ts[i]` as a FROM
    /// item: `(SELECT * FROM t WHERE false)`, with the image's name as its
    /// alias when it has none (rule T2).
    pub(super) fn missing_image(&self, ts: &[T], i: usize) -> Option<String> {
        let t = self.trigger.as_ref()?;
        let name = t.event.missing()?;
        let tok = &ts[i];
        if !(tok.is(name)
            || tok.k == QUOTED_IDENT && tsql_unquote(&tok.s).eq_ignore_ascii_case(name))
        {
            return None;
        }
        if prev_sig(ts, i).is_some_and(|p| ts[p].k == DOT)
            || next_sig(ts, i).is_some_and(|n| matches!(ts[n].k, DOT | L_PAREN))
        {
            return None;
        }
        let aliased = next_sig(ts, i).is_some_and(|n| {
            let x = &ts[n];
            x.is("AS")
                || x.k == QUOTED_IDENT
                || x.k == IDENT && !sqldiff_core::normalize::is_keyword(&x.up())
        });
        let sub = format!("(SELECT * FROM {} WHERE false)", t.out);
        Some(if aliased {
            sub
        } else {
            format!("{sub} AS {name}")
        })
    }

    /// In a trigger, `inserted` and `deleted` name the trigger's table for
    /// its columns and keys.
    pub(super) fn image_table<'n>(&'n self, name: &'n [String]) -> &'n [String] {
        match (&self.trigger, name) {
            (Some(t), [n]) if n == "inserted" || n == "deleted" => &t.table,
            _ => name,
        }
    }

    /// Whether a statement of the body changes the trigger's own table by an
    /// event the T-SQL trigger has: T-SQL does not fire the trigger again,
    /// PostgreSQL fires the trigger of that event.
    fn writes_own_table(&self, body: &SyntaxNode, h: &Header) -> bool {
        let own = |parts: &[String]| {
            parts.last() == h.table.last()
                && (parts.len() == 1
                    || h.table.len() == 1 && parts.len() == 2 && parts[0] == "dbo"
                    || parts.len() >= 2
                        && h.table.len() >= 2
                        && parts[parts.len() - 2] == h.table[h.table.len() - 2])
        };
        for st in body.descendants().filter(|n| n.kind() == STATEMENT) {
            let ts = tokens(&st);
            let s = sig(&ts);
            let Some(&first) = s.first() else { continue };
            let verb = ts[first].up();
            let fires: &[Event] = match verb.as_str() {
                "INSERT" => &[Event::Insert],
                "UPDATE" => &[Event::Update],
                "DELETE" => &[Event::Delete],
                "MERGE" => &[Event::Insert, Event::Update, Event::Delete],
                _ => continue,
            };
            if !fires.iter().any(|e| h.events.contains(e)) {
                continue;
            }
            // The target: after the verb (and INTO / FROM).
            let mut k = 1;
            if s.get(k)
                .is_some_and(|&j| ts[j].is("INTO") || ts[j].is("FROM"))
            {
                k += 1;
            }
            let Some(&at) = s.get(k) else { continue };
            let mut end = at;
            while let Some(d) = next_sig(&ts, end).filter(|&d| ts[d].k == DOT) {
                match next_sig(&ts, d) {
                    Some(x) => end = x,
                    None => break,
                }
            }
            let Some(target) = name_parts(&ts[at..=end]) else {
                continue;
            };
            if own(&target) {
                return true;
            }
            // `UPDATE x … FROM t x`: the alias of a FROM table.
            if target.len() == 1 {
                if let Some(scope) = scope_of(&ts) {
                    if scope.entries.iter().any(|e| {
                        e.named && e.alias.as_deref() == Some(target[0].as_str()) && own(&e.name)
                    }) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// `a.b."c d"` → (`a.b`, `"c d"`): the text before and after the last dot
/// outside quotes.
fn split_last(name: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut last = None;
    for (i, c) in name.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => last = Some(i),
            _ => {}
        }
    }
    last.map(|i| (&name[..i], &name[i + 1..]))
}

/// The function name (with the trigger's schema) and the trigger name for
/// one event; PostgreSQL names are at most 63 bytes.
fn trigger_names(schema: &str, name: &str, suffix: &str) -> R<(String, String)> {
    let last = match name.strip_suffix('"') {
        Some(inner) => format!("{inner}{suffix}\""),
        None => format!("{name}{suffix}"),
    };
    if last.trim_matches('"').len() > 63 {
        return no(format!(
            "the trigger name {last} is longer than PostgreSQL's 63 bytes"
        ));
    }
    Ok((format!("{schema}{last}"), last))
}
