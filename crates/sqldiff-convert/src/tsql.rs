//! T-SQL → PostgreSQL and T-SQL → Db2 for i rules (token level over the
//! lossless CST). One walk serves both targets: statements, expressions and
//! refusals are shared, and the target decides the spelling (PL/pgSQL or SQL
//! PL) and the notes. Db2 for i output relies on the corrected vendor facts
//! in docs/vendor-facts-verification.md; facts that were never checked on a
//! real system (V4, V9, V15, parts of V14) only ever appear as assumptions.
//!
//! Each rule either converts faithfully, converts and records the assumption
//! it needs (a note), or refuses, in which case the unit is kept as a comment
//! and marked not converted. Behavioural checks are left to the engine.

use std::collections::HashMap;

use sqldiff_syntax::{Dialect, SyntaxElement, SyntaxKind, SyntaxNode};
use SyntaxKind::*;

use crate::out::Out;
use crate::tok::{
    find_word, matching, next_sig, prev_sig, sig, snake, split_commas, tokens, trimmed, T,
};
use crate::{Note, Status};

const INDENT: usize = 2;

/// Rule id of a refusal because of where the unit stands.
const CONTEXT: &str = "not-converted.context";

/// The idiom rules (`sqldiff_core::rules`), for what the token rules refuse.
mod rules {
    pub use sqldiff_core::rules::convert::{convert_nodes, Options, Piece};
    pub use sqldiff_core::rules::rewrite::NoFacts;
    pub use sqldiff_core::rules::rule::{RuleSet, Status};
}

pub(crate) fn convert(root: &SyntaxNode, src: &str, to: Dialect) -> Out {
    let mut cx = Cx {
        src,
        to,
        loops: Vec::new(),
        labels: 0,
        cursors: Vec::new(),
        xact_abort: false,
        seen_work: false,
        tables: HashMap::new(),
        savepoints: Vec::new(),
        vars: HashMap::new(),
        notes: Vec::new(),
        mode: Mode::Top,
        in_catch: false,
        in_try: false,
        in_cast: false,
        return_code: false,
        decls: Vec::new(),
        context: false,
    };
    let mut out = Out::default();
    let els: Vec<SyntaxElement> = root.children_with_tokens().collect();
    let mut joined = 0;
    for (i, el) in els.iter().cloned().enumerate() {
        match el {
            SyntaxElement::Node(n) if n.kind() == BATCH => cx.batch(&mut out, &n),
            SyntaxElement::Node(n) if n.kind().is_unit() => {
                cx.list_unit(&mut out, &els, i, 0, &mut joined)
            }
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
    /// Target dialect: PostgreSQL or Db2 for i.
    to: Dialect,
    /// Db2: labels of the enclosing WHILE loops (LEAVE/ITERATE need one).
    loops: Vec<String>,
    /// Db2: loop labels handed out in this routine or script.
    labels: usize,
    /// Db2: result-set cursors of the routine, declared after the variables.
    cursors: Vec<Decl>,
    /// Db2: `SET XACT_ABORT ON` seen; the routine gets an EXIT handler that
    /// rolls back and resignals.
    xact_abort: bool,
    /// A statement that does work ran before (XACT_ABORT ON must come first).
    seen_work: bool,
    /// Db2: table variables (lower-case name without `@`) → `SESSION.t_name`.
    tables: HashMap<String, String>,
    /// Savepoint names set with SAVE TRANSACTION in this routine or script.
    savepoints: Vec<String>,
    vars: HashMap<String, Var>,
    /// Notes of the unit being converted.
    notes: Vec<Note>,
    mode: Mode,
    in_catch: bool,
    /// Inside the TRY part of a `BEGIN … EXCEPTION` block (a subtransaction).
    in_try: bool,
    /// Mapping the target type of CAST/CONVERT (string lengths default to 30 there, 1 elsewhere).
    in_cast: bool,
    /// A procedure returns a value somewhere: it gets an INOUT `return_code`.
    return_code: bool,
    decls: Vec<Decl>,
    /// The unit being converted was refused because of where it stands, not
    /// for want of a rule: the idiom rules, which see statements out of
    /// context, must not take it either.
    context: bool,
}

/// A refusal: the unit is kept as a comment with this reason.
struct No(String);

type R<T> = Result<T, No>;

fn no<T>(reason: impl Into<String>) -> R<T> {
    Err(No(reason.into()))
}

impl Cx<'_> {
    fn db2(&self) -> bool {
        self.to == Dialect::Db2i
    }

    /// The target's wording: PostgreSQL or Db2 for i.
    fn pick<'s>(&self, pg: &'s str, db2: &'s str) -> &'s str {
        if self.db2() {
            db2
        } else {
            pg
        }
    }

    fn note(&mut self, status: Status, rule: &str, text: &str) {
        if !self.notes.iter().any(|n| n.rule == rule) {
            self.notes.push(Note {
                status,
                rule: rule.to_string(),
                text: text.to_string(),
            });
        }
    }

    /// A refusal because of the unit's context (see `context`).
    fn misplaced<T>(&mut self, reason: &str) -> R<T> {
        self.context = true;
        no(reason)
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
        let rule = if std::mem::take(&mut self.context) {
            CONTEXT
        } else {
            "not-converted"
        };
        out.unit(
            Self::range(n),
            indent,
            &text,
            vec![Note {
                status: Status::NotConverted,
                rule: rule.into(),
                text: reason.to_string(),
            }],
        );
    }

    // ----- batches, routines, bodies -------------------------------------------

    fn batch(&mut self, out: &mut Out, b: &SyntaxNode) {
        let units: Vec<SyntaxNode> = b.children().filter(|c| c.kind().is_unit()).collect();
        if units.iter().any(|u| u.kind() == ROUTINE) {
            // T-SQL wants CREATE PROCEDURE alone in its batch, but views and
            // tables can share one: convert every unit, never drop one.
            self.mode = Mode::Top;
            self.body_elements(out, b, 0);
            return;
        }
        if units.iter().any(procedural) {
            self.do_block(out, b);
            return;
        }
        self.mode = Mode::Top;
        self.body_elements(out, b, 0);
    }

    /// Forgets what the previous routine or script declared.
    fn reset(&mut self) {
        self.vars.clear();
        self.decls.clear();
        self.cursors.clear();
        self.loops.clear();
        self.labels = 0;
        self.xact_abort = false;
        self.seen_work = false;
        self.tables.clear();
        self.savepoints.clear();
    }

    fn do_block(&mut self, out: &mut Out, b: &SyntaxNode) {
        self.mode = Mode::Do;
        self.reset();
        let mut body = Out::default();
        self.body_elements(&mut body, b, INDENT);
        if self.db2() {
            // A dynamic compound statement; its commit level is the job's.
            out.push("BEGIN\n");
            self.write_decls(out);
            out.append(body);
            out.push("END;\n");
        } else {
            out.push("DO $$\n");
            self.write_decls(out);
            out.push("BEGIN\n");
            out.append(body);
            out.push("END\n$$;\n");
        }
        self.mode = Mode::Top;
    }

    /// The hoisted declarations. PostgreSQL: a DECLARE section before BEGIN.
    /// Db2: inside BEGIN, variables first, then cursors, then handlers.
    fn write_decls(&mut self, out: &mut Out) {
        if self.db2() {
            for d in std::mem::take(&mut self.decls)
                .into_iter()
                .chain(std::mem::take(&mut self.cursors))
            {
                out.unit(d.src, INDENT, &d.lines.join("\n"), d.notes);
            }
            if self.xact_abort {
                out.lines(
                    INDENT,
                    "DECLARE EXIT HANDLER FOR SQLEXCEPTION\nBEGIN\n  ROLLBACK;\n  RESIGNAL;\nEND;",
                );
            }
            return;
        }
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
            "TRIGGER" if self.db2() => no("triggers: Db2 for i triggers are row or statement triggers with REFERENCING NEW/OLD (TABLE); inserted/deleted need a rewrite"),
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
            let text = format!(
                "View options such as SCHEMABINDING have no {} equivalent and were dropped; they do not change results.",
                self.to.display_name()
            );
            self.note(Status::Converted, "tsql.view-options", &text);
        }
        let name_end = head.iter().position(|t| t.is("WITH")).unwrap_or(head.len());
        let name = self.expr(&head[..name_end]).trim().to_string();
        if ts.iter().take(name_at).any(|t| t.is("ALTER")) && !self.db2() {
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
        let (q, _) = self.select(&tokens(&stmt))?;
        Ok(Some(format!("CREATE OR REPLACE VIEW {name} AS\n{q};")))
    }

    fn proc_or_func(&mut self, out: &mut Out, r: &SyntaxNode, func: bool) -> R<Option<String>> {
        self.reset();
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
        let ds = crate::tok::depths(&ts[i..]);
        let stop = (i..ts.len())
            .find(|&j| {
                ds[j - i] == 0 && (ts[j].is("RETURNS") || ts[j].is("AS") || ts[j].is("WITH"))
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
                return no(format!(
                    "table-valued parameters (READONLY) have no {} equivalent; {}",
                    self.to.display_name(),
                    self.pick(
                        "pass an array or a temp table",
                        "pass the rows in a global temporary table"
                    )
                ));
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
                return self.misplaced("OUTPUT parameters on a function");
            }
            let mode = match (output, self.db2() && !func) {
                (true, _) => "INOUT ",
                (false, true) => "IN ",
                (false, false) => "",
            };
            let mut text = format!("{mode}{pname} {ty}");
            if let Some(eq) = p[..].iter().position(|t| t.k == OP && t.s == "=") {
                let end = (eq + 1..p.len())
                    .find(|&j| p[j].is("OUTPUT") || p[j].is("OUT"))
                    .unwrap_or(p.len());
                text.push_str(&format!(" DEFAULT {}", self.expr(&p[eq + 1..end]).trim()));
                seen_default = true;
            } else if seen_default && !self.db2() {
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
                return no(self.pick(
                    "table-valued functions: convert to RETURNS TABLE (…) with RETURN QUERY",
                    "table-valued functions: convert to RETURNS TABLE (…) with RETURN followed by the query",
                ));
            }
            let (ty, _, _) = self.ty(rt)?;
            returns = format!(" RETURNS {ty}");
        }
        if ts.iter().any(|t| t.is("WITH")) {
            let text = self.pick(
                "Routine options (WITH EXECUTE AS, RECOMPILE, ENCRYPTION, …) were dropped; use SECURITY DEFINER for EXECUTE AS OWNER.",
                "Routine options (WITH EXECUTE AS, RECOMPILE, ENCRYPTION, …) were dropped; use SET OPTION USRPRF = *OWNER for EXECUTE AS OWNER.",
            ).to_string();
            self.note(Status::Decision, "tsql.routine-options", &text);
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
        if self.return_code && !self.db2() {
            params.push("INOUT return_code integer DEFAULT NULL".into());
        }
        self.notes = header_notes;
        if self.db2() {
            return Ok(self.db2_routine(out, r, &body, b, func, &name, &params, &returns, kw));
        }
        if ts[..kw].iter().any(|t| t.is("ALTER")) {
            self.note(
                Status::Assumptions,
                "ASM-ROUTINE-REPLACE",
                "ALTER becomes CREATE OR REPLACE, which replaces the routine only if the parameter types are unchanged. With a changed parameter list PostgreSQL adds a second overload (calls can become ambiguous): drop the old signature first.",
            );
        }
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

    /// The Db2 for i routine around a converted body: SQL PL with the commit
    /// level written out, since without it the level comes from whoever
    /// creates the routine (fact V5).
    #[allow(clippy::too_many_arguments)]
    fn db2_routine(
        &mut self,
        out: &mut Out,
        r: &SyntaxNode,
        body: &SyntaxNode,
        b: Out,
        func: bool,
        name: &str,
        params: &[String],
        returns: &str,
        kw: usize,
    ) -> Option<String> {
        let ts = tokens(r);
        if ts[..kw].iter().any(|t| t.is("ALTER")) {
            self.note(
                Status::Assumptions,
                "ASM-ROUTINE-REPLACE",
                "ALTER becomes CREATE OR REPLACE, which replaces the routine with the same name and number of parameters. With a different number of parameters Db2 for i adds a second routine beside the old one: drop the old one first.",
            );
        }
        if self.return_code {
            self.note(
                Status::Assumptions,
                "ASM-RETURN-STATUS",
                "RETURN n is kept: callers read it with GET DIAGNOSTICS v = DB2_RETURN_STATUS (or the return value of the CALL in JDBC/ODBC), not as T-SQL's EXEC @rc = ….",
            );
        }
        let mut options = String::from("LANGUAGE SQL");
        if !func {
            if !self.cursors.is_empty() {
                options = format!("DYNAMIC RESULT SETS {}\n{options}", self.cursors.len());
            }
            options.push_str("\nSET OPTION COMMIT = *CS");
            self.note(
                Status::Assumptions,
                "ASM-DB2-COMMIT-CS",
                "The procedure is compiled with SET OPTION COMMIT = *CS: each statement is atomic and reads see only committed rows, as in T-SQL (READ COMMITTED). Without it the commit level is whatever the creating job uses (fact V5), and under *NONE nothing, not even a failed statement, is rolled back (facts V2, V4). The work stays in the caller's unit of work until it commits.",
            );
        }
        let header = format!(
            "CREATE OR REPLACE {} {name} ({}){returns}\n{options}\nBEGIN",
            if func { "FUNCTION" } else { "PROCEDURE" },
            params.join(", ")
        );
        let (s, _) = Self::range(r);
        let notes = self.take_notes();
        out.unit((s, body.text_range().start().into()), 0, &header, notes);
        self.write_decls(out);
        out.append(b);
        out.push("END;\n");
        self.mode = Mode::Top;
        None
    }

    /// A statement list of IF, WHILE or a handler. Db2 wants at least one
    /// statement there, so a list left with only comments (a dropped PRINT,
    /// a refused statement) gets an empty compound statement.
    fn stmts(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        if !self.db2() {
            return self.body_elements(out, n, indent);
        }
        let mut b = Out::default();
        self.body_elements(&mut b, n, indent);
        fill_empty(&mut b, indent);
        out.append(b);
    }

    /// Writes the units and comments directly inside `n`, skipping its own keywords.
    fn body_elements(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        let els: Vec<SyntaxElement> = n.children_with_tokens().collect();
        let mut joined = 0;
        for (i, el) in els.iter().cloned().enumerate() {
            match el {
                SyntaxElement::Node(c) if c.kind().is_unit() => {
                    self.list_unit(out, &els, i, indent, &mut joined)
                }
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

    /// Converts the unit `els[i]` of a list. `joined` is where the last pair
    /// converted together ended: a unit before it is already written.
    fn list_unit(
        &mut self,
        out: &mut Out,
        els: &[SyntaxElement],
        i: usize,
        indent: usize,
        joined: &mut usize,
    ) {
        let Some(n) = els[i].as_node() else { return };
        if Self::range(n).0 < *joined {
            return;
        }
        if let Some(next) = adjacent_unit(els, i) {
            if self.idiom_pair(out, n, &next, indent) {
                *joined = Self::range(&next).1;
                return;
            }
        }
        self.unit(out, n, indent)
    }

    /// The idiom rules on `nodes`, with variables spelled as the token rules
    /// spell them (so after the DECLAREs seen so far). Also says whether a
    /// variable had no declaration.
    fn idioms(&self, nodes: &[SyntaxNode]) -> (Vec<rules::Piece>, bool) {
        let undeclared = std::cell::Cell::new(false);
        let vars = |v: &str| {
            self.declared(v).unwrap_or_else(|| {
                undeclared.set(true);
                undeclared_name(v)
            })
        };
        let opts = rules::Options {
            rules: rules::RuleSet::builtin(),
            facts: &rules::NoFacts,
            vars: &vars,
        };
        let pieces = rules::convert_nodes(nodes, Dialect::Tsql, self.to, &opts);
        (pieces, undeclared.get())
    }

    /// Two adjacent statements the idiom rules convert as one (INSERT +
    /// SCOPE_IDENTITY, sp_getapplock + its return-code check), which the
    /// token rules see apart. Only in a procedure or DO block and outside
    /// TRY/CATCH, where the rules' plain statements mean what they say.
    fn idiom_pair(&mut self, out: &mut Out, a: &SyntaxNode, b: &SyntaxNode, indent: usize) -> bool {
        if !matches!(self.mode, Mode::Proc | Mode::Do)
            || self.in_try
            || self.in_catch
            || a.kind() != STATEMENT
            || !matches!(first_word(a).as_str(), "INSERT" | "EXEC" | "EXECUTE")
        {
            return false;
        }
        let (pieces, undeclared) = self.idioms(&[a.clone(), b.clone()]);
        match pieces.as_slice() {
            [p] if p.units.len() == 2 && p.output.is_ok() && !p.rules.is_empty() => {
                self.piece(out, p, undeclared, indent);
                true
            }
            _ => false,
        }
    }

    /// A data statement the token rules refused for want of a rule (MERGE,
    /// DELETE … JOIN), if an idiom rule converts it. Never a refusal because
    /// of context, never a statement whose meaning depends on where it stands
    /// (RETURN, THROW, transaction control, result sets), never a compound.
    fn idiom_single(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) -> bool {
        if self.context
            || !matches!(
                first_word(n).as_str(),
                "INSERT" | "UPDATE" | "DELETE" | "MERGE"
            )
            || self.mode == Mode::Top && procedural(n)
        {
            return false;
        }
        let (pieces, undeclared) = self.idioms(std::slice::from_ref(n));
        match pieces.as_slice() {
            [p] if p.output.is_ok() && !p.rules.is_empty() => {
                self.notes.clear();
                self.piece(out, p, undeclared, indent);
                true
            }
            _ => false,
        }
    }

    fn piece(&mut self, out: &mut Out, p: &rules::Piece, undeclared: bool, indent: usize) {
        let mut notes = vec![Note {
            status: Status::Converted,
            rule: "idiom-rules".into(),
            text: format!("Converted by the idiom rules ({}).", p.rules.join(", ")),
        }];
        notes.extend(p.notes.iter().map(|a| Note {
            status: a.status.into(),
            rule: a.rule.clone(),
            text: a.text.clone(),
        }));
        notes.extend(p.assumptions.iter().map(|(id, text)| Note {
            status: Status::Assumptions,
            rule: id.clone(),
            text: text.clone(),
        }));
        if undeclared {
            notes.push(Note {
                status: Status::Assumptions,
                rule: "ASM-UNDECLARED-VAR".into(),
                text: UNDECLARED.into(),
            });
        }
        let text = p.output.as_deref().unwrap_or_default();
        out.unit(p.src, indent, text, notes);
    }

    /// How a declared T-SQL variable (or parameter) is spelled in the output.
    fn declared(&self, v: &str) -> Option<String> {
        self.vars
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
            .map(|x| x.name.clone())
    }

    fn unit(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        self.notes.clear();
        self.context = false;
        if matches!(n.kind(), IF_STMT | LOOP_STMT | TRY_CATCH) {
            self.seen_work = true;
        }
        match n.kind() {
            STATEMENT => {
                let reason = match self.stmt(n) {
                    Ok(_) if self.blocked().is_some() => self.blocked().unwrap(),
                    Ok(text) => {
                        let notes = self.take_notes();
                        return out.unit(Self::range(n), indent, &text, notes);
                    }
                    Err(No(reason)) => reason,
                };
                if !self.idiom_single(out, n, indent) {
                    self.refuse(out, n, indent, &reason)
                }
            }
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
            .map(|c| {
                let mut ts = tokens(&c);
                if self.db2() {
                    ts = dummy_from(&ts, false);
                }
                self.expr(&ts).trim().to_string()
            })
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
        let kw = match (elsif, self.db2()) {
            (false, _) => "IF",
            (true, false) => "ELSIF",
            (true, true) => "ELSEIF",
        };
        self.header_unit(out, n, indent, &format!("{kw} {cond} THEN"));
        let branches: Vec<SyntaxNode> = n.children().filter(|c| c.kind() == BRANCH).collect();
        if let Some(b) = branches.first() {
            self.stmts(out, b, indent + INDENT);
        }
        if let Some(e) = branches.get(1) {
            let inner: Vec<SyntaxNode> = e.children().filter(|c| c.kind().is_unit()).collect();
            if inner.len() == 1 && inner[0].kind() == IF_STMT {
                self.if_stmt(out, &inner[0], indent, true);
            } else {
                out.lines(indent, "ELSE");
                self.stmts(out, e, indent + INDENT);
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
        if self.db2() {
            // LEAVE and ITERATE name the loop they act on.
            self.labels += 1;
            let label = format!("loop_{}", self.labels);
            self.header_unit(out, n, indent, &format!("{label}: WHILE {cond} DO"));
            self.loops.push(label.clone());
            let mut body = Out::default();
            for b in n.children().filter(|c| c.kind() == BRANCH) {
                self.body_elements(&mut body, &b, indent + INDENT);
            }
            self.loops.pop();
            fill_empty(&mut body, indent + INDENT);
            out.append(body);
            out.lines(indent, &format!("END WHILE {label};"));
            return;
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
        if self.db2() {
            return self.db2_try_catch(out, n, &try_b, &catch_b, indent);
        }
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
        let was_try = std::mem::replace(&mut self.in_try, true);
        self.body_elements(out, &try_b, indent + INDENT);
        self.in_try = was_try;
        out.lines(indent, "EXCEPTION WHEN OTHERS THEN");
        let was = std::mem::replace(&mut self.in_catch, true);
        self.body_elements(out, &catch_b, indent + INDENT);
        self.in_catch = was;
        out.lines(indent, "END;");
    }

    /// Db2: TRY/CATCH becomes a compound statement whose EXIT handler is the
    /// CATCH part. Unlike PostgreSQL's EXCEPTION block nothing the TRY part
    /// completed is undone, which is T-SQL's behaviour with XACT_ABORT OFF.
    fn db2_try_catch(
        &mut self,
        out: &mut Out,
        n: &SyntaxNode,
        try_b: &SyntaxNode,
        catch_b: &SyntaxNode,
        indent: usize,
    ) {
        let s = Self::range(n).0;
        let rethrow = rollback_rethrow(catch_b);
        if rethrow {
            self.note(
                Status::Assumptions,
                "idiom.rollback-rethrow",
                "CATCH only rolls back and rethrows: the handler does ROLLBACK; RESIGNAL;, which undoes the whole unit of work (the caller's too, as T-SQL's ROLLBACK does) and raises the same error. Assumes the procedure runs under commitment control (it is compiled with *CS); under *NONE the ROLLBACK undoes nothing (fact V4).",
            );
        } else {
            self.note(
                Status::Assumptions,
                "ASM-TRY-HANDLER",
                "CATCH becomes an EXIT handler for SQLEXCEPTION: what the TRY part completed stays, as in T-SQL with XACT_ABORT OFF, and the failed statement is rolled back under commitment control. SQLEXCEPTION covers every error SQLSTATE; T-SQL's CATCH skips compile and name-resolution errors at the same level.",
            );
        }
        let notes = self.take_notes();
        out.unit((s, s + 9), indent, "BEGIN", notes);
        let inner = indent + INDENT;
        out.lines(inner, "DECLARE EXIT HANDLER FOR SQLEXCEPTION");
        out.lines(inner, "BEGIN");
        let was = std::mem::replace(&mut self.in_catch, true);
        if rethrow {
            let (cs, ce) = Self::range(catch_b);
            out.unit((cs, ce), inner + INDENT, "ROLLBACK;\nRESIGNAL;", Vec::new());
        } else {
            // What ERROR_MESSAGE() and ERROR_NUMBER() read, captured first:
            // any statement in the handler resets the diagnostics area.
            let words: Vec<String> = sqldiff_syntax::ast::sig_tokens(catch_b)
                .map(|t| t.text().to_ascii_uppercase())
                .collect();
            let msg = words.iter().any(|w| w == "ERROR_MESSAGE");
            let num = words.iter().any(|w| w == "ERROR_NUMBER");
            let mut items = Vec::new();
            if msg {
                out.lines(inner + INDENT, "DECLARE v_error_message VARCHAR(32740);");
                items.push("v_error_message = MESSAGE_TEXT");
            }
            if num {
                out.lines(inner + INDENT, "DECLARE v_error_number INTEGER;");
                items.push("v_error_number = DB2_RETURNED_SQLCODE");
            }
            if !items.is_empty() {
                out.lines(
                    inner + INDENT,
                    &format!("GET DIAGNOSTICS CONDITION 1 {};", items.join(", ")),
                );
            }
            self.stmts(out, catch_b, inner + INDENT);
        }
        self.in_catch = was;
        out.lines(inner, "END;");
        let was_try = std::mem::replace(&mut self.in_try, true);
        self.stmts(out, try_b, inner);
        self.in_try = was_try;
        out.lines(indent, "END;");
    }

    // ----- statements -----------------------------------------------------------

    fn stmt(&mut self, n: &SyntaxNode) -> R<String> {
        let mut ts = tokens(n);
        if self.db2() {
            // A query's own FROM-less SELECT is handled in `select`.
            let top = !matches!(
                ts.iter().find(|t| t.sig()).map(T::up).as_deref(),
                Some("SELECT" | "WITH")
            );
            ts = dummy_from(&ts, top);
        }
        let s = sig(&ts);
        let w = |k: usize| s.get(k).map(|&i| ts[i].up()).unwrap_or_default();
        let ind = column(self.src, Self::range(n).0);
        if self.mode == Mode::Top && procedural(n) {
            return self.misplaced(self.pick(
                "uses variables or procedural statements next to a routine in one batch; put it in its own batch so it becomes a DO block",
                "uses variables or procedural statements next to a routine in one batch; put it in its own batch so it becomes a compound (BEGIN … END) statement",
            ));
        }
        let first = w(0);
        if !matches!(first.as_str(), "SET" | "DECLARE" | "PRINT") {
            // XACT_ABORT ON after this cannot become a routine-wide handler.
            self.seen_work = true;
        }
        let text = match first.as_str() {
            "SELECT" | "WITH" => {
                let (q, assigns) = self.select(&ts)?;
                if self.mode != Mode::Top && !assigns {
                    if self.db2() && self.mode == Mode::Proc {
                        return Ok(self.result_set(n, &q));
                    }
                    return self.misplaced(self.pick(
                        "returns a result set: a PostgreSQL procedure cannot; use a function RETURNS TABLE, a refcursor, or PERFORM if the rows are not needed",
                        "returns a result set: only a procedure can (through a cursor WITH RETURN), not a function or a compound statement",
                    ));
                }
                q
            }
            "INSERT" | "UPDATE" | "DELETE" => self.dml(&ts)?,
            "MERGE" if self.db2() => self.merge_db2(&ts)?,
            "MERGE" => return no(self.pick(
                "MERGE: T-SQL and PostgreSQL MERGE differ (OUTPUT, BY SOURCE, concurrency); planned with the upsert canonical form",
                "MERGE: T-SQL and Db2 for i MERGE differ (OUTPUT, BY SOURCE, atomicity under *NONE); planned with the upsert canonical form",
            )),
            "SET" => self.set(&ts)?,
            "DECLARE" => self.declare(&ts, n)?,
            "PRINT" if self.db2() => {
                self.note(Status::Decision, "db2.print", "PRINT was dropped: Db2 for i has no message channel for SQL PL. Use SIGNAL with a warning SQLSTATE ('01xxx'), or write the text to a log table if callers rely on it.");
                let body = Self::body_toks(&ts);
                format!("-- PRINT {}: dropped", self.expr(&body[s[1]..]).trim())
            }
            "PRINT" => {
                let e = self.expr(&ts[s[1]..]);
                format!("RAISE NOTICE '%', {}", e.trim())
            }
            "RETURN" => self.ret(&ts)?,
            "BREAK" | "CONTINUE" if self.db2() => {
                let Some(label) = self.loops.last() else {
                    return self.misplaced("BREAK/CONTINUE outside a WHILE loop");
                };
                format!("{} {label}", if first == "BREAK" { "LEAVE" } else { "ITERATE" })
            }
            "BREAK" => "EXIT".into(),
            "CONTINUE" => "CONTINUE".into(),
            "THROW" => self.throw(&ts)?,
            "RAISERROR" => return no(self.pick(
                "RAISERROR: severity decides whether it is an error at all; rewrite as RAISE EXCEPTION or RAISE NOTICE",
                "RAISERROR: severity decides whether it is an error at all; rewrite as SIGNAL (an error or a warning SQLSTATE)",
            )),
            "BEGIN" if matches!(w(1).as_str(), "TRAN" | "TRANSACTION") => self.txn("BEGIN", &ts)?,
            "COMMIT" => self.txn("COMMIT", &ts)?,
            "ROLLBACK" => self.txn("ROLLBACK", &ts)?,
            "SAVE" if self.db2() => self.save(&ts)?,
            "SAVE" => return no("SAVE TRANSACTION: use SAVEPOINT, but a PL/pgSQL body cannot; use a nested BEGIN … EXCEPTION block"),
            "EXEC" | "EXECUTE" => self.exec(&ts)?,
            "CREATE" => self.create(&ts)?,
            "ALTER" if w(1) == "TABLE" => self.alter_table(&ts)?,
            "DROP" => {
                if w(1) == "INDEX" && find_word(&ts, s[1], "ON").is_some() {
                    return no(self.pick(
                        "DROP INDEX … ON table: PostgreSQL index names are per schema; drop by name",
                        "DROP INDEX … ON table: Db2 for i index names are per schema; drop by name",
                    ));
                }
                if self.db2() && w(2) == "IF" {
                    return no("DROP … IF EXISTS: not checked for Db2 for i; test for the object in QSYS2 (for example QSYS2.SYSTABLES) first");
                }
                self.expr(&ts)
            }
            "TRUNCATE" if self.db2() => {
                self.note(Status::Assumptions, "ASM-DB2-TRUNCATE", "TRUNCATE TABLE gets RESTART IDENTITY, since T-SQL resets the identity seed. Not checked on a real system: whether Db2 for i can roll a TRUNCATE back as T-SQL can (IMMEDIATE certainly cannot).");
                format!("{} RESTART IDENTITY", self.expr(&Self::body_toks(&ts)).trim_end())
            }
            "TRUNCATE" => self.expr(&ts),
            "USE" => return no(self.pick(
                "USE: connect to the other database instead; PostgreSQL cannot switch databases in a session",
                "USE: Db2 for i has one database per system; use SET SCHEMA or qualify names with the library",
            )),
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

    /// A query, and whether it assigns variables (`SELECT @a = …`).
    fn select(&mut self, ts: &[T]) -> R<(String, bool)> {
        let ts = Self::body_toks(ts);
        let ds = crate::tok::depths(&ts);
        let s = sig(&ts);
        if ts[s[0]].is("WITH") {
            if ts.iter().any(|t| t.is("TOP")) {
                return no("TOP inside a CTE query");
            }
            return Ok((self.expr(&ts), false));
        }
        let mut i = 1; // index into s
        let mut limit = None;
        let mut list_from = None; // first token of the select list after TOP
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
                let inner = trimmed(&ts[arg + 1..r]);
                if self.db2() {
                    // FETCH FIRST takes a constant or a variable, not an expression.
                    if inner.len() != 1 || !matches!(inner[0].k, NUMBER | VARIABLE) {
                        return no("TOP (expression): Db2 for i FETCH FIRST takes a number or a variable; compute the count into a variable first");
                    }
                    (r, self.expr(inner).trim().to_string())
                } else {
                    (r, format!("({})", self.expr(inner).trim()))
                }
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
            list_from = next_sig(&ts, end);
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
                return no(self.pick(
                    "SELECT … INTO new_table: use CREATE TABLE … AS SELECT",
                    "SELECT … INTO new_table: use CREATE TABLE … AS (SELECT …) WITH DATA",
                ));
            }
        }
        // Assignment: SELECT @a = e, @b = f [FROM …]
        let list_start = list_from.or_else(|| s.get(i).copied()).unwrap_or(ts.len());
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
                let db2 = self.db2();
                return Ok((
                    targets
                        .iter()
                        .zip(&exprs)
                        .map(|(t, e)| {
                            if db2 {
                                format!("SET {t} = {e};")
                            } else {
                                format!("{t} := {e};")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    true,
                ));
            }
            let text = self.pick(
                "The query returns at most one row. With several rows T-SQL keeps the last row's values and PostgreSQL the first; with none T-SQL keeps the old values and PostgreSQL sets NULL.",
                "The query returns at most one row. With several rows T-SQL keeps the last row's values and Db2 for i raises SQLSTATE 21000; with none both keep the old values (Db2 signals NOT FOUND, a warning that does not stop the routine).",
            ).to_string();
            self.note(Status::Assumptions, "ASM-SELECT-ASSIGN-CARDINALITY", &text);
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
                q.push_str(&self.limit(&l, " "));
            }
            return Ok((q.trim_end().to_string(), true));
        }
        let mut kept: Vec<T> = ts
            .iter()
            .enumerate()
            .filter(|(k, _)| !cut.contains(k))
            .map(|(_, t)| t.clone())
            .collect();
        if self.db2() {
            kept = dummy_from(&kept, true);
        }
        let mut q = self.expr(&kept).trim_end().to_string();
        if let Some(l) = limit {
            q.push_str(&self.limit(&l, "\n"));
        }
        Ok((q, false))
    }

    /// The row limit clause that replaces TOP.
    fn limit(&self, n: &str, sep: &str) -> String {
        if self.db2() {
            format!("{sep}FETCH FIRST {n} ROWS ONLY")
        } else {
            format!("{sep}LIMIT {n}")
        }
    }

    /// Db2: a result set of a procedure becomes a cursor WITH RETURN, declared
    /// with the variables and opened where the query stood.
    fn result_set(&mut self, n: &SyntaxNode, q: &str) -> String {
        let name = format!("result_{}", self.cursors.len() + 1);
        self.note(
            Status::Assumptions,
            "ASM-DB2-RESULT-SET",
            "The result set becomes an INSENSITIVE cursor WITH RETURN TO CLIENT, opened here: its rows are fixed when it is opened, as T-SQL sends them here, and the procedure declares DYNAMIC RESULT SETS. Callers must read the result sets of the CALL; a caller that inserts them (INSERT … EXEC) has no Db2 equivalent.",
        );
        let notes = self.take_notes();
        let q = dedent(q.trim_end(), column(self.src, Self::range(n).0));
        let q: String = q.lines().map(|l| format!("  {l}\n")).collect();
        self.cursors.push(Decl {
            src: Self::range(n),
            lines: vec![format!(
                "DECLARE {name} INSENSITIVE CURSOR WITH RETURN TO CLIENT FOR\n{};",
                q.trim_end()
            )],
            notes,
        });
        format!("OPEN {name};")
    }

    fn dml(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if find_word(&ts, 0, "OUTPUT").is_some() {
            return no(self.pick(
                "OUTPUT clause: use RETURNING (inserted.x → the new row, deleted.x needs a CTE)",
                "OUTPUT clause: use SELECT … FROM FINAL TABLE (INSERT …) or OLD TABLE (DELETE …)",
            ));
        }
        if ts[s[1]].is("TOP") {
            return no(self.pick(
                "TOP in INSERT/UPDATE/DELETE: restrict the rows with a key subquery and LIMIT",
                "TOP in INSERT/UPDATE/DELETE: restrict the rows with a key subquery and FETCH FIRST",
            ));
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
                    return no(self.pick(
                        "UPDATE … FROM with a join: PostgreSQL's UPDATE … FROM must not repeat the target, and with several matching rows both pick one unpredictably; needs a rewrite",
                        "UPDATE … FROM with a join: Db2 for i needs a correlated subquery or MERGE, and with several matching rows T-SQL picks one unpredictably; needs a rewrite",
                    ));
                }
                from = s[0];
            }
            "DELETE" => {
                if !ts[s[1]].is("FROM") {
                    if find_word(&ts, s[1], "FROM").is_some() {
                        return no(self.pick(
                            "DELETE t FROM … JOIN: rewrite with USING or a key subquery",
                            "DELETE t FROM … JOIN: rewrite with EXISTS or a key subquery",
                        ));
                    }
                    pre = "DELETE FROM ".into();
                } else {
                    let first_from = s[1];
                    if find_word(&ts, first_from + 1, "FROM").is_some() {
                        return no(self.pick(
                            "DELETE FROM t FROM … JOIN: rewrite with USING or a key subquery",
                            "DELETE FROM t FROM … JOIN: rewrite with EXISTS or a key subquery",
                        ));
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
            let assign = |v: String| {
                if self.db2() {
                    format!("SET {target} = {v}")
                } else {
                    format!("{target} := {v}")
                }
            };
            return match op.s.as_str() {
                "=" => Ok(assign(rhs)),
                "+=" | "-=" | "*=" | "/=" => {
                    let o = if op.s == "+=" && self.is_string_var(&a.s) {
                        "||".to_string()
                    } else {
                        op.s[..1].to_string()
                    };
                    Ok(assign(format!("{target} {o} {rhs}")))
                }
                _ => no("compound assignment operator"),
            };
        }
        let opt = a.up();
        let val = s.get(2).map(|&j| ts[j].up()).unwrap_or_default();
        if self.db2() {
            return self.set_option_db2(&ts, &opt, &val);
        }
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

    /// Db2: T-SQL's MERGE is close to Db2 for i's; what Db2 lacks is refused.
    fn merge_db2(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if ts
            .iter()
            .any(|t| t.is("OUTPUT") || t.s.eq_ignore_ascii_case("$action"))
        {
            return no("MERGE … OUTPUT: use SELECT … FROM FINAL TABLE around separate statements; Db2 for i MERGE has no OUTPUT");
        }
        if s.get(1).is_some_and(|&j| ts[j].is("TOP")) {
            return no("MERGE TOP (n): restrict the source rows instead");
        }
        let mut kept = Vec::new();
        let mut k = 0;
        while k < ts.len() {
            let t = &ts[k];
            if t.is("BY") {
                match next_sig(&ts, k).map(|j| ts[j].up()).as_deref() {
                    Some("SOURCE") => return no("WHEN NOT MATCHED BY SOURCE: Db2 for i MERGE has only WHEN NOT MATCHED (fact V1); delete the unmatched target rows in a separate DELETE … WHERE NOT EXISTS, which is no longer atomic with the MERGE"),
                    Some("TARGET") => {
                        // WHEN NOT MATCHED BY TARGET = WHEN NOT MATCHED; drop "BY TARGET".
                        let j = next_sig(&ts, k).unwrap();
                        while kept.last().is_some_and(|x: &T| !x.sig()) {
                            kept.pop();
                        }
                        k = j + 1;
                        continue;
                    }
                    _ => {}
                }
            }
            kept.push(t.clone());
            if k == s[0] && !s.get(1).is_some_and(|&j| ts[j].is("INTO")) {
                kept.push(T {
                    k: WHITESPACE,
                    s: " ".into(),
                });
                kept.push(T {
                    k: IDENT,
                    s: "INTO".into(),
                });
            }
            k += 1;
        }
        if self.mode == Mode::Proc {
            self.note(Status::Converted, "db2.merge", "MERGE is kept: in the procedure (compiled with *CS) it is atomic, as in T-SQL. Db2 for i honours ATOMIC only under commitment control (fact V2).");
        } else {
            self.note(Status::Assumptions, "ASM-FACT-V2", "MERGE is atomic only under commitment control: under *NONE, rows changed before an error stay changed (fact V2). Assumes the script runs with a commit level other than *NONE.");
        }
        Ok(self.expr(&kept))
    }

    /// Db2: T-SQL session options.
    fn set_option_db2(&mut self, ts: &[T], opt: &str, val: &str) -> R<String> {
        match (opt, val) {
            ("NOCOUNT", _) => Ok(format!("-- SET NOCOUNT {val}: no Db2 for i equivalent needed")),
            ("XACT_ABORT", "ON") if self.mode == Mode::Top => self.misplaced("SET XACT_ABORT ON in a plain script: Db2 for i has no session switch for it; put the statements in a procedure or compound statement, where it becomes an EXIT handler"),
            ("XACT_ABORT", "ON") if self.seen_work => no("SET XACT_ABORT ON after other statements: a Db2 handler covers the whole compound statement; move the SET to the top, or wrap the statements after it in their own BEGIN … END"),
            ("XACT_ABORT", "ON") => {
                self.xact_abort = true;
                self.note(Status::Converted, "db2.xact-abort", "SET XACT_ABORT ON becomes an EXIT handler at the top (ROLLBACK; RESIGNAL;): any error rolls back the unit of work and ends the routine with that error, as XACT_ABORT ON does.");
                if self.mode == Mode::Do {
                    self.script_commit_note();
                }
                Ok("-- SET XACT_ABORT ON: see the EXIT handler at the top".into())
            }
            ("XACT_ABORT", _) => no("XACT_ABORT OFF: a Db2 for i routine stops at an unhandled error; continuing after it needs a CONTINUE handler around the statements that may fail"),
            ("ANSI_NULLS" | "QUOTED_IDENTIFIER" | "ANSI_PADDING" | "ANSI_WARNINGS" | "ARITHABORT" | "CONCAT_NULL_YIELDS_NULL", "ON") => {
                Ok(format!("-- SET {opt} ON: Db2 for i always behaves this way"))
            }
            ("TRANSACTION", _) if self.mode == Mode::Top => {
                if ts.iter().any(|t| t.is("SNAPSHOT")) {
                    return no("SNAPSHOT isolation: Db2 for i has no row versioning; readers wait for writers (or use *CHG to read uncommitted rows)");
                }
                self.note(Status::Assumptions, "ASM-DB2-ISOLATION", "Db2 for i maps the ISO level names to its commit levels: READ UNCOMMITTED = *CHG, READ COMMITTED = *CS, REPEATABLE READ = *ALL, SERIALIZABLE = *RR. Readers and writers block each other at slightly different points than in T-SQL.");
                Ok(self.expr(ts))
            }
            _ => no(format!("SET {opt}: no rule for this session option")),
        }
    }

    /// Db2: transaction statements in a script depend on the job's commit level.
    fn script_commit_note(&mut self) {
        self.note(Status::Assumptions, "ASM-DB2-SCRIPT-COMMIT", "The script runs under commitment control (for example RUNSQLSTM COMMIT(*CS), or a connection with autocommit off). Under *NONE COMMIT and ROLLBACK do nothing and every statement is final at once (facts V4, V5; V4 not checked on a real system).");
    }

    fn declare(&mut self, ts: &[T], n: &SyntaxNode) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if s.len() > 2 && (ts[s[2]].is("CURSOR") || ts[s[1]].is("CURSOR")) {
            return no(self.pick(
                "cursors: declare a PL/pgSQL cursor (or use a FOR loop over the query)",
                "cursors: declare an SQL PL cursor (or use a FOR loop over the query)",
            ));
        }
        if self.db2()
            && s.len() > 2
            && (ts[s[2]].is("TABLE")
                || ts[s[2]].is("AS") && s.get(3).is_some_and(|&j| ts[j].is("TABLE")))
        {
            return self.table_variable(&ts);
        }
        let mut lines = Vec::new();
        let mut pending = Vec::new(); // (variable, initialiser tokens)
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
                return no(self.pick(
                    "table variables: use a temporary table (note: it does not survive ROLLBACK the way a table variable does)",
                    "table variables: declare one table variable per DECLARE",
                ));
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
            lines.push(if self.db2() {
                format!("DECLARE {name} {ty};")
            } else {
                format!("{name} {ty};")
            });
            if let Some(eq) = eq {
                pending.push((name, it[eq + 1..].to_vec()));
            }
        }
        // Type notes belong to the hoisted declaration; initialiser notes to
        // the assignment left in place. Each note is counted once.
        let notes = self.take_notes();
        self.decls.push(Decl {
            src: Self::range(n),
            lines,
            notes,
        });
        let db2 = self.db2();
        let inits: Vec<String> = pending
            .iter()
            .map(|(name, init)| {
                let v = self.expr(init).trim().to_string();
                if db2 {
                    format!("SET {name} = {v};")
                } else {
                    format!("{name} := {v};")
                }
            })
            .collect();
        if inits.is_empty() {
            Ok(format!(
                "-- DECLARE {}: moved to the {}",
                split_commas(&ts[s[1]..])
                    .iter()
                    .filter_map(|it| trimmed(it).first().map(|t| t.s.clone()))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.pick("DECLARE section", "top of the block")
            ))
        } else {
            Ok(inits.join("\n"))
        }
    }

    /// Db2: `DECLARE @t TABLE (…)` becomes a NOT LOGGED global temporary
    /// table that keeps its rows on rollback, as a table variable does (V16).
    fn table_variable(&mut self, ts: &[T]) -> R<String> {
        let s = sig(ts);
        let v = &ts[s[1]];
        if v.k != VARIABLE || split_commas(&ts[s[1]..]).len() != 1 {
            return no("DECLARE form not understood");
        }
        let open = (s[2]..ts.len())
            .find(|&j| ts[j].k == L_PAREN)
            .ok_or(No("table variable without columns".into()))?;
        let close = matching(ts, open).ok_or(No("unbalanced parentheses".into()))?;
        if trimmed(&ts[close + 1..]).iter().any(|t| t.sig()) {
            return no("table variable options after the column list");
        }
        let name = format!("tv_{}", snake(&v.s[1..]));
        let mut cols = Vec::new();
        let pk = primary_key_columns(&ts[open + 1..close]);
        for item in split_commas(&ts[open + 1..close]) {
            cols.push(self.table_item(item, &pk)?);
        }
        self.tables
            .insert(v.s[1..].to_ascii_lowercase(), format!("SESSION.{name}"));
        self.note(Status::Assumptions, "ASM-TABLE-VARIABLE", "The table variable becomes a NOT LOGGED global temporary table with ON ROLLBACK PRESERVE ROWS, so a rollback keeps its rows as T-SQL does (fact V16; under *NONE the ON ROLLBACK clause does not apply). It lives until the job ends rather than the batch; WITH REPLACE empties it when the code runs again.");
        Ok(format!(
            "DECLARE GLOBAL TEMPORARY TABLE SESSION.{name} ({}\n) ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE",
            cols.join(",")
        ))
    }

    fn ret(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let val = s.get(1).map(|&j| self.expr(&ts[j..]).trim().to_string());
        match (self.mode, val) {
            (Mode::Func, Some(v)) => Ok(format!("RETURN {v}")),
            (Mode::Proc, Some(v)) if self.db2() => {
                self.return_code = true;
                Ok(format!("RETURN {v}"))
            }
            (Mode::Do, _) if self.db2() => self.misplaced("RETURN in a script: a Db2 for i compound statement cannot return; put a label on the block and LEAVE it"),
            (Mode::Proc, Some(v)) => {
                self.return_code = true;
                Ok(format!("return_code := {v};\nRETURN;"))
            }
            (Mode::Func, None) => self.misplaced("RETURN without a value in a function"),
            (_, None) => Ok("RETURN".into()),
            (_, Some(_)) => self.misplaced("RETURN with a value outside a procedure"),
        }
    }

    fn throw(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        if s.len() == 1 {
            if self.in_catch {
                return Ok(self.pick("RAISE", "RESIGNAL").into());
            }
            return self.misplaced("THROW without arguments outside CATCH");
        }
        let args = split_commas(&ts[s[1]..]);
        if args.len() != 3 {
            return no("THROW form not understood");
        }
        let msg = self.expr(args[1]).trim().to_string();
        if self.db2() {
            self.note(
                Status::Assumptions,
                "ASM-THROW-ERRCODE",
                "The T-SQL error number is not carried over: callers see SQLSTATE 75000 with the same message text.",
            );
            return Ok(format!("SIGNAL SQLSTATE '75000' SET MESSAGE_TEXT = {msg}"));
        }
        self.note(
            Status::Assumptions,
            "ASM-THROW-ERRCODE",
            "The T-SQL error number is not carried over: callers see SQLSTATE P0001 with the same message.",
        );
        Ok(format!(
            "RAISE EXCEPTION USING MESSAGE = {msg}, ERRCODE = 'P0001'"
        ))
    }

    fn txn(&mut self, kw: &str, ts: &[T]) -> R<String> {
        // A name after TRAN/TRANSACTION/WORK: a transaction or savepoint name.
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let name = s
            .iter()
            .position(|&j| ts[j].is("TRAN") || ts[j].is("TRANSACTION") || ts[j].is("WORK"))
            .and_then(|k| s.get(k + 1))
            .map(|&j| ts[j].s.to_ascii_lowercase());
        if kw == "ROLLBACK" {
            if let Some(n) = &name {
                if !self.db2() || !self.savepoints.contains(n) {
                    return no("ROLLBACK TRANSACTION name: rolls back to a savepoint or the named outer transaction; needs a rewrite");
                }
                if self.mode == Mode::Func {
                    return self.misplaced("transaction control inside a function");
                }
                self.savepoint_note();
                return Ok(format!("ROLLBACK TO SAVEPOINT {n}"));
            }
        }
        if self.db2() {
            return self.txn_db2(kw);
        }
        match self.mode {
            Mode::Top => {
                if kw == "BEGIN" {
                    self.note(Status::Assumptions, "ASM-NO-NESTED-TRAN", "PostgreSQL transactions do not nest: a second BEGIN only warns, where T-SQL increments @@TRANCOUNT.");
                }
                Ok(kw.to_string())
            }
            Mode::Func => {
                self.misplaced("transaction control inside a function is not allowed in PostgreSQL")
            }
            Mode::Proc | Mode::Do => {
                if self.in_try && kw != "BEGIN" {
                    return self.misplaced("COMMIT/ROLLBACK inside TRY: the PostgreSQL block with an EXCEPTION clause is a subtransaction, and committing inside it fails at runtime; move the transaction control outside the block");
                }
                if self.in_catch {
                    return self.misplaced("ROLLBACK/COMMIT inside an exception handler is not allowed in PostgreSQL; the failed block is already rolled back");
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

    /// Db2: transaction control. Under commitment control a unit of work is
    /// always open, so BEGIN TRAN has no statement of its own.
    fn txn_db2(&mut self, kw: &str) -> R<String> {
        match self.mode {
            Mode::Func => {
                self.misplaced("transaction control inside a function is not allowed in Db2 for i")
            }
            Mode::Top => {
                self.script_commit_note();
                if kw == "BEGIN" {
                    return Ok("-- BEGIN TRANSACTION: a unit of work is always open under commitment control".into());
                }
                Ok(kw.to_string())
            }
            Mode::Proc | Mode::Do => {
                if self.mode == Mode::Do {
                    self.script_commit_note();
                }
                self.note(Status::Decision, "db2.proc-transaction", "Under commitment control a unit of work is always open: BEGIN TRAN is dropped, and transactions do not nest (no @@TRANCOUNT). A COMMIT or ROLLBACK here ends the caller's unit of work too, where T-SQL's inner COMMIT only decrements @@TRANCOUNT.");
                if kw == "BEGIN" {
                    return Ok("-- BEGIN TRANSACTION: implicit under commitment control".into());
                }
                Ok(kw.to_string())
            }
        }
    }

    fn savepoint_note(&mut self) {
        self.note(Status::Assumptions, "ASM-FACT-V14", "Savepoints need commitment control (procedures are compiled with *CS) and cannot be reached across the boundary of an ATOMIC block (fact V14, only partly verified).");
    }

    /// Db2: `SAVE TRAN name` → `SAVEPOINT name`.
    fn save(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let Some(&j) = s.get(2) else {
            return no("SAVE TRANSACTION without a name");
        };
        if s.len() != 3 || ts[j].k == VARIABLE {
            return no("SAVE TRANSACTION with a variable name");
        }
        if self.mode == Mode::Func {
            return self.misplaced("transaction control inside a function");
        }
        if self.mode == Mode::Top {
            self.script_commit_note();
        }
        let name = ts[j].s.to_ascii_lowercase();
        self.savepoints.push(name.clone());
        self.savepoint_note();
        Ok(format!("SAVEPOINT {name} ON ROLLBACK RETAIN CURSORS"))
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
            return no(format!(
                "system procedures have no {} equivalent",
                self.to.display_name()
            ));
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
                if self.db2() {
                    if let Some(w) = ts.iter().find(|t| t.is("INCLUDE") || t.is("WHERE")) {
                        return no(format!("index with {}: not checked for Db2 for i; create a plain index (or an encoded vector or derived-key index) after review", w.up()));
                    }
                }
                let kept: Vec<T> = ts
                    .iter()
                    .filter(|t| !(t.is("CLUSTERED") || t.is("NONCLUSTERED")))
                    .cloned()
                    .collect();
                if ts.iter().any(|t| t.is("CLUSTERED")) {
                    let text = self.pick(
                        "PostgreSQL has no clustered indexes: this becomes a plain index (CLUSTER can reorder the table once). Results are the same; performance may differ.",
                        "Db2 for i has no clustered indexes: this becomes a plain index. Results are the same; performance may differ.",
                    ).to_string();
                    self.note(Status::Assumptions, "ASM-CLUSTERED", &text);
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
        let pk = if self.db2() {
            primary_key_columns(&ts[open + 1..close])
        } else {
            Vec::new()
        };
        for item in split_commas(&ts[open + 1..close]) {
            cols.push(self.table_item(item, &pk)?);
        }
        let tail = self.expr(&ts[close + 1..]);
        if temp && self.db2() {
            // LOGGED: a rollback undoes the rows, as for a #temp table.
            return Ok(format!(
                "DECLARE GLOBAL TEMPORARY TABLE {name} ({}\n) ON COMMIT PRESERVE ROWS LOGGED WITH REPLACE{}",
                cols.join(","),
                tail.trim_end()
            ));
        }
        Ok(format!(
            "CREATE {}TABLE {name} ({}\n){}",
            if temp { "TEMP " } else { "" },
            cols.join(","),
            tail.trim_end()
        ))
    }

    /// One column definition or table constraint, keeping its leading layout.
    /// `pk`: (Db2) columns of a table-level PRIMARY KEY, which Db2 wants NOT NULL.
    fn table_item(&mut self, item: &[T], pk: &[String]) -> R<String> {
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
        let pk_col = pk.contains(&tsql_unquote(&first.s).to_ascii_lowercase());
        Ok(format!("{lead}{}", self.column_def(it, pk_col)?))
    }

    /// `pk_col`: (Db2) the column is in a table-level PRIMARY KEY.
    fn column_def(&mut self, it: &[T], pk_col: bool) -> R<String> {
        let s = sig(it);
        let name = self.expr(&it[s[0]..=s[0]]);
        let Some(&ty_at) = s.get(1) else {
            return no("column without a type");
        };
        if it[ty_at].is("AS") {
            return no(self.pick(
                "computed column: use GENERATED ALWAYS AS (…) STORED after checking the expression",
                "computed column: Db2 for i has no generated expression columns; use a view or a trigger",
            ));
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
                return no(self.pick(
                    "column COLLATE: choose a PostgreSQL collation (a nondeterministic ICU collation for case-insensitive)",
                    "column COLLATE: Db2 for i compares by the program's sort sequence (SRTSEQ), not per column",
                ));
            }
            if t.is("DEFAULT") && self.db2() {
                // Db2 defaults are constants or special registers, unparenthesised.
                let v = next_sig(it, j).ok_or(No("DEFAULT without a value".into()))?;
                let call = next_sig(it, v).filter(|&p| it[v].k == IDENT && it[p].k == L_PAREN);
                let end = match call {
                    Some(p) => matching(it, p).ok_or(No("unbalanced DEFAULT".into()))?,
                    None if it[v].k == L_PAREN => {
                        matching(it, v).ok_or(No("unbalanced DEFAULT".into()))?
                    }
                    None => v,
                };
                let mut inner = trimmed(&it[v..=end]);
                while inner.len() > 2
                    && inner[0].k == L_PAREN
                    && matching(inner, 0) == Some(inner.len() - 1)
                {
                    inner = trimmed(&inner[1..inner.len() - 1]);
                }
                let val = self.expr(inner).trim().to_string();
                let constant = match inner {
                    [x] => matches!(x.k, NUMBER | STRING) || x.is("NULL"),
                    [m, x] => m.k == OP && m.s == "-" && x.k == NUMBER,
                    _ => false,
                };
                if !constant && val != "CURRENT TIMESTAMP" {
                    return no(format!("column DEFAULT {val}: Db2 for i defaults must be constants or special registers such as CURRENT TIMESTAMP; set the value in the INSERT or with a trigger"));
                }
                // Already converted: a token the expression walk copies as is.
                rest.push(T {
                    s: format!("DEFAULT {val}"),
                    k: NUMBER,
                });
                j = end + 1;
                continue;
            }
            rest.push(t.clone());
            j += 1;
        }
        let mut rest = collapse_spaces(&self.expr(&rest));
        if self.db2()
            && (pk_col || rest.to_ascii_uppercase().contains("PRIMARY KEY"))
            && !rest.to_ascii_uppercase().contains("NOT NULL")
        {
            // T-SQL makes primary key columns NOT NULL; Db2 wants it written.
            rest = format!("NOT NULL {}", rest.trim());
        }
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
        let first = sig(&ts)[0];
        if find_word(&ts, first + 1, "ALTER").is_some() {
            return no(self.pick(
                "ALTER COLUMN: PostgreSQL uses ALTER COLUMN … TYPE / SET NOT NULL as separate actions",
                "ALTER COLUMN: Db2 for i uses ALTER COLUMN … SET DATA TYPE / SET NOT NULL as separate actions",
            ));
        }
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
                    self.column_def(trimmed(c), false)
                        .map(|d| format!("ADD COLUMN {d}"))
                })
                .collect();
            // Db2 lists several alterations without commas.
            let sep = self.pick(", ", " ");
            let head = head
                .trim_end()
                .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                .trim_end();
            return Ok(format!("{head} {}", cols?.join(sep)));
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
            let reason = match (up.as_str(), self.db2()) {
                ("@@ROWCOUNT", false) => "@@ROWCOUNT: use GET DIAGNOSTICS n = ROW_COUNT right after the statement (or FOUND)",
                ("@@ROWCOUNT", true) => "@@ROWCOUNT: use GET DIAGNOSTICS n = ROW_COUNT right after the statement",
                ("@@ERROR", false) => "@@ERROR: PostgreSQL stops at the error instead; use BEGIN … EXCEPTION",
                ("@@ERROR", true) => "@@ERROR: a Db2 for i routine stops at the error instead; use a handler",
                ("@@TRANCOUNT", false) => "@@TRANCOUNT: PostgreSQL transactions do not nest",
                ("@@TRANCOUNT", true) => "@@TRANCOUNT: Db2 for i transactions do not nest",
                ("@@IDENTITY", false) => "@@IDENTITY: use INSERT … RETURNING",
                ("@@IDENTITY", true) => "@@IDENTITY: use IDENTITY_VAL_LOCAL() or SELECT … FROM FINAL TABLE (INSERT …)",
                (_, false) => "T-SQL global variable with no PostgreSQL equivalent",
                (_, true) => "T-SQL global variable with no Db2 for i equivalent",
            };
            self.note(
                Status::NotConverted,
                &format!("tsql.{}", up.trim_start_matches('@').to_ascii_lowercase()),
                reason,
            );
            return v.to_string();
        }
        if let Some(name) = self.declared(v) {
            return name;
        }
        if let Some(t) = self
            .tables
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
        {
            return t.clone();
        }
        self.note(Status::Assumptions, "ASM-UNDECLARED-VAR", UNDECLARED);
        undeclared_name(v)
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
                        let text = self.pick(
                            "The dbo schema is dropped: objects are assumed to be found through search_path (usually the public schema).",
                            "The dbo schema is dropped: objects are assumed to be found through the default schema (CURRENT SCHEMA, or the library list with system naming).",
                        ).to_string();
                        self.note(Status::Assumptions, "ASM-DBO-SCHEMA", &text);
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
                            let text = format!(
                                "Global temporary tables (##t) have no {} equivalent.",
                                self.to.display_name()
                            );
                            self.note(Status::NotConverted, "tsql.global-temp", &text);
                            o.push_str(&t.s);
                        } else if self.db2() {
                            self.note(Status::Assumptions, "ASM-TEMP-TABLE", "#temp tables become declared global temporary tables (SESSION.name, in QTEMP), LOGGED so a rollback undoes their changes as for #temp, with ON COMMIT PRESERVE ROWS. They live until the job ends (T-SQL drops them when the creating procedure ends); WITH REPLACE lets a second run in the same job recreate them.");
                            o.push_str("SESSION.");
                            o.push_str(rest);
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
        if self.db2() {
            self.note(Status::Assumptions, "ASM-LISTAGG", "STRING_AGG becomes LISTAGG, whose result has a maximum length (it raises an error when the list is longer) where STRING_AGG over (N)VARCHAR(MAX) has none. Equal while the lists stay short; the Db2 for i limit was not checked.");
        }
        let w = next_sig(ts, close).filter(|&j| ts[j].is("WITHIN"));
        let Some(w) = w else {
            return Some((
                format!("{}({x}, {sep})", self.pick("string_agg", "LISTAGG")),
                close,
            ));
        };
        let g = next_sig(ts, w).filter(|&j| ts[j].is("GROUP"))?;
        let p = next_sig(ts, g).filter(|&j| ts[j].k == L_PAREN)?;
        let r = matching(ts, p)?;
        let order = self.expr(&ts[p + 1..r]).trim().to_string();
        if self.db2() {
            return Some((format!("LISTAGG({x}, {sep}) WITHIN GROUP ({order})"), r));
        }
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
            self.ci_note();
        }
    }

    fn ci_note(&mut self) {
        let text = self.pick(
            "String comparison: T-SQL's default collation is case-insensitive and ignores trailing spaces; PostgreSQL compares case-sensitively. Equal only if the data's case is consistent (or use a case-insensitive collation).",
            "String comparison: T-SQL's default collation is case-insensitive; Db2 for i compares by the sort sequence, case-sensitively with the default *HEX. Both ignore trailing blanks (fact V8). Equal only if the data's case is consistent (or the routine uses SRTSEQ(*LANGIDSHR)).",
        ).to_string();
        self.note(Status::Assumptions, "ASM-CI-COLLATION", &text);
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
            let text = self.pick(
                "Lock hints were dropped. To keep the locking, add FOR UPDATE (UPDLOCK), FOR UPDATE SKIP LOCKED (READPAST) or a stricter isolation level (HOLDLOCK/SERIALIZABLE).",
                "Lock hints were dropped. To keep the locking, add FOR UPDATE (UPDLOCK), SKIP LOCKED DATA (READPAST), an isolation clause such as WITH RR (HOLDLOCK/SERIALIZABLE), or USE AND KEEP EXCLUSIVE LOCKS (its rules on SELECT INTO were not checked, fact V15).",
            ).to_string();
            self.note(Status::Decision, "tsql.lock-hint", &text);
        } else if words
            .iter()
            .any(|w| w == "NOLOCK" || w == "READUNCOMMITTED")
        {
            let text = self.pick(
                "NOLOCK was dropped: PostgreSQL never reads uncommitted rows. Results differ only while other transactions are writing.",
                "NOLOCK was dropped: under *CS only committed rows are read; add WITH UR to the query to read uncommitted rows as NOLOCK does. Results differ only while other transactions are writing.",
            ).to_string();
            self.note(Status::Assumptions, "ASM-NOLOCK", &text);
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
        if self.db2() {
            self.note(Status::Assumptions, "ASM-TIME-SOURCE", "Current time: both evaluate once per statement (fact V20), but Db2 for i uses the job's time zone where T-SQL uses the server clock. Equal if the job runs in the server's time zone. CURRENT TIMESTAMP has 6 fractional digits.");
            return "CURRENT TIMESTAMP".into();
        }
        self.note(Status::Assumptions, "ASM-TIME-SOURCE", "Current time: both use the statement start, but PostgreSQL uses the session time zone where T-SQL uses the server clock. Equal if the session time zone is the server's.");
        "statement_timestamp()::timestamp".into()
    }

    /// Built-in function rewrites. `None` keeps the call as written.
    fn func(&mut self, name: &str, args: &[T]) -> Option<String> {
        if self.db2() {
            return self.func_db2(name, args);
        }
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
                self.ci_note();
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
                self.in_cast = true;
                let ty = self.ty(&a[0][at + 1..]);
                self.in_cast = false;
                let (ty, _, _) = ty.ok()?;
                format!("CAST({x} AS {ty})")
            }
            ("CONVERT", 2) => {
                self.in_cast = true;
                let ty = self.ty(a[0]);
                self.in_cast = false;
                let (ty, _, _) = ty.ok()?;
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

    /// Db2 for i built-in rewrites. `None` keeps the call as written.
    fn func_db2(&mut self, name: &str, args: &[T]) -> Option<String> {
        let a = split_commas(args);
        let n = a.len();
        let e = |cx: &mut Self, k: usize| cx.expr(a[k]).trim().to_string();
        Some(match (name, n) {
            ("GETDATE" | "SYSDATETIME", 0) => self.now(),
            ("GETUTCDATE" | "SYSUTCDATETIME", 0) => {
                self.note(Status::Assumptions, "ASM-DB2-UTC", "UTC time becomes CURRENT TIMESTAMP - CURRENT TIMEZONE, using the job's current UTC offset; around a daylight-saving change the result can be off by the change. Db2 for i UTC helpers were not checked (fact V13).");
                "(CURRENT TIMESTAMP - CURRENT TIMEZONE)".into()
            }
            ("ISNULL", 2) => {
                self.note(Status::Assumptions, "ASM-ISNULL-TYPE", "ISNULL becomes COALESCE: ISNULL takes the type of its first argument (it can truncate), COALESCE the widest type. Equal unless the fallback is longer or wider than the first argument.");
                let (x, y) = (e(self, 0), e(self, 1));
                format!("COALESCE({x}, {y})")
            }
            ("LEN", 1) => {
                self.note(Status::Assumptions, "ASM-DB2-LENGTH", "LEN becomes LENGTH(RTRIM(…)), which counts characters for single-byte and NVARCHAR (UTF-16) data but bytes for UTF-8 (CCSID 1208) columns.");
                format!("LENGTH(RTRIM({}))", e(self, 0))
            }
            ("CHARINDEX", 2) => {
                self.ci_note();
                let (x, y) = (e(self, 0), e(self, 1));
                format!("LOCATE({x}, {y})")
            }
            ("NEWID", 0) => {
                self.note(Status::Assumptions, "ASM-UUID-TEXT", "NEWID() becomes GENERATE_UUID(): a version 4 UUID as CHAR(36) in upper case, as a uniqueidentifier reads when converted to text (fact V13). It is a string, so it compares case-sensitively. Needs IBM i 7.6, or 7.4/7.5 with the Technology Refresh that added it.");
                "GENERATE_UUID()".into()
            }
            ("IIF", 3) => {
                let (c, x, y) = (e(self, 0), e(self, 1), e(self, 2));
                format!("(CASE WHEN {c} THEN {x} ELSE {y} END)")
            }
            ("REPLICATE", 2) => {
                let (x, y) = (e(self, 0), e(self, 1));
                format!("REPEAT({x}, {y})")
            }
            ("SCOPE_IDENTITY", 0) => {
                self.note(Status::Assumptions, "ASM-IDENTITY-VAL-LOCAL", "SCOPE_IDENTITY() becomes IDENTITY_VAL_LOCAL(): both give the last identity value assigned at the same nesting level. Assumes the INSERT before it is a single-row INSERT … VALUES; after a multi-row INSERT the two may differ (not checked).");
                "IDENTITY_VAL_LOCAL()".into()
            }
            ("ERROR_MESSAGE", 0) if self.in_catch => "v_error_message".into(),
            ("ERROR_NUMBER", 0) if self.in_catch => {
                self.note(Status::Decision, "db2.sqlcode", "ERROR_NUMBER() becomes the SQLCODE of the error (DB2_RETURNED_SQLCODE): a Db2 number, not T-SQL's, so comparisons against T-SQL error numbers must be rewritten.");
                "v_error_number".into()
            }
            ("CAST", 1) => {
                let at = crate::tok::find_word(a[0], 0, "AS")?;
                let x = self.expr(&a[0][..at]).trim().to_string();
                self.in_cast = true;
                let ty = self.ty(&a[0][at + 1..]);
                self.in_cast = false;
                let (ty, _, numeric) = ty.ok()?;
                if numeric {
                    self.cast_note(&a[0][..at]);
                }
                format!("CAST({x} AS {ty})")
            }
            ("CONVERT", 2) => {
                self.in_cast = true;
                let ty = self.ty(a[0]);
                self.in_cast = false;
                let (ty, _, numeric) = ty.ok()?;
                if numeric {
                    self.cast_note(a[1]);
                }
                format!("CAST({} AS {ty})", e(self, 1))
            }
            ("CONVERT", 3) => {
                self.note(Status::NotConverted, "tsql.convert-style", "CONVERT with a style code: use VARCHAR_FORMAT / TIMESTAMP_FORMAT with the matching format.");
                return None;
            }
            ("DATEADD", 3) => {
                let part = date_part(&self.expr(a[0]))?;
                let (k, d) = (e(self, 1), e(self, 2));
                if matches!(part, "month" | "year") {
                    self.note(Status::Assumptions, "ASM-DB2-MONTH-END", "Adding months or years: both move the day to the end of a shorter month (Jan 31 + 1 month = Feb 28 or 29); Db2 also returns warning 01506, which does not stop the routine. Not in the verified vendor facts.");
                }
                format!("({d} + ({k}) {}S)", part.to_ascii_uppercase())
            }
            ("DATEPART", 2) => {
                let part = date_part(&self.expr(a[0]))?;
                format!("{}({})", part.to_ascii_uppercase(), e(self, 1))
            }
            ("DATEDIFF", 3)
                if matches!(date_part(&self.expr(a[0])), Some("day" | "month" | "year")) =>
            {
                // Boundary counts, exactly as T-SQL defines them.
                let part = date_part(&self.expr(a[0])).unwrap();
                let (x, y) = (e(self, 1), e(self, 2));
                match part {
                    "day" => format!("(DAYS({y}) - DAYS({x}))"),
                    "year" => format!("(YEAR({y}) - YEAR({x}))"),
                    _ => format!("((YEAR({y}) - YEAR({x})) * 12 + MONTH({y}) - MONTH({x}))"),
                }
            }
            ("DATEDIFF", 3) => {
                self.note(Status::NotConverted, "tsql.datediff", "DATEDIFF counts boundaries crossed (DATEDIFF(year, '2024-12-31', '2025-01-01') = 1); Db2 date subtraction gives elapsed durations. Needs a per-unit rewrite.");
                return None;
            }
            ("DATALENGTH", 1) => {
                self.note(
                    Status::NotConverted,
                    "tsql.datalength",
                    "DATALENGTH: byte counts depend on the column's CCSID; no rule yet.",
                );
                return None;
            }
            (
                "OBJECT_ID" | "FORMAT" | "STR" | "STUFF" | "PATINDEX" | "DATENAME" | "SUSER_SNAME"
                | "XACT_STATE" | "ERROR_SEVERITY" | "ERROR_STATE" | "ERROR_LINE"
                | "ERROR_PROCEDURE" | "ERROR_MESSAGE" | "ERROR_NUMBER",
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

    /// Db2: a cast to a number of something that may be a string (fact V9).
    fn cast_note(&mut self, operand: &[T]) {
        let op = trimmed(operand);
        if let [t] = op {
            if t.k == NUMBER || self.operand_kind(op, Some(0)).1 {
                return;
            }
        }
        self.note(Status::Assumptions, "ASM-FACT-V9", "Cast to a number: T-SQL turns '' into 0, Db2 for i is expected to raise SQLSTATE 22018 (fact V9, not checked on a real system). Equal if the value is never an empty or blank string.");
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
        if self.db2() {
            return self.ty_db2(&up, &args, max, ts);
        }
        // T-SQL defaults when the length or precision is left out.
        let len_default = if self.in_cast { "30" } else { "1" };
        let sized = |a: &[String]| {
            if a.is_empty() {
                format!("({len_default})")
            } else {
                format!("({})", a.join(", "))
            }
        };
        let frac = |cx: &mut Self, a: &[String]| -> String {
            match a.first().map(|p| p.parse::<u32>().unwrap_or(7)) {
                Some(p) if p <= 6 => format!("({p})"),
                _ => {
                    cx.note(Status::Assumptions, "ASM-FRACTIONAL-SECONDS", "T-SQL keeps 7 fractional digits (100 ns) by default; PostgreSQL keeps at most 6 (microseconds), so the last digit is rounded.");
                    String::new()
                }
            }
        };
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
            "DECIMAL" | "NUMERIC" | "DEC" if args.is_empty() => num("numeric(18,0)"),
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
            "DATETIME2" => (format!("timestamp{}", frac(self, &args)), false, false),
            "SMALLDATETIME" => ("timestamp(0)".into(), false, false),
            "DATE" => ("date".into(), false, false),
            "TIME" => (format!("time{}", frac(self, &args)), false, false),
            "DATETIMEOFFSET" => {
                self.note(Status::Assumptions, "ASM-DATETIMEOFFSET", "DATETIMEOFFSET becomes timestamptz, which stores the instant but not the original offset.");
                (format!("timestamptz{}", paren(&args)), false, false)
            }
            "CHAR" | "NCHAR" => txt(format!("char{}", sized(&args))),
            "VARCHAR" | "NVARCHAR" if max => txt("text".into()),
            "VARCHAR" | "NVARCHAR" => txt(format!("varchar{}", sized(&args))),
            "TEXT" | "NTEXT" => txt("text".into()),
            "SYSNAME" => txt("varchar(128)".into()),
            "VARBINARY" | "BINARY" if !max => {
                self.note(Status::Assumptions, "ASM-BINARY-LENGTH", "bytea has no length limit: values longer than the T-SQL length are no longer rejected, and BINARY(n) values are no longer zero-padded.");
                ("bytea".into(), false, false)
            }
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

    /// Maps a T-SQL type to Db2 for i: (type, is string, is numeric).
    fn ty_db2(
        &mut self,
        up: &str,
        args: &[String],
        max: bool,
        ts: &[T],
    ) -> R<(String, bool, bool)> {
        let len_default = if self.in_cast { "30" } else { "1" };
        let sized = |a: &[String]| {
            if a.is_empty() {
                format!("({len_default})")
            } else {
                format!("({})", a.join(", "))
            }
        };
        let num = |t: &str| (t.to_string(), false, true);
        let txt = |t: String| (t, true, false);
        let other = |t: String| (t, false, false);
        let lob = |cx: &mut Self| {
            cx.note(Status::Decision, "db2.lob", "(N)VARCHAR(MAX) and (N)TEXT become CLOB/NCLOB, which cannot be compared, grouped or indexed directly. If the values stay short, choose VARCHAR(n) (up to 32739 bytes) instead.");
        };
        Ok(match up {
            "INT" | "INTEGER" => num("INTEGER"),
            "BIGINT" => num("BIGINT"),
            "SMALLINT" => num("SMALLINT"),
            "TINYINT" => {
                self.note(Status::Assumptions, "ASM-TINYINT", "TINYINT becomes SMALLINT: values outside 0–255 are no longer rejected.");
                num("SMALLINT")
            }
            "BIT" => {
                self.note(Status::Assumptions, "ASM-BIT-SMALLINT", "BIT becomes SMALLINT, which keeps comparisons with 1 and 0: T-SQL stores any non-zero value as 1, SMALLINT stores it as given. Equal if only 0, 1 and NULL are written.");
                num("SMALLINT")
            }
            "DECIMAL" | "NUMERIC" | "DEC" if args.is_empty() => num("DECIMAL(18, 0)"),
            "DECIMAL" | "NUMERIC" | "DEC" => (format!("DECIMAL({})", args.join(", ")), false, true),
            "MONEY" => num("DECIMAL(19, 4)"),
            "SMALLMONEY" => num("DECIMAL(10, 4)"),
            "FLOAT" => {
                if args.first().and_then(|a| a.parse::<u32>().ok()).is_some_and(|p| p <= 24) {
                    num("REAL")
                } else {
                    num("DOUBLE")
                }
            }
            "REAL" => num("REAL"),
            "DATETIME" => {
                self.note(Status::Assumptions, "ASM-DATETIME-PRECISION", "DATETIME becomes TIMESTAMP(3): T-SQL rounds DATETIME to 1/300 s, Db2 for i keeps milliseconds exactly.");
                other("TIMESTAMP(3)".into())
            }
            "DATETIME2" => other(format!(
                "TIMESTAMP({})",
                args.first().map_or("7", String::as_str)
            )),
            "SMALLDATETIME" => {
                self.note(Status::Assumptions, "ASM-SMALLDATETIME", "SMALLDATETIME becomes TIMESTAMP(0): T-SQL rounds to the minute, TIMESTAMP(0) keeps the seconds.");
                other("TIMESTAMP(0)".into())
            }
            "DATE" => other("DATE".into()),
            "TIME" => {
                if args.first().map_or(true, |p| p != "0") {
                    self.note(Status::Assumptions, "ASM-DB2-TIME-FRACTION", "Db2 for i TIME has no fractional seconds, so they are dropped. Use TIMESTAMP where they matter.");
                }
                other("TIME".into())
            }
            "DATETIMEOFFSET" => return no("DATETIMEOFFSET: Db2 for i has no type with a time zone; store a UTC TIMESTAMP and the offset"),
            "CHAR" => txt(format!("CHAR{}", sized(args))),
            "NCHAR" => txt(format!("NCHAR{}", sized(args))),
            "VARCHAR" if max => {
                lob(self);
                txt("CLOB(2G)".into())
            }
            "NVARCHAR" if max => {
                lob(self);
                txt("NCLOB(1G)".into())
            }
            "VARCHAR" => txt(format!("VARCHAR{}", sized(args))),
            "NVARCHAR" => txt(format!("NVARCHAR{}", sized(args))),
            "TEXT" => {
                lob(self);
                txt("CLOB(2G)".into())
            }
            "NTEXT" => {
                lob(self);
                txt("NCLOB(1G)".into())
            }
            "SYSNAME" => txt("NVARCHAR(128)".into()),
            "VARBINARY" | "BINARY" if !max => other(format!("{up}{}", sized(args))),
            "VARBINARY" | "IMAGE" => other("BLOB(2G)".into()),
            "UNIQUEIDENTIFIER" => {
                self.note(Status::Assumptions, "ASM-UUID-CHAR", "UNIQUEIDENTIFIER becomes CHAR(36): comparisons are case-sensitive and ordering is textual (T-SQL orders GUIDs by byte groups). GENERATE_UUID() and T-SQL's text form are both upper case (fact V13).");
                txt("CHAR(36)".into())
            }
            "XML" => other("XML".into()),
            "ROWVERSION" | "TIMESTAMP" => return no("ROWVERSION/TIMESTAMP: use a row change timestamp column (TIMESTAMP GENERATED ALWAYS FOR EACH ROW ON UPDATE AS ROW CHANGE TIMESTAMP)"),
            "SQL_VARIANT" | "HIERARCHYID" | "GEOGRAPHY" | "GEOMETRY" | "CURSOR" | "TABLE" => return no(format!("type {up} has no Db2 for i equivalent rule yet")),
            _ => {
                self.note(Status::Assumptions, "ASM-USER-TYPE", "A type without a rule was kept as written: it must exist in Db2 for i under that name.");
                let t: String = ts.iter().map(|t| t.s.clone()).collect();
                other(self.expr_plain(&t))
            }
        })
    }

    fn expr_plain(&self, s: &str) -> String {
        s.replace("dbo.", "")
    }
}

/// Whether a batch-level unit needs PL/pgSQL (variables or control flow).
/// Note text for a variable used without a declaration.
const UNDECLARED: &str =
    "A variable is used without a declaration in this unit; it was renamed with the v_ prefix.";

fn undeclared_name(v: &str) -> String {
    format!("v_{}", snake(v.trim_start_matches('@')))
}

/// Adds `BEGIN END;` to a statement list with no statement left in it.
fn fill_empty(b: &mut Out, indent: usize) {
    let live = b.text.lines().any(|l| {
        let l = l.trim();
        !l.is_empty() && !l.starts_with("--")
    });
    if !live {
        b.lines(indent, "BEGIN END;");
    }
}

/// Db2 for i needs a FROM: a SELECT without one (`(SELECT @id AS id)`) gets
/// `FROM SYSIBM.SYSDUMMY1`, the one-row table. Only nested SELECTs unless
/// `top`.
fn dummy_from(ts: &[T], top: bool) -> Vec<T> {
    let ds = crate::tok::depths(ts);
    let mut at = Vec::new(); // insertion points
    for (i, t) in ts.iter().enumerate() {
        let d = ds[i];
        if !t.is("SELECT") || d == 0 && !top {
            continue;
        }
        let mut end = ts.len();
        let mut from = false;
        let mut clause = None;
        for j in i + 1..ts.len() {
            if ds[j] < d || ts[j].k == R_PAREN && ds[j] == d - 1 || ts[j].k == SEMICOLON {
                end = j;
                break;
            }
            if ds[j] != d {
                continue;
            }
            let w = ts[j].up();
            if ts[j].k == IDENT && matches!(w.as_str(), "UNION" | "EXCEPT" | "INTERSECT") {
                end = j;
                break;
            }
            if ts[j].is("FROM") || ts[j].is("INTO") {
                from = true;
                break;
            }
            if clause.is_none()
                && ts[j].k == IDENT
                && matches!(w.as_str(), "WHERE" | "GROUP" | "HAVING" | "ORDER")
            {
                clause = Some(j);
            }
        }
        if !from {
            let mut p = clause.unwrap_or(end);
            while p > i + 1 && !ts[p - 1].sig() {
                p -= 1;
            }
            at.push(p);
        }
    }
    let mut out = Vec::with_capacity(ts.len() + at.len());
    for (i, t) in ts.iter().enumerate() {
        if at.contains(&i) {
            out.push(T {
                k: NUMBER, // copied as is by the expression walk
                s: " FROM SYSIBM.SYSDUMMY1".into(),
            });
        }
        out.push(t.clone());
    }
    if at.contains(&ts.len()) {
        out.push(T {
            k: NUMBER,
            s: " FROM SYSIBM.SYSDUMMY1".into(),
        });
    }
    out
}

/// Lower-case column names of a table-level `PRIMARY KEY (…)` in a column list.
fn primary_key_columns(items: &[T]) -> Vec<String> {
    let mut out = Vec::new();
    for item in split_commas(items) {
        let it = trimmed(item);
        let s = sig(it);
        let Some(p) = s.iter().position(|&j| it[j].is("PRIMARY")) else {
            continue;
        };
        if !matches!(it[s[0]].up().as_str(), "CONSTRAINT" | "PRIMARY") {
            continue; // a column's own PRIMARY KEY
        }
        let Some(open) = (s[p]..it.len()).find(|&j| it[j].k == L_PAREN) else {
            continue;
        };
        let close = matching(it, open).unwrap_or(it.len());
        for c in split_commas(&it[open + 1..close]) {
            if let Some(t) = trimmed(c).first() {
                out.push(tsql_unquote(&t.s).to_ascii_lowercase());
            }
        }
    }
    out
}

/// The first keyword of a unit, upper case.
fn first_word(n: &SyntaxNode) -> String {
    tokens(n)
        .iter()
        .find(|t| t.sig())
        .map(|t| t.up())
        .unwrap_or_default()
}

/// The unit after `els[i]` when only whitespace and semicolons stand between
/// them (a comment or anything else keeps them apart).
fn adjacent_unit(els: &[SyntaxElement], i: usize) -> Option<SyntaxNode> {
    for el in &els[i + 1..] {
        match el {
            SyntaxElement::Node(n) if n.kind().is_unit() => return Some(n.clone()),
            SyntaxElement::Token(t) if matches!(t.kind(), WHITESPACE | SEMICOLON) => {}
            _ => return None,
        }
    }
    None
}

impl From<rules::Status> for Status {
    fn from(s: rules::Status) -> Self {
        match s {
            rules::Status::Converted => Status::Converted,
            rules::Status::Assumptions => Status::Assumptions,
            rules::Status::Decision => Status::Decision,
            rules::Status::Differs => Status::Differs,
            rules::Status::NotConverted => Status::NotConverted,
        }
    }
}

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
