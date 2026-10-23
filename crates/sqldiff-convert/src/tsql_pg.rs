//! T-SQL → PostgreSQL rules (P0: token level over the lossless CST).
//!
//! Each rule either converts faithfully, converts and records the assumption
//! it needs (a note), or refuses, in which case the unit is kept as a comment
//! and marked not converted. Behavioural checks are left to the engine.

use std::collections::HashMap;

use sqldiff_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use SyntaxKind::*;

use crate::out::Out;
use crate::tok::{
    find_word, matching, next_sig, prev_sig, sig, snake, split_commas, tokens, trimmed, T,
};
use crate::{Note, Status};

const INDENT: usize = 2;

pub(crate) fn convert(root: &SyntaxNode, src: &str) -> Out {
    let mut cx = Cx {
        src,
        vars: HashMap::new(),
        notes: Vec::new(),
        mode: Mode::Top,
        in_catch: false,
        return_code: false,
        decls: Vec::new(),
    };
    let mut out = Out::default();
    for el in root.children_with_tokens() {
        match el {
            SyntaxElement::Node(n) if n.kind() == BATCH => cx.batch(&mut out, &n),
            SyntaxElement::Node(n) if n.kind().is_unit() => cx.unit(&mut out, &n, 0),
            SyntaxElement::Token(t) if matches!(t.kind(), LINE_COMMENT | BLOCK_COMMENT) => {
                out.lines(0, t.text())
            }
            SyntaxElement::Token(t)
                if t.kind() == WHITESPACE
                    && t.text().contains("\n\n")
                    && !out.text.is_empty()
                    && !out.text.ends_with("\n\n") =>
            {
                out.push("\n");
            }
            _ => {}
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Plain SQL script.
    Top,
    /// Body of a procedure.
    Proc,
    /// Body of a scalar function.
    Func,
    /// A T-SQL script with variables or control flow, run as a `DO` block.
    Do,
}

#[derive(Debug, Clone)]
struct Var {
    name: String,
    string: bool,
    numeric: bool,
}

/// A hoisted declaration: the DECLARE statement's span and the lines it adds.
struct Decl {
    src: (usize, usize),
    lines: Vec<String>,
    notes: Vec<Note>,
}

struct Cx<'a> {
    src: &'a str,
    vars: HashMap<String, Var>,
    /// Notes of the unit being converted.
    notes: Vec<Note>,
    mode: Mode,
    in_catch: bool,
    /// A procedure returns a value somewhere: it gets an INOUT `return_code`.
    return_code: bool,
    decls: Vec<Decl>,
}

/// A refusal: the unit is kept as a comment with this reason.
struct No(String);

type R<T> = Result<T, No>;

fn no<T>(reason: impl Into<String>) -> R<T> {
    Err(No(reason.into()))
}

impl Cx<'_> {
    fn note(&mut self, status: Status, rule: &str, text: &str) {
        if !self.notes.iter().any(|n| n.rule == rule) {
            self.notes.push(Note {
                status,
                rule: rule.to_string(),
                text: text.to_string(),
            });
        }
    }

    fn take_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    fn range(n: &SyntaxNode) -> (usize, usize) {
        sqldiff_syntax::ast::sig_range(n)
    }

    /// The original text of a node as comment lines, with the reason first.
    fn commented(&self, n: &SyntaxNode, reason: &str) -> String {
        let (s, e) = Self::range(n);
        let mut t = format!("-- [not converted] {reason}\n");
        for line in dedent(&self.src[s..e], column(self.src, s)).lines() {
            t.push_str("-- ");
            t.push_str(line);
            t.push('\n');
        }
        t
    }

    fn refuse(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize, reason: &str) {
        let text = self.commented(n, reason);
        self.notes.clear();
        out.unit(
            Self::range(n),
            indent,
            &text,
            vec![Note {
                status: Status::NotConverted,
                rule: "not-converted".into(),
                text: reason.to_string(),
            }],
        );
    }

    // ----- batches, routines, bodies -------------------------------------------

    fn batch(&mut self, out: &mut Out, b: &SyntaxNode) {
        let units: Vec<SyntaxNode> = b.children().filter(|c| c.kind().is_unit()).collect();
        if let Some(r) = units.iter().find(|u| u.kind() == ROUTINE) {
            self.routine(out, r);
            return;
        }
        if units.iter().any(procedural) {
            self.do_block(out, b);
            return;
        }
        self.mode = Mode::Top;
        self.body_elements(out, b, 0);
    }

    fn do_block(&mut self, out: &mut Out, b: &SyntaxNode) {
        self.mode = Mode::Do;
        self.vars.clear();
        self.decls.clear();
        let mut body = Out::default();
        self.body_elements(&mut body, b, INDENT);
        out.push("DO $$\n");
        self.write_decls(out);
        out.push("BEGIN\n");
        out.append(body);
        out.push("END\n$$;\n");
        self.mode = Mode::Top;
    }

    fn write_decls(&mut self, out: &mut Out) {
        if self.decls.is_empty() {
            return;
        }
        out.push("DECLARE\n");
        for d in std::mem::take(&mut self.decls) {
            out.unit(d.src, INDENT, &d.lines.join("\n"), d.notes);
        }
    }

    fn routine(&mut self, out: &mut Out, r: &SyntaxNode) {
        let ts = tokens(r);
        let s = sig(&ts);
        let words: Vec<String> = s.iter().take(4).map(|&i| ts[i].up()).collect();
        let kind = words
            .iter()
            .find(|w| ["PROC", "PROCEDURE", "FUNCTION", "VIEW", "TRIGGER"].contains(&w.as_str()))
            .cloned()
            .unwrap_or_default();
        let res = match kind.as_str() {
            "PROC" | "PROCEDURE" => self.proc_or_func(out, r, false),
            "FUNCTION" => self.proc_or_func(out, r, true),
            "VIEW" => self.view(r),
            "TRIGGER" => no("triggers: PostgreSQL needs a trigger function plus CREATE TRIGGER, and inserted/deleted become NEW/OLD or transition tables"),
            _ => no("unrecognised routine"),
        };
        match res {
            Ok(Some(_)) if self.blocked().is_some() => {
                let reason = self.blocked().unwrap();
                self.mode = Mode::Top;
                self.refuse(out, r, 0, &reason)
            }
            Ok(Some(text)) => {
                let notes = self.take_notes();
                out.unit(Self::range(r), 0, &text, notes);
            }
            Ok(None) => {}
            Err(No(reason)) => {
                self.mode = Mode::Top;
                self.refuse(out, r, 0, &reason)
            }
        }
    }

    fn view(&mut self, r: &SyntaxNode) -> R<Option<String>> {
        let ts = tokens(r);
        let body = r
            .children()
            .find(|c| c.kind() == ROUTINE_BODY)
            .ok_or(No("view without a body".into()))?;
        let name_at = ts.iter().position(|t| t.is("VIEW")).unwrap();
        let as_at = (name_at..ts.len())
            .find(|&i| ts[i].is("AS"))
            .ok_or(No("view without AS".into()))?;
        let head = &ts[name_at + 1..as_at];
        if let Some(w) = head.iter().find(|t| t.is("WITH")) {
            let _ = w;
            self.note(
                Status::Converted,
                "tsql.view-options",
                "View options such as SCHEMABINDING have no PostgreSQL equivalent and were dropped; they do not change results.",
            );
        }
        let name_end = head.iter().position(|t| t.is("WITH")).unwrap_or(head.len());
        let name = self.expr(&head[..name_end]).trim().to_string();
        if ts.iter().take(name_at).any(|t| t.is("ALTER")) {
            self.note(
                Status::Assumptions,
                "ASM-VIEW-REPLACE",
                "ALTER VIEW becomes CREATE OR REPLACE VIEW, which only works if the new column list starts with the old columns.",
            );
        }
        self.mode = Mode::Top;
        let stmt = body
            .children()
            .find(|c| c.kind() == STATEMENT)
            .ok_or(No("view body is not a query".into()))?;
        let q = self.select(&tokens(&stmt))?;
        Ok(Some(format!("CREATE OR REPLACE VIEW {name} AS\n{q};")))
    }

    fn proc_or_func(&mut self, out: &mut Out, r: &SyntaxNode, func: bool) -> R<Option<String>> {
        self.vars.clear();
        self.decls.clear();
        self.return_code = false;
        let all = tokens(r);
        let body_at = all.len()
            - r.children()
                .find(|c| c.kind() == ROUTINE_BODY)
                .map_or(0, |b| tokens(&b).len());
        let ts = &all[..body_at];
        let kw = ts
            .iter()
            .position(|t| t.is("PROC") || t.is("PROCEDURE") || t.is("FUNCTION"))
            .unwrap();
        // Name: up to the parameters, RETURNS, WITH or AS.
        let mut i = kw + 1;
        let mut name_toks = Vec::new();
        while i < ts.len() {
            let t = &ts[i];
            if t.k == VARIABLE || t.k == L_PAREN || t.is("AS") || t.is("RETURNS") || t.is("WITH") {
                break;
            }
            name_toks.push(t.clone());
            i += 1;
        }
        let name = self.expr(&name_toks).trim().to_string();
        // Parameters: up to RETURNS (functions) or a top-level WITH / AS.
        let stop = (i..ts.len())
            .find(|&j| {
                crate::tok::depths(&ts[i..])[j - i] == 0
                    && (ts[j].is("RETURNS") || ts[j].is("AS") || ts[j].is("WITH"))
            })
            .unwrap_or(ts.len());
        let mut ptoks = trimmed(&ts[i..stop]).to_vec();
        if ptoks.first().is_some_and(|t| t.k == L_PAREN)
            && matching(&ptoks, 0) == Some(ptoks.len() - 1)
        {
            ptoks = ptoks[1..ptoks.len() - 1].to_vec();
        }
        let mut params = Vec::new();
        let mut seen_default = false;
        for p in split_commas(&ptoks) {
            let p = trimmed(p);
            let Some(v) = p.first().filter(|t| t.k == VARIABLE) else {
                return no("parameter list not understood");
            };
            let ty_end = (1..p.len())
                .find(|&j| {
                    p[j].k == OP && p[j].s == "="
                        || p[j].is("OUTPUT")
                        || p[j].is("OUT")
                        || p[j].is("READONLY")
                })
                .unwrap_or(p.len());
            if p.iter().any(|t| t.is("READONLY")) {
                return no("table-valued parameters (READONLY) have no PostgreSQL equivalent; pass an array or a temp table");
            }
            let (ty, string, numeric) = self.ty(&p[1..ty_end])?;
            let pname = format!("p_{}", snake(&v.s[1..]));
            self.vars.insert(
                v.s[1..].to_ascii_lowercase(),
                Var {
                    name: pname.clone(),
                    string,
                    numeric,
                },
            );
            let output = p.iter().any(|t| t.is("OUTPUT") || t.is("OUT"));
            if output && func {
                return no("OUTPUT parameters on a function");
            }
            let mut text = format!("{}{pname} {ty}", if output { "INOUT " } else { "" });
            if let Some(eq) = p[..].iter().position(|t| t.k == OP && t.s == "=") {
                let end = (eq + 1..p.len())
                    .find(|&j| p[j].is("OUTPUT") || p[j].is("OUT"))
                    .unwrap_or(p.len());
                text.push_str(&format!(" DEFAULT {}", self.expr(&p[eq + 1..end]).trim()));
                seen_default = true;
            } else if seen_default {
                self.note(
                    Status::Decision,
                    "pg.param-default-order",
                    "PostgreSQL requires every parameter after one with a default to have a default too; reorder the parameters or add defaults.",
                );
            }
            params.push(text);
        }
        let mut returns = String::new();
        if func {
            let ret = ts
                .iter()
                .position(|t| t.is("RETURNS"))
                .ok_or(No("function without RETURNS".into()))?;
            let rend = (ret + 1..ts.len())
                .find(|&j| ts[j].is("AS") || ts[j].is("WITH"))
                .unwrap_or(ts.len());
            let rt = trimmed(&ts[ret + 1..rend]);
            if rt.first().is_some_and(|t| t.is("TABLE") || t.k == VARIABLE) {
                return no(
                    "table-valued functions: convert to RETURNS TABLE (…) with RETURN QUERY",
                );
            }
            let (ty, _, _) = self.ty(rt)?;
            returns = format!(" RETURNS {ty}");
        }
        if ts.iter().any(|t| t.is("WITH")) {
            self.note(
                Status::Decision,
                "tsql.routine-options",
                "Routine options (WITH EXECUTE AS, RECOMPILE, ENCRYPTION, …) were dropped; use SECURITY DEFINER for EXECUTE AS OWNER.",
            );
        }
        let header_notes = self.take_notes();

        // Body.
        self.mode = if func { Mode::Func } else { Mode::Proc };
        let body = r.children().find(|c| c.kind() == ROUTINE_BODY).unwrap();
        let mut b = Out::default();
        let block = body.children().find(|c| c.kind() == BLOCK);
        match block {
            Some(blk) if body.children().count() == 1 => self.body_elements(&mut b, &blk, INDENT),
            _ => self.body_elements(&mut b, &body, INDENT),
        }
        if self.return_code {
            params.push("INOUT return_code integer DEFAULT NULL".into());
        }
        self.notes = header_notes;
        if self.return_code {
            self.note(
                Status::Assumptions,
                "ASM-RETURN-CODE",
                "PostgreSQL procedures cannot return a value: RETURN n becomes `return_code := n` on an INOUT parameter, so callers must read return_code.",
            );
        }
        let header = format!(
            "CREATE OR REPLACE {} {name}({}){returns}\nLANGUAGE plpgsql\nAS $$",
            if func { "FUNCTION" } else { "PROCEDURE" },
            params.join(", ")
        );
        let (s, _) = Self::range(r);
        let notes = self.take_notes();
        out.unit((s, body.text_range().start().into()), 0, &header, notes);
        self.write_decls(out);
        out.push("BEGIN\n");
        out.append(b);
        out.push("END;\n$$;\n");
        self.mode = Mode::Top;
        Ok(None)
    }

    /// Writes the units and comments directly inside `n`, skipping its own keywords.
    fn body_elements(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        for el in n.children_with_tokens() {
            match el {
                SyntaxElement::Node(c) if c.kind().is_unit() => self.unit(out, &c, indent),
                SyntaxElement::Node(c) if c.kind().is_container() => {
                    self.body_elements(out, &c, indent)
                }
                SyntaxElement::Token(t) if matches!(t.kind(), LINE_COMMENT | BLOCK_COMMENT) => {
                    out.lines(indent, t.text())
                }
                _ => {}
            }
        }
    }

    fn unit(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        self.notes.clear();
        match n.kind() {
            STATEMENT => match self.stmt(n) {
                Ok(_) if self.blocked().is_some() => {
                    let reason = self.blocked().unwrap();
                    self.refuse(out, n, indent, &reason)
                }
                Ok(text) => {
                    let notes = self.take_notes();
                    out.unit(Self::range(n), indent, &text, notes);
                }
                Err(No(reason)) => self.refuse(out, n, indent, &reason),
            },
            BLOCK => self.body_elements(out, n, indent),
            IF_STMT if self.mode != Mode::Top => self.if_stmt(out, n, indent, false),
            LOOP_STMT if self.mode != Mode::Top => self.while_stmt(out, n, indent),
            TRY_CATCH if self.mode != Mode::Top => self.try_catch(out, n, indent),
            ROUTINE => self.routine(out, n),
            _ => self.refuse(out, n, indent, "no rule for this construct yet"),
        }
    }

    fn condition(&mut self, n: &SyntaxNode) -> String {
        n.children()
            .find(|c| c.kind() == CONDITION)
            .map(|c| self.expr(&tokens(&c)).trim().to_string())
            .unwrap_or_default()
    }

    fn header_unit(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize, text: &str) {
        let s = Self::range(n).0;
        let e = n
            .children()
            .find(|c| c.kind() == CONDITION)
            .map_or(s, |c| c.text_range().end().into());
        let notes = self.take_notes();
        out.unit((s, e.max(s + 1)), indent, text, notes);
    }

    /// The reason of the first not-converted note, if the unit has one.
    fn blocked(&self) -> Option<String> {
        self.notes
            .iter()
            .find(|n| n.status == Status::NotConverted)
            .map(|n| n.text.clone())
    }

    fn if_stmt(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize, elsif: bool) {
        let cond = self.condition(n);
        if let Some(reason) = self.blocked() {
            if elsif {
                out.lines(indent, "ELSE");
                return self.refuse(out, n, indent + INDENT, &reason);
            }
            return self.refuse(out, n, indent, &reason);
        }
        let kw = if elsif { "ELSIF" } else { "IF" };
        self.header_unit(out, n, indent, &format!("{kw} {cond} THEN"));
        let branches: Vec<SyntaxNode> = n.children().filter(|c| c.kind() == BRANCH).collect();
        if let Some(b) = branches.first() {
            self.body_elements(out, b, indent + INDENT);
        }
        if let Some(e) = branches.get(1) {
            let inner: Vec<SyntaxNode> = e.children().filter(|c| c.kind().is_unit()).collect();
            if inner.len() == 1 && inner[0].kind() == IF_STMT {
                self.if_stmt(out, &inner[0], indent, true);
            } else {
                out.lines(indent, "ELSE");
                self.body_elements(out, e, indent + INDENT);
            }
        }
        if !elsif {
            out.lines(indent, "END IF;");
        }
    }

    fn while_stmt(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        let ts = tokens(n);
        if !ts.iter().find(|t| t.sig()).is_some_and(|t| t.is("WHILE")) {
            return self.refuse(out, n, indent, "loop form not supported");
        }
        let cond = self.condition(n);
        if let Some(reason) = self.blocked() {
            return self.refuse(out, n, indent, &reason);
        }
        self.header_unit(out, n, indent, &format!("WHILE {cond} LOOP"));
        for b in n.children().filter(|c| c.kind() == BRANCH) {
            self.body_elements(out, &b, indent + INDENT);
        }
        out.lines(indent, "END LOOP;");
    }

    fn try_catch(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        let try_b = n.children().find(|c| c.kind() == TRY_BLOCK);
        let catch_b = n.children().find(|c| c.kind() == CATCH_BLOCK);
        let (Some(try_b), Some(catch_b)) = (try_b, catch_b) else {
            return self.refuse(out, n, indent, "TRY without CATCH");
        };
        let s = Self::range(n).0;
        // Idiom: CATCH that only rolls back and rethrows = let the error propagate.
        if rollback_rethrow(&catch_b) {
            self.note(
                Status::Assumptions,
                "idiom.rollback-rethrow",
                "CATCH only rolls back and rethrows, so the error simply propagates: PostgreSQL aborts the caller's transaction on the error, as T-SQL does with ROLLBACK + THROW (assumes the caller does not catch it and continue).",
            );
            let notes = self.take_notes();
            out.unit(
                (s, s + 9),
                indent,
                "-- BEGIN TRY … CATCH: ROLLBACK; THROW → error propagates",
                notes,
            );
            self.body_elements(out, &try_b, indent);
            let (cs, ce) = Self::range(&catch_b);
            out.unit(
                (cs, ce),
                indent,
                "-- (CATCH removed: see above)",
                Vec::new(),
            );
            return;
        }
        self.note(
            Status::Assumptions,
            "ASM-TRY-SUBTRANSACTION",
            "On an error, PostgreSQL undoes everything the TRY part did before running the handler (the block is a subtransaction); T-SQL keeps the statements that completed unless XACT_ABORT is ON or the CATCH rolls back.",
        );
        let notes = self.take_notes();
        out.unit((s, s + 9), indent, "BEGIN", notes);
        self.body_elements(out, &try_b, indent + INDENT);
        out.lines(indent, "EXCEPTION WHEN OTHERS THEN");
        let was = std::mem::replace(&mut self.in_catch, true);
        self.body_elements(out, &catch_b, indent + INDENT);
        self.in_catch = was;
        out.lines(indent, "END;");
    }

    // ----- statements -----------------------------------------------------------

    fn stmt(&mut self, n: &SyntaxNode) -> R<String> {
        let ts = tokens(n);
        let s = sig(&ts);
        let w = |k: usize| s.get(k).map(|&i| ts[i].up()).unwrap_or_default();
        let ind = column(self.src, Self::range(n).0);
        let text = match w(0).as_str() {
            "SELECT" | "WITH" => {
                let q = self.select(&ts)?;
                if self.mode != Mode::Top && !q.contains(" INTO ") && !q.contains(":=") {
                    return no("returns a result set: a PostgreSQL procedure cannot; use a function RETURNS TABLE, a refcursor, or PERFORM if the rows are not needed");
                }
                q
            }
            "INSERT" | "UPDATE" | "DELETE" => self.dml(&ts)?,
            "MERGE" => return no("MERGE: T-SQL and PostgreSQL MERGE differ (OUTPUT, BY SOURCE, concurrency); planned with the upsert canonical form"),
            "SET" => self.set(&ts)?,
            "DECLARE" => self.declare(&ts, n)?,
            "PRINT" => {
                let e = self.expr(&ts[s[1]..]);
                format!("RAISE NOTICE '%', {}", e.trim())
            }
            "RETURN" => self.ret(&ts)?,
            "BREAK" => "EXIT".into(),
            "CONTINUE" => "CONTINUE".into(),
            "THROW" => self.throw(&ts)?,
            "RAISERROR" => return no("RAISERROR: severity decides whether it is an error at all; rewrite as RAISE EXCEPTION or RAISE NOTICE"),
            "BEGIN" if matches!(w(1).as_str(), "TRAN" | "TRANSACTION") => self.txn("BEGIN")?,
            "COMMIT" => self.txn("COMMIT")?,
            "ROLLBACK" => self.txn("ROLLBACK")?,
            "SAVE" => return no("SAVE TRANSACTION: use SAVEPOINT, but a PL/pgSQL body cannot; use a nested BEGIN … EXCEPTION block"),
            "EXEC" | "EXECUTE" => self.exec(&ts)?,
            "CREATE" => self.create(&ts)?,
            "ALTER" if w(1) == "TABLE" => self.alter_table(&ts)?,
            "DROP" => {
                if w(1) == "INDEX" && find_word(&ts, s[1], "ON").is_some() {
                    return no("DROP INDEX … ON table: PostgreSQL index names are per schema; drop by name");
                }
                self.expr(&ts)
            }
            "TRUNCATE" => self.expr(&ts),
            "USE" => return no("USE: connect to the other database instead; PostgreSQL cannot switch databases in a session"),
            _ => return no("no rule for this statement yet"),
        };
        let text = dedent(text.trim_end(), ind);
        Ok(
            if text.trim_start().starts_with("--") || text.ends_with(';') {
                text
            } else {
                format!("{text};")
            },
        )
    }

    /// The statement without its terminator.
    fn body_toks(ts: &[T]) -> Vec<T> {
        let mut v: Vec<T> = trimmed(ts).to_vec();
        while v.last().is_some_and(|t| t.k == SEMICOLON || !t.sig()) {
            v.pop();
        }
        v
    }

    fn select(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let ds = crate::tok::depths(&ts);
        let s = sig(&ts);
        if ts[s[0]].is("WITH") {
            if ts.iter().any(|t| t.is("TOP")) {
                return no("TOP inside a CTE query");
            }
            return Ok(self.expr(&ts));
        }
        let mut i = 1; // index into s
        let mut limit = None;
        let mut cut = Vec::new(); // token indices removed
        if s.get(1)
            .is_some_and(|&j| ts[j].is("DISTINCT") || ts[j].is("ALL"))
        {
            i = 2;
        }
        if let Some(&top) = s.get(i).filter(|&&j| ts[j].is("TOP")) {
            let arg = *s.get(i + 1).ok_or(No("TOP without a count".into()))?;
            let (end, lim) = if ts[arg].k == L_PAREN {
                let r = matching(&ts, arg).ok_or(No("unbalanced TOP".into()))?;
                (r, format!("({})", self.expr(&ts[arg + 1..r]).trim()))
            } else {
                (arg, self.expr(&ts[arg..=arg]))
            };
            if next_sig(&ts, end).is_some_and(|j| ts[j].is("PERCENT") || ts[j].is("WITH")) {
                return no("TOP … PERCENT / WITH TIES");
            }
            cut.extend(top..=end);
            if next_sig(&ts, end).is_some() {
                // also drop whitespace after the count
                let nx = next_sig(&ts, end).unwrap();
                cut.extend(end + 1..nx);
            }
            limit = Some(lim);
            i += 2;
        }
        if ts
            .iter()
            .enumerate()
            .any(|(j, t)| t.is("TOP") && !cut.contains(&j))
        {
            return no("TOP inside a subquery: rewrite with LIMIT in the subquery");
        }
        if limit.is_some()
            && (0..ts.len()).any(|j| {
                ds[j] == 0 && (ts[j].is("UNION") || ts[j].is("EXCEPT") || ts[j].is("INTERSECT"))
            })
        {
            return no("TOP with UNION/EXCEPT/INTERSECT");
        }
        let from = find_word(&ts, 0, "FROM");
        let option = find_word(&ts, 0, "OPTION");
        if let Some(o) = option {
            self.note(
                Status::Converted,
                "tsql.query-hints",
                "OPTION (…) query hints were dropped; they affect plans, not results.",
            );
            if let Some(r) = next_sig(&ts, o).and_then(|p| matching(&ts, p)) {
                cut.extend(o..=r);
            }
        }
        // INTO a new table.
        if let Some(into) = find_word(&ts, 0, "INTO") {
            if !ts[next_sig(&ts, into).unwrap_or(into)].s.starts_with('@') {
                return no("SELECT … INTO new_table: use CREATE TABLE … AS SELECT");
            }
        }
        // Assignment: SELECT @a = e, @b = f [FROM …]
        let list_start = s.get(i).copied().unwrap_or(ts.len());
        let list_end = from.unwrap_or_else(|| option.unwrap_or(ts.len()));
        let items = split_commas(&ts[list_start..list_end]);
        let assigns: Vec<bool> = items
            .iter()
            .map(|it| {
                let t: Vec<&T> = it.iter().filter(|t| t.sig()).collect();
                t.len() > 1 && t[0].k == VARIABLE && t[1].k == OP && t[1].s == "="
            })
            .collect();
        if assigns.iter().any(|&a| a) {
            if !assigns.iter().all(|&a| a) {
                return no("SELECT mixes variable assignment and result columns");
            }
            let mut targets = Vec::new();
            let mut exprs = Vec::new();
            for it in &items {
                let t = trimmed(it);
                let eq = t.iter().position(|x| x.k == OP && x.s == "=").unwrap();
                targets.push(self.var_ref(&t[0].s));
                exprs.push(self.expr(&t[eq + 1..]).trim().to_string());
            }
            if from.is_none() && limit.is_none() {
                return Ok(targets
                    .iter()
                    .zip(&exprs)
                    .map(|(t, e)| format!("{t} := {e};"))
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
            self.note(
                Status::Assumptions,
                "ASM-SELECT-ASSIGN-CARDINALITY",
                "The query returns at most one row. With several rows T-SQL keeps the last row's values and PostgreSQL the first; with none T-SQL keeps the old values and PostgreSQL sets NULL.",
            );
            let rest: Vec<T> = ts[list_end..]
                .iter()
                .enumerate()
                .filter(|(k, _)| !cut.contains(&(k + list_end)))
                .map(|(_, t)| t.clone())
                .collect();
            let mut q = format!(
                "SELECT {} INTO {} {}",
                exprs.join(", "),
                targets.join(", "),
                self.expr(&rest).trim()
            );
            if let Some(l) = limit {
                q.push_str(&format!(" LIMIT {l}"));
            }
            return Ok(q.trim_end().to_string());
        }
        let kept: Vec<T> = ts
            .iter()
            .enumerate()
            .filter(|(k, _)| !cut.contains(k))
            .map(|(_, t)| t.clone())
            .collect();
        let mut q = self.expr(&kept).trim_end().to_string();
        if let Some(l) = limit {
            q.push_str(&format!("\nLIMIT {l}"));
        }
        Ok(q)
    }

    fn dml(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if find_word(&ts, 0, "OUTPUT").is_some() {
            return no(
                "OUTPUT clause: use RETURNING (inserted.x → the new row, deleted.x needs a CTE)",
            );
        }
        if ts[s[1]].is("TOP") {
            return no(
                "TOP in INSERT/UPDATE/DELETE: restrict the rows with a key subquery and LIMIT",
            );
        }
        if find_word(&ts, 0, "EXEC").is_some() || find_word(&ts, 0, "EXECUTE").is_some() {
            return no("INSERT … EXEC");
        }
        let mut pre = String::new();
        let mut from = s[1];
        match ts[s[0]].up().as_str() {
            "INSERT" if !ts[s[1]].is("INTO") => pre = "INSERT INTO ".into(),
            "UPDATE" => {
                let set = find_word(&ts, 0, "SET").unwrap_or(0);
                if find_word(&ts, set, "FROM").is_some() {
                    return no("UPDATE … FROM with a join: PostgreSQL's UPDATE … FROM must not repeat the target, and with several matching rows both pick one unpredictably; needs a rewrite");
                }
                from = s[0];
            }
            "DELETE" => {
                if !ts[s[1]].is("FROM") {
                    if find_word(&ts, s[1], "FROM").is_some() {
                        return no("DELETE t FROM … JOIN: rewrite with USING or a key subquery");
                    }
                    pre = "DELETE FROM ".into();
                } else {
                    let first_from = s[1];
                    if find_word(&ts, first_from + 1, "FROM").is_some() {
                        return no(
                            "DELETE FROM t FROM … JOIN: rewrite with USING or a key subquery",
                        );
                    }
                    from = s[0];
                }
            }
            _ => from = s[0],
        }
        if pre.is_empty() {
            return Ok(self.expr(&ts));
        }
        Ok(format!("{pre}{}", self.expr(&ts[from..]).trim_start()))
    }

    fn set(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let a = &ts[s[1]];
        if a.k == VARIABLE && !a.s.starts_with("@@") {
            let op = &ts[*s.get(2).ok_or(No("SET without a value".into()))?];
            let target = self.var_ref(&a.s);
            let rhs = self.expr(&ts[s[3]..]).trim().to_string();
            return match op.s.as_str() {
                "=" => Ok(format!("{target} := {rhs}")),
                "+=" | "-=" | "*=" | "/=" => {
                    let o = if op.s == "+=" && self.is_string_var(&a.s) {
                        "||".to_string()
                    } else {
                        op.s[..1].to_string()
                    };
                    Ok(format!("{target} := {target} {o} {rhs}"))
                }
                _ => no("compound assignment operator"),
            };
        }
        let opt = a.up();
        let val = s.get(2).map(|&j| ts[j].up()).unwrap_or_default();
        match (opt.as_str(), val.as_str()) {
            ("NOCOUNT", _) => Ok(format!("-- SET NOCOUNT {val}: no PostgreSQL equivalent needed")),
            ("XACT_ABORT", "ON") => {
                self.note(Status::Assumptions, "ASM-XACT-ABORT", "PostgreSQL always aborts the transaction on an error, which matches XACT_ABORT ON for the statements that follow.");
                Ok("-- SET XACT_ABORT ON: PostgreSQL always behaves this way".into())
            }
            ("XACT_ABORT", _) => no("XACT_ABORT OFF: PostgreSQL cannot continue a transaction after an error; use BEGIN … EXCEPTION blocks around the statements that may fail"),
            ("ANSI_NULLS" | "QUOTED_IDENTIFIER" | "ANSI_PADDING" | "ANSI_WARNINGS" | "ARITHABORT" | "CONCAT_NULL_YIELDS_NULL", "ON") => {
                Ok(format!("-- SET {opt} ON: PostgreSQL always behaves this way"))
            }
            ("TRANSACTION", _) if self.mode == Mode::Top => {
                if ts.iter().any(|t| t.is("UNCOMMITTED")) {
                    self.note(Status::Assumptions, "ASM-READ-UNCOMMITTED", "PostgreSQL runs READ UNCOMMITTED as READ COMMITTED: dirty reads never happen.");
                }
                Ok(self.expr(&ts))
            }
            _ => no(format!("SET {opt}: no rule for this session option")),
        }
    }

    fn declare(&mut self, ts: &[T], n: &SyntaxNode) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if s.len() > 2 && (ts[s[2]].is("CURSOR") || ts[s[1]].is("CURSOR")) {
            return no("cursors: declare a PL/pgSQL cursor (or use a FOR loop over the query)");
        }
        let mut lines = Vec::new();
        let mut inits = Vec::new();
        for item in split_commas(&ts[s[1]..]) {
            let it = trimmed(item);
            let Some(v) = it.first().filter(|t| t.k == VARIABLE) else {
                return no("DECLARE form not understood");
            };
            let mut j = 1;
            if it.get(j).is_some_and(|t| t.is("AS")) {
                j += 1;
            }
            let it2: Vec<T> = it[j..].iter().filter(|t| t.sig()).cloned().collect();
            if it2.first().is_some_and(|t| t.is("TABLE")) {
                return no("table variables: use a temporary table (note: it does not survive ROLLBACK the way a table variable does)");
            }
            let eq = it.iter().position(|t| t.k == OP && t.s == "=");
            let (ty, string, numeric) = self.ty(&it[j..eq.unwrap_or(it.len())])?;
            let name = format!("v_{}", snake(&v.s[1..]));
            self.vars.insert(
                v.s[1..].to_ascii_lowercase(),
                Var {
                    name: name.clone(),
                    string,
                    numeric,
                },
            );
            lines.push(format!("{name} {ty};"));
            if let Some(eq) = eq {
                inits.push(format!("{name} := {};", self.expr(&it[eq + 1..]).trim()));
            }
        }
        let notes = self.take_notes();
        self.decls.push(Decl {
            src: Self::range(n),
            lines,
            notes: notes.clone(),
        });
        self.notes = notes;
        if inits.is_empty() {
            Ok(format!(
                "-- DECLARE {}: moved to the DECLARE section",
                split_commas(&ts[s[1]..])
                    .iter()
                    .filter_map(|it| trimmed(it).first().map(|t| t.s.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        } else {
            Ok(inits.join("\n"))
        }
    }

    fn ret(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let val = s.get(1).map(|&j| self.expr(&ts[j..]).trim().to_string());
        match (self.mode, val) {
            (Mode::Func, Some(v)) => Ok(format!("RETURN {v}")),
            (Mode::Proc, Some(v)) => {
                self.return_code = true;
                Ok(format!("return_code := {v};\nRETURN;"))
            }
            (Mode::Func, None) => no("RETURN without a value in a function"),
            (_, None) => Ok("RETURN".into()),
            (_, Some(_)) => no("RETURN with a value outside a procedure"),
        }
    }

    fn throw(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if s.len() == 1 {
            if self.in_catch {
                return Ok("RAISE".into());
            }
            return no("THROW without arguments outside CATCH");
        }
        let args = split_commas(&ts[s[1]..]);
        if args.len() != 3 {
            return no("THROW form not understood");
        }
        let msg = self.expr(args[1]).trim().to_string();
        self.note(
            Status::Assumptions,
            "ASM-THROW-ERRCODE",
            "The T-SQL error number is not carried over: callers see SQLSTATE P0001 with the same message.",
        );
        Ok(format!(
            "RAISE EXCEPTION USING MESSAGE = {msg}, ERRCODE = 'P0001'"
        ))
    }

    fn txn(&mut self, kw: &str) -> R<String> {
        match self.mode {
            Mode::Top => {
                if kw == "BEGIN" {
                    self.note(Status::Assumptions, "ASM-NO-NESTED-TRAN", "PostgreSQL transactions do not nest: a second BEGIN only warns, where T-SQL increments @@TRANCOUNT.");
                }
                Ok(kw.to_string())
            }
            Mode::Func => no("transaction control inside a function is not allowed in PostgreSQL"),
            Mode::Proc | Mode::Do => {
                if self.in_catch {
                    return no("ROLLBACK/COMMIT inside an exception handler is not allowed in PostgreSQL; the failed block is already rolled back");
                }
                if kw == "BEGIN" {
                    self.note(Status::Decision, "pg.proc-transaction", "A PostgreSQL procedure already runs inside a transaction; BEGIN TRAN is dropped. Nested-transaction behaviour (@@TRANCOUNT) does not exist.");
                    return Ok("-- BEGIN TRANSACTION: implicit in PostgreSQL".into());
                }
                self.note(Status::Decision, "pg.proc-transaction", "COMMIT/ROLLBACK in a procedure only works when it is CALLed outside an explicit transaction block. In T-SQL a COMMIT inside the caller's transaction only decrements @@TRANCOUNT; here it commits the caller's work too.");
                Ok(kw.to_string())
            }
        }
    }

    fn exec(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let first = ts
            .get(*s.get(1).ok_or(No("EXEC without a target".into()))?)
            .unwrap();
        if first.k == L_PAREN || first.k == VARIABLE {
            return no("dynamic SQL / EXEC @rc = …: use EXECUTE format(…) after review; dynamic SQL is opaque to the checker");
        }
        if ts.iter().any(|t| t.is("OUTPUT") || t.is("OUT")) {
            return no("EXEC with OUTPUT arguments: CALL with INOUT parameters into variables");
        }
        // name: dotted run of identifiers
        let mut j = s[1];
        let mut name_end = j + 1;
        while let Some(d) = next_sig(&ts, name_end - 1).filter(|&d| ts[d].k == DOT) {
            j = next_sig(&ts, d).ok_or(No("bad name".into()))?;
            name_end = j + 1;
        }
        let raw_name: String = ts[s[1]..name_end].iter().map(|t| t.s.clone()).collect();
        if raw_name.rsplit('.').next().is_some_and(|n| {
            n.to_ascii_lowercase().starts_with("sp_") || n.to_ascii_lowercase().starts_with("xp_")
        }) {
            return no("system procedures have no PostgreSQL equivalent");
        }
        let name = self.expr(&ts[s[1]..name_end]).trim().to_string();
        let mut args = Vec::new();
        for a in split_commas(&ts[name_end..]) {
            let a = trimmed(a);
            let eq = a.iter().position(|x| x.k == OP && x.s == "=");
            let named = a.first().is_some_and(|x| x.k == VARIABLE)
                && eq.is_some_and(|e| a[1..e].iter().all(|x| !x.sig()));
            if named {
                let eq = eq.unwrap();
                self.note(Status::Assumptions, "ASM-PARAM-NAMES", "Named arguments assume the called procedure was converted with the same p_ parameter names.");
                args.push(format!(
                    "p_{} => {}",
                    snake(&a[0].s[1..]),
                    self.expr(&a[eq + 1..]).trim()
                ));
            } else {
                args.push(self.expr(a).trim().to_string());
            }
        }
        Ok(format!("CALL {name}({})", args.join(", ")))
    }

    fn create(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let w1 = ts[s[1]].up();
        match w1.as_str() {
            "TABLE" => self.create_table(&ts),
            "UNIQUE" | "INDEX" | "CLUSTERED" | "NONCLUSTERED" => {
                let ts = self.strip_storage(&ts);
                let kept: Vec<T> = ts
                    .iter()
                    .filter(|t| !(t.is("CLUSTERED") || t.is("NONCLUSTERED")))
                    .cloned()
                    .collect();
                if ts.iter().any(|t| t.is("CLUSTERED")) {
                    self.note(Status::Assumptions, "ASM-CLUSTERED", "PostgreSQL has no clustered indexes: this becomes a plain index (CLUSTER can reorder the table once). Results are the same; performance may differ.");
                }
                Ok(collapse_spaces(&self.expr(&kept)))
            }
            "SCHEMA" => Ok(self.expr(&ts)),
            _ => no(format!("CREATE {w1}: no rule yet")),
        }
    }

    /// Removes `ON [PRIMARY]`, `TEXTIMAGE_ON x` and `WITH (…)` after the main column list.
    fn strip_storage(&mut self, ts: &[T]) -> Vec<T> {
        let ds = crate::tok::depths(ts);
        let mut cut = vec![false; ts.len()];
        let mut i = 0;
        let mut dropped = false;
        while i < ts.len() {
            let after_paren = prev_sig(ts, i).is_some_and(|p| ts[p].k == R_PAREN);
            if ds[i] == 0 && after_paren && (ts[i].is("ON") || ts[i].is("TEXTIMAGE_ON")) {
                let nx = next_sig(ts, i).unwrap_or(i);
                cut[i..=nx].iter_mut().for_each(|c| *c = true);
                dropped = true;
                i = nx + 1;
                continue;
            }
            if ds[i] == 0 && after_paren && ts[i].is("WITH") {
                if let Some(r) = next_sig(ts, i).and_then(|p| matching(ts, p)) {
                    cut[i..=r].iter_mut().for_each(|c| *c = true);
                    dropped = true;
                    i = r + 1;
                    continue;
                }
            }
            i += 1;
        }
        if dropped {
            self.note(Status::Converted, "tsql.storage-options", "Filegroup and storage options (ON [PRIMARY], WITH (…)) were dropped; they do not change results.");
        }
        ts.iter()
            .zip(cut)
            .filter(|(_, c)| !c)
            .map(|(t, _)| t.clone())
            .collect()
    }

    fn create_table(&mut self, ts: &[T]) -> R<String> {
        let ts = self.strip_storage(ts);
        let open = ts
            .iter()
            .position(|t| t.k == L_PAREN)
            .ok_or(No("CREATE TABLE without columns".into()))?;
        let close = matching(&ts, open).ok_or(No("unbalanced parentheses".into()))?;
        let name_t: Vec<T> = ts[..open]
            .iter()
            .skip_while(|t| !t.is("TABLE"))
            .skip(1)
            .cloned()
            .collect();
        let temp = name_t.iter().any(|t| t.s.starts_with('#'));
        let name = self.expr(&name_t).trim().to_string();
        let mut cols = Vec::new();
        for item in split_commas(&ts[open + 1..close]) {
            cols.push(self.table_item(item)?);
        }
        let tail = self.expr(&ts[close + 1..]);
        Ok(format!(
            "CREATE {}TABLE {name} ({}\n){}",
            if temp { "TEMP " } else { "" },
            cols.join(","),
            tail.trim_end()
        ))
    }

    /// One column definition or table constraint, keeping its leading layout.
    fn table_item(&mut self, item: &[T]) -> R<String> {
        let lead: String = item
            .iter()
            .take_while(|t| !t.sig())
            .map(|t| t.s.clone())
            .collect();
        let lead = if lead.contains('\n') {
            format!("\n{}", lead.rsplit('\n').next().unwrap_or(""))
        } else {
            "\n    ".to_string()
        };
        let it = trimmed(item);
        let first = &it[0];
        if ["CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK"]
            .iter()
            .any(|w| first.is(w))
        {
            let kept: Vec<T> = it
                .iter()
                .filter(|t| !(t.is("CLUSTERED") || t.is("NONCLUSTERED")))
                .cloned()
                .collect();
            return Ok(format!("{lead}{}", collapse_spaces(&self.expr(&kept))));
        }
        if first.is("INDEX") {
            return no(
                "inline INDEX in CREATE TABLE: create the index with a separate CREATE INDEX",
            );
        }
        Ok(format!("{lead}{}", self.column_def(it)?))
    }

    fn column_def(&mut self, it: &[T]) -> R<String> {
        let s = sig(it);
        let name = self.expr(&it[s[0]..=s[0]]);
        let Some(&ty_at) = s.get(1) else {
            return no("column without a type");
        };
        if it[ty_at].is("AS") {
            return no(
                "computed column: use GENERATED ALWAYS AS (…) STORED after checking the expression",
            );
        }
        let ty_end = if next_sig(it, ty_at).is_some_and(|j| it[j].k == L_PAREN) {
            matching(it, next_sig(it, ty_at).unwrap()).ok_or(No("unbalanced type".into()))?
        } else {
            ty_at
        };
        let (ty, _, _) = self.ty(&it[ty_at..=ty_end])?;
        let mut rest: Vec<T> = Vec::new();
        let mut gen = String::new();
        let mut j = ty_end + 1;
        while j < it.len() {
            let t = &it[j];
            if t.is("IDENTITY") {
                let mut seed = ("1".to_string(), "1".to_string());
                if let Some(p) = next_sig(it, j).filter(|&p| it[p].k == L_PAREN) {
                    let r = matching(it, p).ok_or(No("unbalanced IDENTITY".into()))?;
                    let a = split_commas(&it[p + 1..r]);
                    if a.len() == 2 {
                        seed = (self.expr(a[0]).trim().into(), self.expr(a[1]).trim().into());
                    }
                    j = r + 1;
                } else {
                    j += 1;
                }
                gen = format!(
                    " GENERATED ALWAYS AS IDENTITY (START WITH {} INCREMENT BY {})",
                    seed.0, seed.1
                );
                self.note(Status::Assumptions, "ASM-IDENTITY", "IDENTITY becomes GENERATED ALWAYS AS IDENTITY: explicit values need OVERRIDING SYSTEM VALUE, as T-SQL needs IDENTITY_INSERT. Values used by rolled-back inserts are lost in both.");
                continue;
            }
            if t.is("CLUSTERED") || t.is("NONCLUSTERED") || t.is("ROWGUIDCOL") {
                j += 1;
                continue;
            }
            if t.is("COLLATE") {
                return no("column COLLATE: choose a PostgreSQL collation (a nondeterministic ICU collation for case-insensitive)");
            }
            rest.push(t.clone());
            j += 1;
        }
        let mut rest = collapse_spaces(&self.expr(&rest));
        if ty == "boolean" {
            for (from, to) in [
                ("((1))", "TRUE"),
                ("((0))", "FALSE"),
                ("(1)", "TRUE"),
                ("(0)", "FALSE"),
                ("1", "TRUE"),
                ("0", "FALSE"),
            ] {
                rest = replace_word(&rest, &format!("DEFAULT {from}"), &format!("DEFAULT {to}"));
            }
        }
        let rest = rest.trim();
        Ok(format!(
            "{name} {ty}{gen}{}{rest}",
            if rest.is_empty() { "" } else { " " }
        ))
    }

    fn alter_table(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        if let Some(add) = find_word(&ts, 0, "ADD") {
            let nx = next_sig(&ts, add).ok_or(No("ADD without a column".into()))?;
            if ["CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK"]
                .iter()
                .any(|w| ts[nx].is(w))
            {
                return Ok(self.expr(&ts));
            }
            let head = self.expr(&ts[..nx]);
            let cols: R<Vec<String>> = split_commas(&ts[nx..])
                .iter()
                .map(|c| {
                    self.column_def(trimmed(c))
                        .map(|d| format!("ADD COLUMN {d}"))
                })
                .collect();
            let head = head
                .trim_end()
                .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                .trim_end();
            return Ok(format!("{head} {}", cols?.join(", ")));
        }
        if find_word(&ts, 0, "ALTER").is_some_and(|i| i > 0) {
            return no("ALTER COLUMN: PostgreSQL uses ALTER COLUMN … TYPE / SET NOT NULL as separate actions");
        }
        Ok(self.expr(&ts))
    }

    // ----- expressions ----------------------------------------------------------

    fn is_string_var(&self, v: &str) -> bool {
        self.vars
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
            .is_some_and(|x| x.string)
    }

    fn var_ref(&mut self, v: &str) -> String {
        if v.starts_with("@@") {
            let up = v.to_ascii_uppercase();
            let reason = match up.as_str() {
                "@@ROWCOUNT" => "@@ROWCOUNT: use GET DIAGNOSTICS n = ROW_COUNT right after the statement (or FOUND)",
                "@@ERROR" => "@@ERROR: PostgreSQL stops at the error instead; use BEGIN … EXCEPTION",
                "@@TRANCOUNT" => "@@TRANCOUNT: PostgreSQL transactions do not nest",
                "@@IDENTITY" => "@@IDENTITY: use INSERT … RETURNING",
                _ => "T-SQL global variable with no PostgreSQL equivalent",
            };
            self.note(
                Status::NotConverted,
                &format!("tsql.{}", up.trim_start_matches('@').to_ascii_lowercase()),
                reason,
            );
            return v.to_string();
        }
        let key = v[1..].to_ascii_lowercase();
        if let Some(x) = self.vars.get(&key) {
            return x.name.clone();
        }
        self.note(Status::Assumptions, "ASM-UNDECLARED-VAR", "A variable is used without a declaration in this unit; it was renamed with the v_ prefix.");
        format!("v_{}", snake(&v[1..]))
    }

    fn operand_kind(&self, ts: &[T], j: Option<usize>) -> (bool, bool) {
        let Some(j) = j else { return (false, false) };
        let t = &ts[j];
        match t.k {
            STRING => (true, false),
            NUMBER => (false, true),
            VARIABLE => self
                .vars
                .get(&t.s.trim_start_matches('@').to_ascii_lowercase())
                .map_or((false, false), |v| (v.string, v.numeric)),
            _ => (false, false),
        }
    }

    fn expr(&mut self, ts: &[T]) -> String {
        let mut o = String::new();
        let mut i = 0;
        while i < ts.len() {
            let t = &ts[i];
            match t.k {
                INFERRED_TERMINATOR => {}
                VARIABLE => {
                    let r = self.var_ref(&t.s);
                    o.push_str(&r)
                }
                QUOTED_IDENT => o.push_str(&quoted(&t.s)),
                STRING => {
                    let lit = t.s.strip_prefix(['N', 'n']).unwrap_or(&t.s);
                    o.push_str(lit);
                    self.string_compare(ts, i);
                }
                OP if t.s == "+" => {
                    let l = self.operand_kind(ts, prev_sig(ts, i));
                    let r = self.operand_kind(ts, next_sig(ts, i));
                    let unary = prev_sig(ts, i).map_or(true, |p| {
                        matches!(ts[p].k, OP | L_PAREN | COMMA)
                            || ts[p].k == IDENT && is_clause_word(&ts[p].s)
                    });
                    if !unary && (l.0 || r.0) {
                        self.note(Status::Assumptions, "ASM-CONCAT-NULL", "String + becomes ||; both give NULL when an operand is NULL (CONCAT_NULL_YIELDS_NULL ON, the default).");
                        o.push_str("||");
                    } else {
                        if !unary && !(l.1 || r.1) {
                            self.note(Status::Assumptions, "ASM-PLUS-NUMERIC", "`+` is kept as addition: the operands are assumed to be numbers. If they are strings, use || instead.");
                        }
                        o.push('+');
                    }
                }
                OP if matches!(t.s.as_str(), "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=") => {
                    o.push_str(&t.s);
                    self.string_compare(ts, i);
                }
                IDENT => {
                    let up = t.up();
                    let nx = next_sig(ts, i);
                    if up == "DBO" && nx.is_some_and(|d| ts[d].k == DOT) {
                        self.note(Status::Assumptions, "ASM-DBO-SCHEMA", "The dbo schema is dropped: objects are assumed to be found through search_path (usually the public schema).");
                        i = nx.unwrap() + 1;
                        continue;
                    }
                    if up == "WITH" && nx.is_some_and(|p| ts[p].k == L_PAREN) {
                        if let Some(r) = self.table_hint(ts, nx.unwrap()) {
                            // drop trailing whitespace before WITH
                            while o.ends_with(' ') {
                                o.pop();
                            }
                            i = r + 1;
                            continue;
                        }
                    }
                    if up == "STRING_AGG" {
                        if let Some((text, end)) = self.string_agg(ts, nx) {
                            o.push_str(&text);
                            i = end + 1;
                            continue;
                        }
                    }
                    if let Some(p) = nx.filter(|&p| ts[p].k == L_PAREN) {
                        if let Some(r) = matching(ts, p) {
                            if let Some(s) = self.func(&up, &ts[p + 1..r]) {
                                o.push_str(&s);
                                i = r + 1;
                                continue;
                            }
                        }
                    }
                    if up == "CURRENT_TIMESTAMP" {
                        o.push_str(&self.now());
                    } else if up == "LIKE" {
                        o.push_str(&t.s);
                        self.string_compare(ts, i);
                    } else if let Some(rest) = t.s.strip_prefix('#') {
                        if rest.starts_with('#') {
                            self.note(
                                Status::NotConverted,
                                "tsql.global-temp",
                                "Global temporary tables (##t) have no PostgreSQL equivalent.",
                            );
                            o.push_str(&t.s);
                        } else {
                            self.note(Status::Assumptions, "ASM-TEMP-TABLE", "#temp tables become TEMP tables: they live until the session ends (T-SQL drops them when the creating procedure ends), so a second CREATE in the same session fails.");
                            o.push_str(rest);
                        }
                    } else {
                        o.push_str(&t.s);
                    }
                }
                _ => o.push_str(&t.s),
            }
            i += 1;
        }
        o
    }

    /// `STRING_AGG(x, sep) [WITHIN GROUP (ORDER BY …)]` → `string_agg(x, sep [ORDER BY …])`.
    fn string_agg(&mut self, ts: &[T], open: Option<usize>) -> Option<(String, usize)> {
        let open = open.filter(|&p| ts[p].k == L_PAREN)?;
        let close = matching(ts, open)?;
        let a = split_commas(&ts[open + 1..close]);
        if a.len() != 2 {
            return None;
        }
        let x = self.expr(a[0]).trim().to_string();
        let sep = self.expr(a[1]).trim().to_string();
        let w = next_sig(ts, close).filter(|&j| ts[j].is("WITHIN"));
        let Some(w) = w else {
            return Some((format!("string_agg({x}, {sep})"), close));
        };
        let g = next_sig(ts, w).filter(|&j| ts[j].is("GROUP"))?;
        let p = next_sig(ts, g).filter(|&j| ts[j].k == L_PAREN)?;
        let r = matching(ts, p)?;
        let order = self.expr(&ts[p + 1..r]).trim().to_string();
        Some((format!("string_agg({x}, {sep} {order})"), r))
    }

    fn string_compare(&mut self, ts: &[T], i: usize) {
        let is_cmp = |t: &T| {
            t.k == OP && matches!(t.s.as_str(), "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=")
                || t.is("LIKE")
        };
        let t = &ts[i];
        let hit = if is_cmp(t) {
            let l = self.operand_kind(ts, prev_sig(ts, i)).0;
            let r = self.operand_kind(ts, next_sig(ts, i)).0;
            l || r || t.is("LIKE")
        } else {
            // a string literal next to a comparison
            prev_sig(ts, i).is_some_and(|p| is_cmp(&ts[p]))
                || next_sig(ts, i).is_some_and(|n| is_cmp(&ts[n]))
        };
        if hit {
            self.note(Status::Assumptions, "ASM-CI-COLLATION", "String comparison: T-SQL's default collation is case-insensitive and ignores trailing spaces; PostgreSQL compares case-sensitively. Equal only if the data's case is consistent (or use a case-insensitive collation).");
        }
    }

    /// `WITH (NOLOCK)` and friends after a table name. Returns the closing paren.
    fn table_hint(&mut self, ts: &[T], open: usize) -> Option<usize> {
        let r = matching(ts, open)?;
        let words: Vec<String> = ts[open + 1..r]
            .iter()
            .filter(|t| t.k == IDENT)
            .map(T::up)
            .collect();
        const READS: &[&str] = &[
            "NOLOCK",
            "READUNCOMMITTED",
            "READCOMMITTED",
            "READCOMMITTEDLOCK",
            "NOWAIT",
            "INDEX",
            "FORCESEEK",
        ];
        const LOCKS: &[&str] = &[
            "UPDLOCK",
            "HOLDLOCK",
            "ROWLOCK",
            "PAGLOCK",
            "TABLOCK",
            "TABLOCKX",
            "XLOCK",
            "SERIALIZABLE",
            "READPAST",
            "REPEATABLEREAD",
        ];
        if words.is_empty()
            || !words
                .iter()
                .all(|w| READS.contains(&w.as_str()) || LOCKS.contains(&w.as_str()))
        {
            return None;
        }
        if words.iter().any(|w| LOCKS.contains(&w.as_str())) {
            self.note(Status::Decision, "tsql.lock-hint", "Lock hints were dropped. To keep the locking, add FOR UPDATE (UPDLOCK), FOR UPDATE SKIP LOCKED (READPAST) or a stricter isolation level (HOLDLOCK/SERIALIZABLE).");
        } else if words
            .iter()
            .any(|w| w == "NOLOCK" || w == "READUNCOMMITTED")
        {
            self.note(Status::Assumptions, "ASM-NOLOCK", "NOLOCK was dropped: PostgreSQL never reads uncommitted rows. Results differ only while other transactions are writing.");
        } else {
            self.note(
                Status::Converted,
                "tsql.plan-hint",
                "Plan hints were dropped; they do not change results.",
            );
        }
        Some(r)
    }

    fn now(&mut self) -> String {
        self.note(Status::Assumptions, "ASM-TIME-SOURCE", "Current time: both use the statement start, but PostgreSQL uses the session time zone where T-SQL uses the server clock. Equal if the session time zone is the server's.");
        "statement_timestamp()::timestamp".into()
    }

    /// Built-in function rewrites. `None` keeps the call as written.
    fn func(&mut self, name: &str, args: &[T]) -> Option<String> {
        let a = split_commas(args);
        let n = a.len();
        let e = |cx: &mut Self, k: usize| cx.expr(a[k]).trim().to_string();
        Some(match (name, n) {
            ("GETDATE" | "SYSDATETIME", 0) => self.now(),
            ("GETUTCDATE" | "SYSUTCDATETIME", 0) => {
                "(statement_timestamp() AT TIME ZONE 'UTC')".into()
            }
            ("ISNULL", 2) => {
                self.note(Status::Assumptions, "ASM-ISNULL-TYPE", "ISNULL becomes COALESCE: ISNULL takes the type of its first argument (it can truncate), COALESCE the widest type. Equal unless the fallback is longer or wider than the first argument.");
                let (x, y) = (e(self, 0), e(self, 1));
                format!("COALESCE({x}, {y})")
            }
            ("LEN", 1) => format!("length(rtrim({}))", e(self, 0)),
            ("DATALENGTH", 1) => {
                self.note(Status::Assumptions, "ASM-DATALENGTH", "DATALENGTH becomes octet_length: byte counts differ for NVARCHAR (UTF-16 in T-SQL, UTF-8 in PostgreSQL).");
                format!("octet_length({})", e(self, 0))
            }
            ("CHARINDEX", 2) => {
                self.note(Status::Assumptions, "ASM-CI-COLLATION", "String comparison: T-SQL's default collation is case-insensitive and ignores trailing spaces; PostgreSQL compares case-sensitively. Equal only if the data's case is consistent (or use a case-insensitive collation).");
                let (x, y) = (e(self, 0), e(self, 1));
                format!("strpos({y}, {x})")
            }
            ("NEWID", 0) => "gen_random_uuid()".into(),
            ("IIF", 3) => {
                let (c, x, y) = (e(self, 0), e(self, 1), e(self, 2));
                format!("(CASE WHEN {c} THEN {x} ELSE {y} END)")
            }
            ("REPLICATE", 2) => {
                let (x, y) = (e(self, 0), e(self, 1));
                format!("repeat({x}, {y})")
            }
            ("SCOPE_IDENTITY", 0) => {
                self.note(Status::Decision, "pg.lastval", "SCOPE_IDENTITY() becomes lastval(), which also sees sequence values used by triggers and other statements in the session. Prefer INSERT … RETURNING id INTO v_id.");
                "lastval()".into()
            }
            ("ERROR_MESSAGE", 0) if self.in_catch => "SQLERRM".into(),
            ("ERROR_NUMBER", 0) if self.in_catch => {
                self.note(Status::Decision, "pg.sqlstate", "ERROR_NUMBER() becomes SQLSTATE: a five-character code, not a number, so comparisons against T-SQL error numbers must be rewritten.");
                "SQLSTATE".into()
            }
            ("CAST", 1) => {
                let at = crate::tok::find_word(a[0], 0, "AS")?;
                let x = self.expr(&a[0][..at]).trim().to_string();
                let (ty, _, _) = self.ty(&a[0][at + 1..]).ok()?;
                format!("CAST({x} AS {ty})")
            }
            ("CONVERT", 2) => {
                let (ty, _, _) = self.ty(a[0]).ok()?;
                format!("CAST({} AS {ty})", e(self, 1))
            }
            ("CONVERT", 3) => {
                self.note(Status::NotConverted, "tsql.convert-style", "CONVERT with a style code: use to_char / to_timestamp with the matching format.");
                return None;
            }
            ("DATEADD", 3) => {
                let part = date_part(&self.expr(a[0]))?;
                let (k, d) = (e(self, 1), e(self, 2));
                format!("({d} + ({k}) * interval '1 {part}')")
            }
            ("DATEPART", 2) => {
                let part = date_part(&self.expr(a[0]))?;
                format!("EXTRACT({part} FROM {})::int", e(self, 1))
            }
            ("YEAR" | "MONTH" | "DAY", 1) => format!("EXTRACT({name} FROM {})::int", e(self, 0)),
            ("DATEDIFF", 3)
                if matches!(date_part(&self.expr(a[0])), Some("day" | "month" | "year")) =>
            {
                // Boundary counts, exactly as T-SQL defines them.
                let part = date_part(&self.expr(a[0])).unwrap();
                let (x, y) = (e(self, 1), e(self, 2));
                match part {
                    "day" => format!("(({y})::date - ({x})::date)"),
                    "year" => format!("(EXTRACT(YEAR FROM {y}) - EXTRACT(YEAR FROM {x}))::int"),
                    _ => format!("((EXTRACT(YEAR FROM {y}) - EXTRACT(YEAR FROM {x})) * 12 + EXTRACT(MONTH FROM {y}) - EXTRACT(MONTH FROM {x}))::int"),
                }
            }
            ("DATEDIFF", 3) => {
                self.note(Status::NotConverted, "tsql.datediff", "DATEDIFF counts boundaries crossed (DATEDIFF(year, '2024-12-31', '2025-01-01') = 1); PostgreSQL subtraction gives elapsed time. Needs a per-unit rewrite.");
                return None;
            }
            (
                "OBJECT_ID" | "FORMAT" | "STR" | "STUFF" | "PATINDEX" | "DATENAME" | "SUSER_SNAME"
                | "XACT_STATE" | "ERROR_SEVERITY" | "ERROR_STATE" | "ERROR_LINE"
                | "ERROR_PROCEDURE",
                _,
            ) => {
                self.note(
                    Status::NotConverted,
                    &format!("tsql.{}", name.to_ascii_lowercase()),
                    &format!("{name}() has no rule yet."),
                );
                return None;
            }
            _ => return None,
        })
    }

    /// Maps a T-SQL type to PostgreSQL: (type, is string, is numeric).
    fn ty(&mut self, ts: &[T]) -> R<(String, bool, bool)> {
        let ts = trimmed(ts);
        let Some(first) = ts.first() else {
            return no("missing type");
        };
        let name = crate::tok::T {
            s: tsql_unquote(&first.s),
            ..first.clone()
        };
        let up = name.s.to_ascii_uppercase();
        let args: Vec<String> = match ts.iter().position(|t| t.k == L_PAREN) {
            Some(p) => split_commas(&ts[p + 1..matching(ts, p).unwrap_or(ts.len())])
                .iter()
                .map(|a| trimmed(a).iter().map(|t| t.s.clone()).collect::<String>())
                .collect(),
            None => Vec::new(),
        };
        let paren = |a: &[String]| {
            if a.is_empty() {
                String::new()
            } else {
                format!("({})", a.join(", "))
            }
        };
        let max = args.first().is_some_and(|a| a.eq_ignore_ascii_case("MAX"));
        let num = |t: &str| (t.to_string(), false, true);
        let txt = |t: String| (t, true, false);
        Ok(match up.as_str() {
            "INT" | "INTEGER" => num("integer"),
            "BIGINT" => num("bigint"),
            "SMALLINT" => num("smallint"),
            "TINYINT" => {
                self.note(Status::Assumptions, "ASM-TINYINT", "TINYINT becomes smallint: values outside 0–255 are no longer rejected.");
                num("smallint")
            }
            "BIT" => {
                self.note(Status::Decision, "pg.bit-boolean", "BIT becomes boolean: comparisons with 1/0 must become TRUE/FALSE. Use smallint instead to keep numeric comparisons.");
                ("boolean".into(), false, false)
            }
            "DECIMAL" | "NUMERIC" | "DEC" => (format!("numeric{}", paren(&args)), false, true),
            "MONEY" => num("numeric(19,4)"),
            "SMALLMONEY" => num("numeric(10,4)"),
            "FLOAT" => {
                if args.first().and_then(|a| a.parse::<u32>().ok()).is_some_and(|p| p <= 24) {
                    num("real")
                } else {
                    num("double precision")
                }
            }
            "REAL" => num("real"),
            "DATETIME" => {
                self.note(Status::Assumptions, "ASM-DATETIME-PRECISION", "DATETIME becomes timestamp(3): T-SQL rounds DATETIME to 1/300 s, PostgreSQL keeps milliseconds exactly.");
                ("timestamp(3)".into(), false, false)
            }
            "DATETIME2" => (format!("timestamp{}", paren(&args)), false, false),
            "SMALLDATETIME" => ("timestamp(0)".into(), false, false),
            "DATE" => ("date".into(), false, false),
            "TIME" => (format!("time{}", paren(&args)), false, false),
            "DATETIMEOFFSET" => {
                self.note(Status::Assumptions, "ASM-DATETIMEOFFSET", "DATETIMEOFFSET becomes timestamptz, which stores the instant but not the original offset.");
                (format!("timestamptz{}", paren(&args)), false, false)
            }
            "CHAR" | "NCHAR" => txt(format!("char{}", paren(&args))),
            "VARCHAR" | "NVARCHAR" if max => txt("text".into()),
            "VARCHAR" | "NVARCHAR" => txt(format!("varchar{}", paren(&args))),
            "TEXT" | "NTEXT" => txt("text".into()),
            "SYSNAME" => txt("varchar(128)".into()),
            "VARBINARY" | "BINARY" | "IMAGE" => ("bytea".into(), false, false),
            "UNIQUEIDENTIFIER" => ("uuid".into(), false, false),
            "XML" => ("xml".into(), false, false),
            "ROWVERSION" | "TIMESTAMP" => return no("ROWVERSION/TIMESTAMP: PostgreSQL has no automatic row version column; use xmin or a trigger-maintained column"),
            "SQL_VARIANT" | "HIERARCHYID" | "GEOGRAPHY" | "GEOMETRY" | "CURSOR" | "TABLE" => return no(format!("type {up} has no PostgreSQL equivalent rule yet")),
            _ => {
                self.note(Status::Assumptions, "ASM-USER-TYPE", "A type without a rule was kept as written: it must exist in PostgreSQL under that name.");
                let t: String = ts.iter().map(|t| t.s.clone()).collect();
                (self.expr_plain(&t), false, false)
            }
        })
    }

    fn expr_plain(&self, s: &str) -> String {
        s.replace("dbo.", "")
    }
}

/// Whether a batch-level unit needs PL/pgSQL (variables or control flow).
fn procedural(n: &SyntaxNode) -> bool {
    if matches!(n.kind(), IF_STMT | LOOP_STMT | TRY_CATCH | BLOCK) {
        return true;
    }
    let ts = tokens(n);
    let first = ts.iter().find(|t| t.sig());
    first.is_some_and(|t| {
        ["DECLARE", "PRINT", "RETURN", "THROW", "BREAK", "CONTINUE"]
            .iter()
            .any(|w| t.is(w))
    }) || ts.iter().any(|t| t.k == VARIABLE && !t.s.starts_with("@@"))
}

/// CATCH body that is only `IF @@TRANCOUNT > 0 ROLLBACK [TRAN]` and `THROW;`.
fn rollback_rethrow(catch: &SyntaxNode) -> bool {
    let words: Vec<String> = sqldiff_syntax::ast::sig_tokens(catch)
        .map(|t| t.text().to_ascii_uppercase())
        .filter(|w| {
            !matches!(
                w.as_str(),
                ";" | "BEGIN" | "END" | "CATCH" | "TRAN" | "TRANSACTION"
            )
        })
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    matches!(
        w.as_slice(),
        ["IF", "@@TRANCOUNT", ">", "0", "ROLLBACK", "THROW"] | ["ROLLBACK", "THROW"]
    )
}

fn is_clause_word(s: &str) -> bool {
    [
        "SELECT", "WHERE", "AND", "OR", "THEN", "ELSE", "WHEN", "RETURN", "SET", "ON", "BY",
    ]
    .iter()
    .any(|w| s.eq_ignore_ascii_case(w))
}

fn date_part(p: &str) -> Option<&'static str> {
    Some(match p.trim().to_ascii_lowercase().as_str() {
        "year" | "yy" | "yyyy" => "year",
        "month" | "mm" | "m" => "month",
        "day" | "dd" | "d" => "day",
        "hour" | "hh" => "hour",
        "minute" | "mi" | "n" => "minute",
        "second" | "ss" | "s" => "second",
        _ => return None,
    })
}

/// `[Name]` → `Name` when it is a plain identifier, else `"Name"`.
fn quoted(s: &str) -> String {
    let inner = tsql_unquote(s);
    let plain = inner
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !sqldiff_core::normalize::is_keyword(&inner.to_ascii_uppercase());
    if plain {
        inner
    } else {
        format!("\"{}\"", inner.replace('"', "\"\""))
    }
}

fn tsql_unquote(s: &str) -> String {
    sqldiff_syntax::ast::unquote(s)
}

/// 0-based column of a byte offset.
fn column(src: &str, at: usize) -> usize {
    at - src[..at].rfind('\n').map_or(0, |p| p + 1)
}

/// Removes up to `n` leading spaces from every line but the first.
fn dedent(text: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            let strip = line.len() - line.trim_start_matches(' ').len();
            out.push_str(&line[strip.min(n)..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Collapses runs of spaces left behind by dropped words.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::new();
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let lead = line.len() - line.trim_start().len();
        out.push_str(&line[..lead]);
        out.push_str(
            &line[lead..]
                .split(' ')
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    out
}

/// Replaces `from` when it ends at a word boundary (case-insensitive on the keyword).
fn replace_word(s: &str, from: &str, to: &str) -> String {
    let up = s.to_ascii_uppercase();
    let Some(i) = up.find(&from.to_ascii_uppercase()) else {
        return s.to_string();
    };
    let end = i + from.len();
    if s[end..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return s.to_string();
    }
    format!("{}{to}{}", &s[..i], &s[end..])
}
