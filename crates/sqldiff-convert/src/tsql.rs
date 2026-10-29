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

use sqldiff_core::catalog::ParamMode;

use crate::out::Out;
use crate::tok::{
    find_word, matching, next_sig, prev_sig, sig, snake, split_commas, tokens, trimmed, T,
};
use crate::{Note, Status};

mod bits;
mod paging;
mod temporal;
mod trigger;

const INDENT: usize = 2;

const BIT_SMALLINT: &str = "BIT becomes SMALLINT, which keeps comparisons with 1 and 0: T-SQL stores any non-zero value as 1, SMALLINT stores it as given (literals are written as 0 or 1). Equal if only 0, 1 and NULL are written.";

/// Rule id of a refusal because of where the unit stands.
const CONTEXT: &str = "not-converted.context";

/// The idiom rules (`sqldiff_core::rules`), for what the token rules refuse.
mod rules {
    pub use sqldiff_core::rules::convert::{convert_nodes, Options, Piece};
    pub use sqldiff_core::rules::rewrite::NoFacts;
    pub use sqldiff_core::rules::rule::{RuleSet, Status};
}

pub(crate) fn convert(
    root: &SyntaxNode,
    src: &str,
    to: Dialect,
    catalog: Option<&sqldiff_core::catalog::Catalog>,
) -> Out {
    let mut cx = Cx {
        src,
        to,
        catalog,
        loops: Vec::new(),
        labels: 0,
        cursors: Vec::new(),
        xact_abort: false,
        seen_work: false,
        tables: HashMap::new(),
        savepoints: Vec::new(),
        vars: HashMap::new(),
        notes: Vec::new(),
        trace: Vec::new(),
        stepped: 0,
        mode: Mode::Top,
        in_catch: false,
        in_try: false,
        in_cast: false,
        return_code: false,
        txn_control: false,
        decls: Vec::new(),
        context: false,
        dynamic: Vec::new(),
        scope: None,
        local: std::cell::OnceCell::new(),
        bit_cols: Vec::new(),
        bit_unhandled: Vec::new(),
        bit_outer: None,
        bit_aliases: Vec::new(),
        ret_bit: false,
        view_query: false,
        trigger: None,
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
    // Completeness of the trace: a unit whose text the rules changed (beyond
    // layout, terminators and letter case) names at least one rule.
    #[cfg(debug_assertions)]
    for u in &out.units {
        let a = crate::trace_text(src.get(u.src.0..u.src.1).unwrap_or(""));
        let b = crate::trace_text(out.text.get(u.out.0..u.out.1).unwrap_or(""));
        assert!(
            a == b || !u.trace.is_empty(),
            "the rules rewrote {:?} as {:?} without a trace step",
            src.get(u.src.0..u.src.1).unwrap_or(""),
            out.text.get(u.out.0..u.out.1).unwrap_or("")
        );
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
    /// The T-SQL type, for DATEADD.
    dt: Dt,
}

/// A hoisted declaration: the DECLARE statement's span and the lines it adds.
struct Decl {
    src: (usize, usize),
    lines: Vec<String>,
    notes: Vec<Note>,
    trace: Vec<crate::Step>,
}

struct Cx<'a> {
    src: &'a str,
    /// Target dialect: PostgreSQL or Db2 for i.
    to: Dialect,
    /// The source catalog of the whole tree being converted, when there is one.
    catalog: Option<&'a sqldiff_core::catalog::Catalog>,
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
    /// The rewrites made in the unit being converted (`step`).
    trace: Vec<crate::Step>,
    /// How many times `step` was called, deduplicated or not: lets the
    /// expression walk see whether a branch recorded its rewrite.
    stepped: usize,
    mode: Mode,
    in_catch: bool,
    /// Inside the TRY part of a `BEGIN … EXCEPTION` block (a subtransaction).
    in_try: bool,
    /// Mapping the target type of CAST/CONVERT (string lengths default to 30 there, 1 elsewhere).
    in_cast: bool,
    /// A procedure returns a value somewhere: it gets an INOUT `return_code`.
    return_code: bool,
    /// PostgreSQL: the procedure body COMMITs or ROLLs BACK, which a
    /// SECURITY DEFINER procedure may not.
    txn_control: bool,
    decls: Vec<Decl>,
    /// The unit being converted was refused because of where it stands, not
    /// for want of a rule: the idiom rules, which see statements out of
    /// context, must not take it either.
    context: bool,
    /// Variables (lower case, without `@`) whose value is run as dynamic SQL
    /// in this routine or script: `EXEC (@sql)`, `sp_executesql @sql`.
    dynamic: Vec<String>,
    /// The tables of the statement being converted, for column types.
    scope: Option<Scope>,
    /// Without a tree catalog: one read from this input, made when needed.
    local: std::cell::OnceCell<sqldiff_core::catalog::Catalog>,
    /// The BIT columns of the table being created or altered (lower case).
    bit_cols: Vec<String>,
    /// PostgreSQL: places where a bit meets a number with no exact rewrite.
    bit_unhandled: Vec<String>,
    /// Edits the next `expr` call applies to its slice, around its own
    /// (a value stored in a bit, from `bit_store_text`).
    bit_outer: Option<bits::BitEdits>,
    /// Names the statement gives a bit in a select list (`IsActive AS a`),
    /// lower case: a derived table's bit column, read outside it.
    bit_aliases: Vec<String>,
    /// The function being converted returns BIT.
    ret_bit: bool,
    /// The query of a view is being converted: its ORDER BY does not order
    /// the view's rows.
    view_query: bool,
    /// The trigger whose body is being converted, for one of its events.
    trigger: Option<trigger::Trig>,
}

/// A refusal: the unit is kept as a comment with this reason.
struct No(String);

type R<T> = Result<T, No>;

fn no<T>(reason: impl Into<String>) -> R<T> {
    Err(No(reason.into()))
}

impl<'a> Cx<'a> {
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

    /// Records that `rule` rewrote the input bytes `span`: every rewrite
    /// leaves a step, with or without a note (see `crate::Step`).
    fn step(&mut self, rule: &str, span: (usize, usize)) {
        self.step_with(rule, span, None);
    }

    /// `step` with what the rule was applied with.
    fn step_with(&mut self, rule: &str, span: (usize, usize), inst: Option<String>) {
        self.stepped += 1;
        let st = crate::Step {
            rule: rule.to_string(),
            start: span.0,
            end: span.1,
            inst,
        };
        if !self.trace.contains(&st) {
            self.trace.push(st);
        }
    }

    /// `step` over the input the significant tokens of `ts` cover; a slice
    /// of trivia alone names its first token's place.
    fn step_ts(&mut self, rule: &str, ts: &[T]) {
        let sp = crate::tok::span(ts)
            .or_else(|| ts.first().map(|t| t.pos))
            .unwrap_or((0, 0));
        self.step(rule, sp);
    }

    /// `step_ts` with what the rule was applied with.
    fn step_ts_with(&mut self, rule: &str, ts: &[T], inst: impl Into<String>) {
        let sp = crate::tok::span(ts)
            .or_else(|| ts.first().map(|t| t.pos))
            .unwrap_or((0, 0));
        self.step_with(rule, sp, Some(inst.into()));
    }

    /// `step` over a node's significant text.
    fn step_node(&mut self, rule: &str, n: &SyntaxNode) {
        self.step(rule, Self::range(n));
    }

    fn take_trace(&mut self) -> Vec<crate::Step> {
        std::mem::take(&mut self.trace)
    }

    /// Writes the unit for `src` with the notes and steps gathered for it.
    fn emit(&mut self, out: &mut Out, src: (usize, usize), indent: usize, text: &str) {
        let notes = self.take_notes();
        let trace = self.take_trace();
        out.unit(src, indent, text, notes, trace);
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
        let mut text = self.commented(n, reason);
        if self.mode != Mode::Top {
            // Inside a routine or block the rest still runs: stop there
            // rather than skip the statement and carry on.
            let line = self.src[..Self::range(n).0].matches('\n').count() + 1;
            let msg = format!("sqldiff: statement at source line {line} was not converted");
            text.push_str(&if self.db2() {
                format!("SIGNAL SQLSTATE '75001' SET MESSAGE_TEXT = '{msg}';\n")
            } else {
                format!("RAISE EXCEPTION '{msg}';\n")
            });
        }
        self.notes.clear();
        self.trace.clear();
        let rule = if std::mem::take(&mut self.context) {
            CONTEXT
        } else {
            "not-converted"
        };
        let (s, e) = Self::range(n);
        out.unit(
            (s, e),
            indent,
            &text,
            vec![Note {
                status: Status::NotConverted,
                rule: rule.into(),
                text: reason.to_string(),
            }],
            vec![crate::Step {
                rule: rule.into(),
                start: s,
                end: e,
                inst: None,
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
        self.dynamic.clear();
    }

    fn do_block(&mut self, out: &mut Out, b: &SyntaxNode) {
        self.mode = Mode::Do;
        self.reset();
        self.dynamic = dynamic_sql_vars(&tokens(b));
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
                out.unit(d.src, INDENT, &d.lines.join("\n"), d.notes, d.trace);
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
            out.unit(d.src, INDENT, &d.lines.join("\n"), d.notes, d.trace);
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
        let res = self.guard_bits(|cx| match kind.as_str() {
            "PROC" | "PROCEDURE" => cx.proc_or_func(out, r, false),
            "FUNCTION" => cx.proc_or_func(out, r, true),
            "VIEW" => cx.view(r),
            "TRIGGER" if cx.db2() => no("triggers: Db2 for i triggers are row or statement triggers with REFERENCING NEW/OLD (TABLE); inserted/deleted need a rewrite"),
            "TRIGGER" => cx.trigger(r),
            _ => no("unrecognised routine"),
        });
        match res {
            Ok(Some(_)) if self.blocked().is_some() => {
                let reason = self.blocked().unwrap();
                self.mode = Mode::Top;
                self.refuse(out, r, 0, &reason)
            }
            Ok(Some(text)) => self.emit(out, Self::range(r), 0, &text),
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
        let name_at = ts
            .iter()
            .position(|t| t.is("VIEW"))
            .ok_or(No("view without VIEW".into()))?;
        let as_at = (name_at..ts.len())
            .find(|&i| ts[i].is("AS"))
            .ok_or(No("view without AS".into()))?;
        let head = &ts[name_at + 1..as_at];
        if let Some(w) = head.iter().position(|t| t.is("WITH")) {
            self.step_ts("tsql.view-options", &head[w..]);
            let text = format!(
                "View options such as SCHEMABINDING have no {} equivalent and were dropped; they do not change results.",
                self.to.display_name()
            );
            self.note(Status::Converted, "tsql.view-options", &text);
        }
        let name_end = head.iter().position(|t| t.is("WITH")).unwrap_or(head.len());
        let name = self.expr(&head[..name_end]).trim().to_string();
        // `CREATE|ALTER VIEW name [WITH …] AS` becomes `CREATE OR REPLACE VIEW name AS`.
        self.step_ts("tsql.routine.view", &ts[..=as_at]);
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
        let qt = tokens(&stmt);
        let outer = std::mem::replace(&mut self.scope, scope_of(&qt));
        let aliases = self.bit_aliases_of(&qt);
        let outer_aliases = std::mem::replace(&mut self.bit_aliases, aliases);
        let was = std::mem::replace(&mut self.view_query, true);
        let q = self.select(&qt);
        self.view_query = was;
        self.scope = outer;
        self.bit_aliases = outer_aliases;
        let (q, _) = q?;
        Ok(Some(format!("CREATE OR REPLACE VIEW {name} AS\n{q};")))
    }

    fn proc_or_func(&mut self, out: &mut Out, r: &SyntaxNode, func: bool) -> R<Option<String>> {
        self.reset();
        self.ret_bit = false;
        self.return_code = false;
        self.txn_control = false;
        let all = tokens(r);
        self.dynamic = dynamic_sql_vars(&all);
        let body_at = all.len()
            - r.children()
                .find(|c| c.kind() == ROUTINE_BODY)
                .map_or(0, |b| tokens(&b).len());
        let ts = &all[..body_at];
        let kw = ts
            .iter()
            .position(|t| t.is("PROC") || t.is("PROCEDURE") || t.is("FUNCTION"))
            .ok_or(No("routine without PROCEDURE or FUNCTION".into()))?;
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
                ds[j - i] == 0
                    && (ts[j].is("RETURNS")
                        || ts[j].is("WITH")
                        // `@p AS int` declares a parameter; the body's AS follows something else.
                        || ts[j].is("AS")
                            && !prev_sig(ts, j).is_some_and(|q| ts[q].k == VARIABLE))
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
            // `@p AS int` is `@p int`.
            let ty_at = next_sig(p, 0)
                .filter(|&j| p[j].is("AS"))
                .map_or(1, |j| j + 1)
                .min(ty_end);
            let (ty, string, numeric) = self.ty(&p[ty_at..ty_end])?;
            let pname = format!("p_{}", snake(&v.s[1..]));
            self.vars.insert(
                v.s[1..].to_ascii_lowercase(),
                Var {
                    name: pname.clone(),
                    string,
                    numeric,
                    dt: Dt::of_tokens(&p[ty_at..ty_end]),
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
            self.step_ts_with("tsql.routine.param", p, text.clone());
            if let Some(eq) = p[..].iter().position(|t| t.k == OP && t.s == "=") {
                let end = (eq + 1..p.len())
                    .find(|&j| p[j].is("OUTPUT") || p[j].is("OUT"))
                    .unwrap_or(p.len());
                let v = if Dt::of_tokens(&p[ty_at..ty_end]) == Dt::Bit {
                    self.bit_store_text(&p[eq + 1..end])
                } else {
                    self.expr(&p[eq + 1..end]).trim().to_string()
                };
                text.push_str(&format!(" DEFAULT {v}"));
                seen_default = true;
            } else if seen_default && !self.db2() {
                self.step_ts("pg.param-default-order", p);
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
            self.ret_bit = Dt::of_tokens(rt) == Dt::Bit;
            returns = format!(" RETURNS {ty}");
            self.step_ts_with("tsql.routine.returns", &ts[ret..rend], ty);
        }
        let owner = self.routine_options(&ts[stop..]);
        let header_notes = self.take_notes();
        let header_trace = self.take_trace();

        // Body.
        self.mode = if func { Mode::Func } else { Mode::Proc };
        // No body: the parser found nothing after the header (it reports why).
        let body = r.children().find(|c| c.kind() == ROUTINE_BODY).ok_or(No(
            "routine without a body: the statement is cut short or malformed".into(),
        ))?;
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
        self.trace = header_trace;
        let routine = if func {
            "tsql.routine.function"
        } else {
            "tsql.routine.procedure"
        };
        self.step_ts(routine, ts);
        if let Some(a) = ts[..kw].iter().position(|t| t.is("ALTER")) {
            self.step_ts("tsql.routine.alter", &ts[a..=a]);
        }
        if self.return_code {
            self.step_ts("tsql.routine.return-code", ts);
        }
        if self.db2() {
            return Ok(
                self.db2_routine(out, r, &body, b, func, &name, &params, &returns, kw, owner)
            );
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
                "PostgreSQL procedures cannot return a value: the T-SQL return status (0 unless RETURN n says otherwise, never NULL) becomes the INOUT parameter return_code, so callers must read return_code, and take 0 from a converted procedure that has none.",
            );
        }
        let mut security = "";
        if owner {
            if self.txn_control {
                self.note(
                    Status::Decision,
                    "pg.security-definer-transaction",
                    "EXECUTE AS OWNER would become SECURITY DEFINER, but a SECURITY DEFINER procedure cannot COMMIT or ROLLBACK in PostgreSQL (invalid transaction termination). The procedure runs with the caller's privileges: grant them, or move the transaction control to the caller and add SECURITY DEFINER.",
                );
            } else {
                security = "\nSECURITY DEFINER";
                self.note(
                    Status::Assumptions,
                    "ASM-OWNER-PRIVILEGES",
                    "EXECUTE AS OWNER becomes SECURITY DEFINER: the statements run with the owner's privileges, so the PostgreSQL owner needs the privileges the T-SQL owner has. Unqualified names still resolve through the caller's search_path; consider SET search_path (PostgreSQL manual, \"Writing SECURITY DEFINER Functions Safely\").",
                );
            }
        }
        let header = format!(
            "CREATE OR REPLACE {} {name}({}){returns}\nLANGUAGE plpgsql{security}\nAS $$",
            if func { "FUNCTION" } else { "PROCEDURE" },
            params.join(", ")
        );
        let (s, _) = Self::range(r);
        self.emit(out, (s, body.text_range().start().into()), 0, &header);
        self.write_decls(out);
        out.push("BEGIN\n");
        if self.return_code && !func {
            // T-SQL's status starts at 0: a path that ends without
            // `RETURN e` returns 0, never the caller's value.
            out.push(&format!("{}return_code := 0;\n", " ".repeat(INDENT)));
        }
        out.append(b);
        out.push("END;\n$$;\n");
        self.mode = Mode::Top;
        Ok(None)
    }

    /// T-SQL `WITH` routine options (header tokens after the parameters):
    /// true for EXECUTE AS OWNER. Options without an effect on results are
    /// dropped; any other one needs a decision.
    fn routine_options(&mut self, ts: &[T]) -> bool {
        let Some(w) = ts.iter().position(|t| t.is("WITH")) else {
            if let Some(r) = ts.iter().position(|t| t.is("REPLICATION")) {
                self.step_ts("tsql.routine-options", &ts[r.saturating_sub(2)..=r]);
                self.note(
                    Status::Decision,
                    "tsql.routine-options",
                    "FOR REPLICATION was dropped: it only matters to replication agents.",
                );
            }
            return false;
        };
        // The body's AS, not the one in EXECUTE AS.
        let si = sig(ts);
        let end = si
            .iter()
            .enumerate()
            .filter(|&(_, &j)| j > w)
            .find(|&(k, &j)| {
                let after_exec = k > 0 && (ts[si[k - 1]].is("EXECUTE") || ts[si[k - 1]].is("EXEC"));
                ts[j].is("AS") && !after_exec || ts[j].is("FOR")
            })
            .map_or(ts.len(), |(_, &j)| j);
        let mut owner = false;
        for opt in split_commas(&ts[w + 1..end]) {
            let rule = match sig(opt).first().map(|&j| opt[j].up()).as_deref() {
                Some("EXECUTE" | "EXEC") => "tsql.routine.execute-as",
                Some("RECOMPILE" | "ENCRYPTION" | "SCHEMABINDING") => {
                    "tsql.routine-options-dropped"
                }
                _ => "tsql.routine-options",
            };
            self.step_ts(rule, opt);
            let words: Vec<String> = sig(opt)
                .iter()
                .map(|&j| opt[j].s.to_ascii_uppercase())
                .collect();
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            match words.as_slice() {
                ["EXECUTE" | "EXEC", "AS", "OWNER"] => owner = true,
                ["EXECUTE" | "EXEC", "AS", "CALLER"] | ["CALLED", "ON", "NULL", "INPUT"] => {}
                ["RECOMPILE"] | ["ENCRYPTION"] | ["SCHEMABINDING"] => self.note(
                    Status::Converted,
                    "tsql.routine-options-dropped",
                    "WITH RECOMPILE, ENCRYPTION and SCHEMABINDING were dropped: they do not change results.",
                ),
                _ => {
                    let text = format!(
                        "The routine option WITH {} was dropped and has no equivalent here; check what it was for.",
                        words.join(" ")
                    );
                    self.note(Status::Decision, "tsql.routine-options", &text);
                }
            }
        }
        if let Some(r) = ts[end..].iter().position(|t| t.is("REPLICATION")) {
            self.step_ts("tsql.routine-options", &ts[end..=end + r]);
            self.note(
                Status::Decision,
                "tsql.routine-options",
                "FOR REPLICATION was dropped: it only matters to replication agents.",
            );
        }
        owner
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
        owner: bool,
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
        // The authority is written out too: without USRPRF it follows the
        // naming convention the routine is created under (*NAMING).
        let usrprf = if owner { "*OWNER" } else { "*USER" };
        let usrprf = format!("USRPRF = {usrprf}, DYNUSRPRF = {usrprf}");
        if owner {
            self.note(
                Status::Assumptions,
                "ASM-OWNER-PRIVILEGES",
                "EXECUTE AS OWNER becomes SET OPTION USRPRF = *OWNER: the statements run with the owner's authority added to the caller's, so the Db2 for i owner needs the privileges the T-SQL owner has.",
            );
        }
        if func {
            options.push_str(&format!("\nSET OPTION {usrprf}"));
        } else {
            if !self.cursors.is_empty() {
                options = format!("DYNAMIC RESULT SETS {}\n{options}", self.cursors.len());
            }
            options.push_str(&format!("\nSET OPTION COMMIT = *CS, {usrprf}"));
            self.note(
                Status::Assumptions,
                "ASM-DB2-COMMIT-CS",
                "The procedure is compiled with SET OPTION COMMIT = *CS: each statement is atomic and reads see only committed rows, as in T-SQL (READ COMMITTED). Without it the commit level is whatever the creating job uses (fact V5), and under *NONE nothing, not even a failed statement, is rolled back (facts V2, V4; V4 not checked on a real system). The work stays in the caller's unit of work until it commits.",
            );
        }
        let header = format!(
            "CREATE OR REPLACE {} {name} ({}){returns}\n{options}\nBEGIN",
            if func { "FUNCTION" } else { "PROCEDURE" },
            params.join(", ")
        );
        let (s, _) = Self::range(r);
        self.emit(out, (s, body.text_range().start().into()), 0, &header);
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
        if self.trigger_guard(out, n, indent) {
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
            || self.trigger.is_some()
            || self.in_try
            || self.in_catch
            || a.kind() != STATEMENT
            || !matches!(first_word(a).as_str(), "INSERT" | "EXEC" | "EXECUTE")
            || self.bits_in(&[a.clone(), b.clone()])
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
            || self.trigger.is_some()
            || !matches!(
                first_word(n).as_str(),
                "INSERT" | "UPDATE" | "DELETE" | "MERGE"
            )
            || self.mode == Mode::Top && procedural(n)
            || self.bits_in(std::slice::from_ref(n))
        {
            return false;
        }
        let (pieces, undeclared) = self.idioms(std::slice::from_ref(n));
        match pieces.as_slice() {
            [p] if p.output.is_ok() && !p.rules.is_empty() => {
                self.notes.clear();
                self.trace.clear();
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
        let trace = p
            .rules
            .iter()
            .map(|r| crate::Step {
                rule: r.clone(),
                start: p.src.0,
                end: p.src.1,
                inst: None,
            })
            .collect();
        out.unit(p.src, indent, text, notes, trace);
    }

    /// How a declared T-SQL variable (or parameter) is spelled in the output.
    fn declared(&self, v: &str) -> Option<String> {
        self.vars
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
            .map(|x| x.name.clone())
    }

    fn unit(&mut self, out: &mut Out, n: &SyntaxNode, indent: usize) {
        self.notes.clear();
        self.trace.clear();
        self.context = false;
        if matches!(n.kind(), IF_STMT | LOOP_STMT | TRY_CATCH) {
            self.seen_work = true;
        }
        match n.kind() {
            STATEMENT => {
                let reason = match self.stmt(n) {
                    Ok(_) if self.blocked().is_some() => self.blocked().unwrap(),
                    Ok(text) => {
                        self.dynamic_text(n);
                        return self.emit(out, Self::range(n), indent, &text);
                    }
                    Err(No(reason)) => reason,
                };
                if !self.idiom_single(out, n, indent) {
                    self.refuse(out, n, indent, &reason)
                }
            }
            BLOCK => self.body_elements(out, n, indent),
            IF_STMT if self.db2() && trancount_guard(n).is_some() => {
                // Under commitment control a unit of work is always open.
                let kw = trancount_guard(n).unwrap();
                match self.txn_db2(kw) {
                    Ok(text) => {
                        self.step_node("tsql.txn.trancount-guard", n);
                        self.emit(out, Self::range(n), indent, &format!("{text};"))
                    }
                    Err(No(reason)) => self.refuse(out, n, indent, &reason),
                }
            }
            IF_STMT if self.mode != Mode::Top => self.if_stmt(out, n, indent, false),
            LOOP_STMT if self.mode != Mode::Top => self.while_stmt(out, n, indent),
            TRY_CATCH if self.mode != Mode::Top => self.try_catch(out, n, indent),
            ROUTINE => self.routine(out, n),
            _ => self.refuse(out, n, indent, "no rule for this construct yet"),
        }
    }

    /// A statement that assigns a variable later run as dynamic SQL builds
    /// T-SQL text: the rules convert the statement, not the SQL in the string,
    /// so the unit cannot count as converted.
    fn dynamic_text(&mut self, n: &SyntaxNode) {
        if self.dynamic.is_empty() {
            return;
        }
        let ts = tokens(n);
        let s = sig(&ts);
        let assigns = s.windows(2).any(|w| {
            ts[w[0]].k == VARIABLE
                && self.dynamic.contains(&ts[w[0]].s[1..].to_ascii_lowercase())
                && ts[w[1]].k == OP
                && matches!(ts[w[1]].s.as_str(), "=" | "+=")
                && s.first()
                    .is_some_and(|&f| ts[f].is("SET") || ts[f].is("SELECT") || ts[f].is("DECLARE"))
        }) || s.first().is_some_and(|&f| ts[f].is("DECLARE"))
            && s.windows(2).any(|w| {
                ts[w[0]].k == VARIABLE
                    && self.dynamic.contains(&ts[w[0]].s[1..].to_ascii_lowercase())
                    && ts[w[1]].k != COMMA
                    && s.iter()
                        .any(|&j| j > w[0] && ts[j].k == OP && ts[j].s == "=")
            });
        if assigns {
            self.note(Status::Differs, "dynamic-sql.text", "This builds the text of dynamic SQL that the routine runs with EXEC or sp_executesql. The statement was converted, the SQL inside the string was not: it is still T-SQL.");
        }
    }

    fn condition(&mut self, n: &SyntaxNode) -> String {
        self.take_bit_unhandled();
        let cond = n
            .children()
            .find(|c| c.kind() == CONDITION)
            .map(|c| {
                let mut ts = tokens(&c);
                if self.db2() {
                    ts = dummy_from(&ts, false);
                }
                self.expr(&ts).trim().to_string()
            })
            .unwrap_or_default();
        // The engine does not compare conditions: what has no exact rewrite is refused.
        if let Some(reason) = self.bit_refusal() {
            self.note(Status::NotConverted, "pg.bit-number", &reason);
        }
        cond
    }

    /// The first line of IF or WHILE, written by `rule`.
    fn header_unit(
        &mut self,
        out: &mut Out,
        n: &SyntaxNode,
        indent: usize,
        text: &str,
        rule: &str,
    ) {
        let s = Self::range(n).0;
        let e = n
            .children()
            .find(|c| c.kind() == CONDITION)
            .map_or(s, |c| c.text_range().end().into());
        self.step(rule, (s, e.max(s + 1)));
        self.emit(out, (s, e.max(s + 1)), indent, text);
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
        self.header_unit(out, n, indent, &format!("{kw} {cond} THEN"), "tsql.ctl.if");
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
            self.header_unit(
                out,
                n,
                indent,
                &format!("{label}: WHILE {cond} DO"),
                "tsql.ctl.while",
            );
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
        self.header_unit(
            out,
            n,
            indent,
            &format!("WHILE {cond} LOOP"),
            "tsql.ctl.while",
        );
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
            self.step("idiom.rollback-rethrow", (s, s + 9));
            self.emit(
                out,
                (s, s + 9),
                indent,
                "-- BEGIN TRY … CATCH: ROLLBACK; THROW → error propagates",
            );
            self.body_elements(out, &try_b, indent);
            let (cs, ce) = Self::range(&catch_b);
            out.unit(
                (cs, ce),
                indent,
                "-- (CATCH removed: see above)",
                Vec::new(),
                one_step("idiom.rollback-rethrow", (cs, ce)),
            );
            return;
        }
        self.note(
            Status::Assumptions,
            "ASM-TRY-SUBTRANSACTION",
            "On an error, PostgreSQL undoes everything the TRY part did before running the handler (the block is a subtransaction); T-SQL keeps the statements that completed unless XACT_ABORT is ON or the CATCH rolls back.",
        );
        self.step_with(
            "tsql.ctl.try-catch",
            (s, s + 9),
            Some("exception-block".into()),
        );
        self.emit(out, (s, s + 9), indent, "BEGIN");
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
                "CATCH only rolls back and rethrows: the handler does ROLLBACK; RESIGNAL;, which undoes the whole unit of work (the caller's too, as T-SQL's ROLLBACK does) and raises the same error. Assumes the procedure runs under commitment control (it is compiled with *CS); under *NONE the ROLLBACK undoes nothing (fact V4, not checked on a real system).",
            );
        } else {
            self.note(
                Status::Assumptions,
                "ASM-TRY-HANDLER",
                "CATCH becomes an EXIT handler for SQLEXCEPTION: what the TRY part completed stays, as in T-SQL with XACT_ABORT OFF, and the failed statement is rolled back under commitment control. SQLEXCEPTION covers every error SQLSTATE; T-SQL's CATCH skips compile and name-resolution errors at the same level.",
            );
        }
        let how = if rethrow {
            "rollback-resignal"
        } else {
            "exit-handler"
        };
        self.step_with("tsql.ctl.try-catch", (s, s + 9), Some(how.into()));
        self.emit(out, (s, s + 9), indent, "BEGIN");
        let inner = indent + INDENT;
        out.lines(inner, "DECLARE EXIT HANDLER FOR SQLEXCEPTION");
        out.lines(inner, "BEGIN");
        let was = std::mem::replace(&mut self.in_catch, true);
        if rethrow {
            let (cs, ce) = Self::range(catch_b);
            out.unit(
                (cs, ce),
                inner + INDENT,
                "ROLLBACK;\nRESIGNAL;",
                Vec::new(),
                one_step("idiom.rollback-rethrow", (cs, ce)),
            );
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
        let ts = tokens(n);
        let outer = std::mem::replace(&mut self.scope, scope_of(&ts));
        let aliases = self.bit_aliases_of(&ts);
        let outer_aliases = std::mem::replace(&mut self.bit_aliases, aliases);
        let r = self.guard_bits(|cx| cx.stmt_in_scope(n));
        self.scope = outer;
        self.bit_aliases = outer_aliases;
        r
    }

    fn stmt_in_scope(&mut self, n: &SyntaxNode) -> R<String> {
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
        // `PERIOD FOR SYSTEM_TIME` declares the period; `t FOR SYSTEM_TIME …` queries it.
        if s.windows(3)
            .any(|p| !ts[p[0]].is("PERIOD") && ts[p[1]].is("FOR") && ts[p[2]].is("SYSTEM_TIME"))
        {
            return no(self.pick(
                "FOR SYSTEM_TIME reads the history of a system-versioned table: PostgreSQL has no temporal queries; a UNION ALL of the table and its history filtered on the period columns would do it; no rule yet",
                "FOR SYSTEM_TIME: system-versioned tables are not converted for Db2 for i yet; no rule yet",
            ));
        }
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
            "WITH" if cte_list(&ts).is_some_and(|(_, v)| !ts[v].is("SELECT")) => {
                self.with_dml(&ts)?
            }
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
                self.step_ts_with("tsql.stmt.print", &ts, "dropped");
                let body = Self::body_toks(&ts);
                let at = *s.get(1).ok_or(No("PRINT without a value".into()))?;
                format!("-- PRINT {}: dropped", self.expr(&body[at..]).trim())
            }
            "PRINT" => {
                let at = *s.get(1).ok_or(No("PRINT without a value".into()))?;
                self.step_ts_with("tsql.stmt.print", &ts, "raise-notice");
                let e = self.expr(&ts[at..]);
                format!("RAISE NOTICE '%', {}", e.trim())
            }
            "RETURN" => self.ret(&ts)?,
            "BREAK" | "CONTINUE" if self.db2() => {
                let Some(label) = self.loops.last() else {
                    return self.misplaced("BREAK/CONTINUE outside a WHILE loop");
                };
                let text = format!("{} {label}", if first == "BREAK" { "LEAVE" } else { "ITERATE" });
                let rule = if first == "BREAK" {
                    "tsql.stmt.break"
                } else {
                    "tsql.stmt.continue"
                };
                self.step_ts_with(rule, &ts, text.clone());
                text
            }
            "BREAK" => {
                self.step_ts("tsql.stmt.break", &ts);
                "EXIT".into()
            }
            "CONTINUE" => "CONTINUE".into(),
            "THROW" => self.throw(&ts)?,
            "RAISERROR" if self.db2() => return no("RAISERROR: severity decides whether it is an error at all; rewrite as SIGNAL (an error or a warning SQLSTATE)"),
            "RAISERROR" => self.raiserror(&ts)?,
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
                self.step_ts("tsql.stmt.truncate", &ts);
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

    /// The statement without its terminator. A block comment just before
    /// it stays (a template's directive stands in as one); a line comment
    /// there cannot, since the terminator would end up inside it.
    fn body_toks(ts: &[T]) -> Vec<T> {
        let mut v: Vec<T> = trimmed(ts).to_vec();
        while v
            .last()
            .is_some_and(|t| t.k == SEMICOLON || !t.sig() && t.k != BLOCK_COMMENT)
        {
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
            // A view's query comes here without `stmt`'s FROM-less SELECTs fixed.
            let ts = if self.db2() {
                dummy_from(&ts, true)
            } else {
                ts
            };
            return Ok((self.with_list(&ts, ts.len())?, false));
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
            let (end, lim) = match self.top_clause(&ts, top)? {
                paging::Top::Rows { end, count } => (end, Some(count)),
                paging::Top::All { end } => (end, None),
            };
            match &lim {
                Some(l) => self.step_ts_with(paging::TOP, &ts[top..=end], self.limit(l, "")),
                None => self.step_ts(paging::TOP, &ts[top..=end]),
            }
            // A view keeps the ORDER BY next to TOP 100 PERCENT.
            if lim.is_none()
                && self.view_query
                && self.db2()
                && find_word(&ts, 0, "ORDER").is_some()
            {
                self.note_v32();
            }
            cut.extend(top..=end);
            if let Some(nx) = next_sig(&ts, end) {
                // also drop whitespace after the count
                cut.extend(end + 1..nx);
            }
            if find_word(&ts, 0, "OFFSET").is_some() {
                return no("TOP and OFFSET in one query: SQL Server rejects it (Msg 10741)");
            }
            limit = lim;
            list_from = next_sig(&ts, end);
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
                self.step_ts("tsql.query-hints", &ts[o..=r]);
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
                targets.push(self.var_ref(&t[0]));
                let bit = self
                    .vars
                    .get(&t[0].s.trim_start_matches('@').to_ascii_lowercase())
                    .is_some_and(|v| v.dt == Dt::Bit);
                exprs.push(if bit {
                    self.bit_store_text(&t[eq + 1..])
                } else {
                    self.expr(&t[eq + 1..]).trim().to_string()
                });
            }
            // Without FROM the list runs to the end of the statement, so a
            // clause there would end up inside the last expression.
            // `SELECT @v = 1 WHERE <false>` keeps @v (SQL Server 2022), while
            // PL/pgSQL `v := 1 WHERE <false>` sets NULL.
            if from.is_none()
                && [
                    "WHERE",
                    "GROUP",
                    "HAVING",
                    "ORDER",
                    "UNION",
                    "EXCEPT",
                    "INTERSECT",
                ]
                .iter()
                .any(|w| find_word(&ts, list_start, w).is_some())
            {
                return no(
                    "SELECT @v = … with WHERE (or another clause) and no FROM: a false condition keeps the variables in T-SQL; rewrite as IF … SET",
                );
            }
            if from.is_none() && limit.is_none() {
                self.step_ts("tsql.select.assign", &ts);
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
            // T-SQL assigns row by row: with no row the variables keep their
            // values, with several the last row's values win, and an expression
            // that uses an assigned variable (`@s = @s + x`) accumulates.
            let assigned: Vec<String> = items
                .iter()
                .map(|it| trimmed(it)[0].s.to_ascii_lowercase())
                .collect();
            // Each item names its target once; any other use of an assigned
            // variable (in an expression, WHERE, ORDER BY, TOP, ...) may see the
            // values of earlier rows in T-SQL, while the loop's query reads it once.
            let accumulates = ts
                .iter()
                .filter(|x| x.k == VARIABLE && assigned.contains(&x.s.to_ascii_lowercase()))
                .count()
                > items.len();
            // The loop's own names must not meet a name of the statement: a
            // column `sqldiff_r` would be ambiguous with the record, and a column
            // `sqldiff_c1` in ORDER BY would read the loop's alias.
            let captured = ts.iter().any(|x| {
                x.s.trim_start_matches(['@', '[', '"'])
                    .to_ascii_lowercase()
                    .starts_with("sqldiff_")
            });
            // TOP 0 or OFFSET can drop the one row an aggregate returns.
            let one_row = from.is_some()
                && limit.is_none()
                && find_word(&ts, 0, "OFFSET").is_none()
                && find_word(&ts, 0, "FETCH").is_none()
                && find_word(&ts, 0, "GROUP").is_none()
                && find_word(&ts, 0, "HAVING").is_none()
                && find_word(&ts, 0, "UNION").is_none()
                && find_word(&ts, 0, "EXCEPT").is_none()
                && find_word(&ts, 0, "INTERSECT").is_none()
                && items.iter().any(|it| has_aggregate(it));
            let rest: Vec<T> = ts[list_end..]
                .iter()
                .enumerate()
                .filter(|(k, _)| !cut.contains(&(k + list_end)))
                .map(|(_, t)| t.clone())
                .collect();
            if !self.db2() && !one_row && !accumulates && !captured {
                // A loop over the query that assigns inside its body does exactly
                // that. The variables cannot be the loop's own targets: PL/pgSQL sets
                // the target of a loop over rows to NULL when the query returns no
                // row. The loop needs a declared record, so the statement is a block
                // of its own: the declaration stays inside it and nothing is hoisted.
                self.step_ts("tsql.select.assign-loop", &ts);
                let cols: Vec<String> = exprs
                    .iter()
                    .enumerate()
                    .map(|(k, e)| format!("{e} AS sqldiff_c{}", k + 1))
                    .collect();
                let mut q = format!(
                    "DECLARE\n  sqldiff_r record;\nBEGIN\n  FOR sqldiff_r IN SELECT {} {}",
                    cols.join(", "),
                    self.expr(&rest).trim()
                );
                if let Some(l) = limit {
                    q.push_str(&self.limit(&l, " "));
                }
                q.push_str(" LOOP");
                for (k, t) in targets.iter().enumerate() {
                    q.push_str(&format!("\n    {t} := sqldiff_r.sqldiff_c{};", k + 1));
                }
                q.push_str("\n  END LOOP;\nEND;");
                return Ok((q, true));
            }
            if !one_row {
                let text = self.pick(
                    "The query returns at most one row. With several rows T-SQL keeps the last row's values and PostgreSQL the first; with none T-SQL keeps the old values and PostgreSQL sets NULL. A statement that reads a variable it assigns (in an expression, WHERE, ORDER BY or TOP) accumulates or filters row by row in T-SQL; here it is read once.",
                    "The query returns at most one row. With several rows T-SQL keeps the last row's values and Db2 for i raises SQLSTATE 21000; with none both keep the old values (Db2 signals NOT FOUND, a warning that does not stop the routine).",
                ).to_string();
                self.note(Status::Assumptions, "ASM-SELECT-ASSIGN-CARDINALITY", &text);
            }
            self.step_ts("tsql.select.assign-into", &ts);
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
        if q.to_ascii_uppercase().contains("SESSION.") {
            self.note(Status::Assumptions, "ASM-DB2-SESSION-RUNTIME", "The cursor reads a global temporary table that the procedure declares later on: assumes Db2 for i resolves SESSION tables when the cursor is opened, not when the procedure is created (not in the verified vendor facts).");
        }
        self.step_node("tsql.stmt.result-set", n);
        let notes = self.take_notes();
        let trace = self.take_trace();
        let q = dedent(q.trim_end(), column(self.src, Self::range(n).0));
        let q: String = q.lines().map(|l| format!("  {l}\n")).collect();
        self.cursors.push(Decl {
            src: Self::range(n),
            lines: vec![format!(
                "DECLARE {name} INSENSITIVE CURSOR WITH RETURN TO CLIENT FOR\n{};",
                q.trim_end()
            )],
            notes,
            trace,
        });
        self.step_node("tsql.stmt.result-set", n);
        format!("OPEN {name};")
    }

    /// `WITH cte AS (…) INSERT/UPDATE/DELETE/MERGE …`. In T-SQL an UPDATE,
    /// DELETE or MERGE whose target is a CTE changes the CTE's base table;
    /// neither target can say that, so those are refused. PostgreSQL takes
    /// the WITH list in front of INSERT, and of UPDATE and DELETE on a real
    /// table, with the same meaning: the statement is converted as without
    /// it and the converted list goes in front. A FROM clause in UPDATE or
    /// DELETE is refused, since its tables may be CTEs bound as the target.
    /// Db2 for i: whether a data change statement takes a WITH list was not
    /// checked, so all of them are refused.
    fn with_dml(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let (names, verb) = cte_list(&ts).ok_or(No("unreadable WITH list".into()))?;
        let what = ts[verb].up();
        if self.db2() {
            return no(format!(
                "WITH … {what}: not converted for Db2 for i (whether a data change statement takes a WITH list was not checked); rewrite with the CTE as a subquery, or against its base table"
            ));
        }
        if what == "MERGE" {
            return no("WITH … MERGE: MERGE has no PostgreSQL rule yet");
        }
        let s: Vec<usize> = sig(&ts).into_iter().filter(|&i| i >= verb).collect();
        let target = s
            .get(1)
            .map(|&j| {
                if ts[j].is("FROM") || ts[j].is("INTO") {
                    s.get(2).copied()
                } else {
                    Some(j)
                }
            })
            .unwrap_or(None);
        let base = target.and_then(|j| name_last(&ts, j));
        if base.as_ref().is_some_and(|b| names.contains(b)) {
            return no(format!(
                "WITH … {what} through a CTE: T-SQL changes the CTE's base table; rewrite the statement against that table"
            ));
        }
        if what != "INSERT" {
            let after = if what == "UPDATE" {
                find_word(&ts, verb, "SET").unwrap_or(verb)
            } else {
                s.get(1)
                    .map_or(verb, |&j| if ts[j].is("FROM") { j } else { verb })
            };
            if find_word(&ts, after + 1, "FROM").is_some() {
                return no(format!(
                    "WITH … {what} … FROM: not converted yet; the FROM tables may be CTEs, and the target one of them"
                ));
            }
        }
        let prefix = self.with_list(&ts, verb)?.trim().to_string();
        let body = self.dml(&ts[verb..])?;
        Ok(format!("{prefix} {}", body.trim_start()))
    }

    /// `ts[..end]` of a statement that starts with WITH. A recursive CTE
    /// needs WITH RECURSIVE in PostgreSQL (Db2 for i has no such keyword).
    /// T-SQL ends a recursion past MAXRECURSION (100 by default) with an
    /// error, which neither target does.
    /// RECURSIVE also lets every CTE see the ones after it, which T-SQL
    /// does not, so a list where a query names a later CTE is refused.
    fn with_list(&mut self, ts: &[T], end: usize) -> R<String> {
        let Some((_, _, true, forward)) = cte_list_rec(ts) else {
            return Ok(self.expr(&ts[..end]));
        };
        if forward && !self.db2() {
            return no("a recursive CTE list where a query names a CTE defined after it: WITH RECURSIVE would make that name the later CTE, where T-SQL reads the table; rename or reorder the CTEs");
        }
        self.note(
            Status::Assumptions,
            "ASM-CTE-MAXRECURSION",
            &format!(
                "A recursive CTE: T-SQL stops it with an error past its recursion limit (MAXRECURSION, 100 levels by default), {} has no such limit. Equal while the recursion stays within the limit.",
                self.to.display_name()
            ),
        );
        let w = sig(ts)[0];
        self.step_ts("tsql.cte.recursive", &ts[w..=w]);
        let rest = self.expr(&ts[w + 1..end]);
        Ok(if self.db2() {
            format!("WITH{rest}")
        } else {
            format!("WITH RECURSIVE{rest}")
        })
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
        if ts[*s.get(1).ok_or(No("incomplete statement".into()))?].is("TOP") {
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
            "INSERT" if !ts[s[1]].is("INTO") => {
                self.step_ts("tsql.dml.insert-into", &ts[s[0]..=s[0]]);
                pre = "INSERT INTO ".into()
            }
            "UPDATE" => {
                let set = find_word(&ts, 0, "SET");
                if let Some(f) = find_word(&ts, set.unwrap_or(0), "FROM") {
                    if self.db2() {
                        return no("UPDATE … FROM with a join: Db2 for i needs a correlated subquery or MERGE, and with several matching rows T-SQL picks one unpredictably; needs a rewrite");
                    }
                    let Some(set) = set else {
                        return no("UPDATE … FROM without a SET clause the rules can read (the assignments may be a placeholder a generator fills); convert it by hand");
                    };
                    return self.update_join(&ts, set, f);
                }
                from = s[0];
            }
            "DELETE" => {
                let start = if ts[s[1]].is("FROM") { s[1] + 1 } else { s[1] };
                if let Some(f) = find_word(&ts, start, "FROM") {
                    return self.delete_join(&ts, start, f);
                }
                if ts[s[1]].is("FROM") {
                    from = s[0];
                } else {
                    self.step_ts("tsql.dml.delete-from", &ts[s[0]..=s[0]]);
                    pre = "DELETE FROM ".into();
                }
            }
            _ => from = s[0],
        }
        if pre.is_empty() {
            return Ok(self.expr(&ts));
        }
        Ok(format!("{pre}{}", self.expr(&ts[from..]).trim_start()))
    }

    /// `DELETE t FROM <tables> WHERE w` (or `DELETE FROM t FROM …`): T-SQL
    /// deletes each row of `t` for which the FROM list has at least one row
    /// that satisfies the join and WHERE conditions, once however many rows
    /// match. When the FROM tables are joined by inner and cross joins alone
    /// (and commas) and `t` names exactly one of them, the joins flatten to
    /// the other tables and the conjunction of all conditions, which is
    /// exactly PostgreSQL's `DELETE FROM t USING <others> WHERE …` and Db2
    /// for i's `DELETE FROM t WHERE EXISTS (SELECT 1 FROM <others> WHERE …)`.
    /// Outer joins, APPLY, join hints, nested ON and a target that is not
    /// named by one FROM table are refused.
    fn delete_join(&mut self, ts: &[T], start: usize, from: usize) -> R<String> {
        let why = |w: &str| {
            No(format!(
                "DELETE … FROM … JOIN: {w}; {}",
                self.pick(
                    "rewrite with USING or a key subquery",
                    "rewrite with EXISTS or a key subquery",
                )
            ))
        };
        let target = name_parts(trimmed(&ts[start..from]))
            .ok_or_else(|| why("the target is not a plain table name or alias"))?;
        let end = ["WHERE", "OPTION"]
            .iter()
            .filter_map(|w| find_word(ts, from + 1, w))
            .min()
            .unwrap_or(ts.len());
        if find_word(ts, from + 1, "OPTION").is_some() {
            return Err(why("query hints (OPTION) are not converted"));
        }
        let mut leaves: Vec<Leaf> = Vec::new();
        let mut conds: Vec<&[T]> = Vec::new();
        for item in split_commas(&ts[from + 1..end]) {
            join_chain(item, &mut leaves, &mut conds).map_err(|w| why(&w))?;
        }
        let hits: Vec<usize> = (0..leaves.len())
            .filter(|&i| leaves[i].binds(&target))
            .collect();
        let [hit] = hits[..] else {
            return Err(why(if hits.is_empty() {
                "the target is not one of the FROM tables by alias or one-part name"
            } else {
                "the target matches more than one FROM table"
            }));
        };
        if !leaves[hit].table {
            return Err(why("the target is not a table"));
        }
        if end < ts.len() {
            conds.push(&ts[end + 1..]);
        }
        if self.db2() && leaves.len() > 1 {
            // Inside EXISTS an unqualified name resolves to the inner tables
            // first, while T-SQL rejects a name that two FROM tables have. So
            // every unqualified name must be in exactly one table by the
            // catalog; without one, it has to be qualified.
            for c in &conds {
                if c.iter().any(|t| t.is("SELECT")) {
                    return Err(why("a subquery in the conditions"));
                }
                for name in bare_columns(c) {
                    if self.column_owners(&leaves, &name) != Some(1) {
                        return Err(why(&format!(
                            "the unqualified column `{name}` could belong to more than one table (qualify it with its table alias)"
                        )));
                    }
                }
            }
        }
        let tgt = self.expr(trimmed(leaves[hit].toks)).trim().to_string();
        let others: Vec<String> = (0..leaves.len())
            .filter(|&i| i != hit)
            .map(|i| self.leaf_text(&leaves[i]))
            .collect();
        let conds: Vec<String> = conds
            .iter()
            .map(|c| format!("({})", self.expr(trimmed(c)).trim()))
            .collect();
        let filter = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        let how = match (others.is_empty(), self.db2()) {
            (true, _) => "plain",
            (false, false) => "using",
            (false, true) => "exists",
        };
        self.step_ts_with("tsql.dml.delete-join", ts, how);
        Ok(match (others.is_empty(), self.db2()) {
            (true, _) => format!("DELETE FROM {tgt}{filter}"),
            (false, false) => format!("DELETE FROM {tgt} USING {}{filter}", others.join(", ")),
            (false, true) => format!(
                "DELETE FROM {tgt} WHERE EXISTS (SELECT 1 FROM {}{filter})",
                others.join(", ")
            ),
        })
    }

    /// `UPDATE x SET … FROM <tables> [WHERE w]` (rule T4): T-SQL changes
    /// the rows of the FROM table `x` names that the joins and WHERE keep,
    /// with values from the row they meet. PostgreSQL's `UPDATE t [AS] x SET
    /// … FROM <others> WHERE …` must not repeat the target in FROM, so the
    /// target leaves the FROM list and the condition of the inner join that
    /// tied it to its neighbour moves to WHERE: `(t ⋈k s) ⟕ u` is `t ⋈k (s ⟕
    /// u)` as long as no other join condition names `t`, which PostgreSQL
    /// rejects anyway. The target must head a comma item or a join chain, or
    /// follow its head through an inner or cross join; only inner, LEFT and
    /// cross joins are taken. When a target row meets several rows, both
    /// dialects pick one unpredictably, so the source catalog must show that
    /// every table is fixed by a unique key matched in full (`unique_join`).
    fn update_join(&mut self, ts: &[T], set: usize, from: usize) -> R<String> {
        let why = |w: &str| {
            No(format!(
                "UPDATE … FROM with a join: {w}; rewrite with the target alone in UPDATE and the other tables in FROM"
            ))
        };
        let upd = sig(ts)[0];
        let target = name_parts(trimmed(&ts[upd + 1..set]))
            .filter(|p| p.len() <= 2)
            .ok_or_else(|| {
                why("the target is not a table name, a schema-qualified one or an alias")
            })?;
        if find_word(ts, from + 1, "OPTION").is_some() {
            return Err(why("query hints (OPTION) are not converted"));
        }
        let end = find_word(ts, from + 1, "WHERE").unwrap_or(ts.len());
        let mut chains: Vec<Vec<Step>> = Vec::new();
        for item in split_commas(&ts[from + 1..end]) {
            chains.push(join_steps(item).map_err(|w| why(&w))?);
        }
        let mut hits: Vec<(usize, usize)> = Vec::new();
        for (i, c) in chains.iter().enumerate() {
            for (j, st) in c.iter().enumerate() {
                if update_binds(&st.leaf, &target).map_err(|w| why(&w))? {
                    hits.push((i, j));
                }
            }
        }
        if hits.len() > 1 {
            return Err(why("the target matches more than one FROM table"));
        }
        if chains
            .iter()
            .flatten()
            .any(|st| find_word(st.leaf.toks, 0, "WITH").is_some() || hinted(st.leaf.toks))
        {
            return Err(why("table hints are not converted"));
        }
        let mut moved: Vec<&[T]> = Vec::new();
        // The target's name parts, its key (alias or last name part), and
        // its text in the output.
        let (tname, tkey, tgt) = if let [(ci, li)] = hits[..] {
            let c = &mut chains[ci];
            if !c[li].leaf.table {
                return Err(why("the target is not a table"));
            }
            let inner = |k: Join| matches!(k, Join::Inner | Join::Cross);
            let tleaf = match li {
                0 if c.len() == 1 => chains.remove(ci).remove(0).leaf,
                0 if inner(c[1].kind) => {
                    moved.extend(c[1].on);
                    let t = c.remove(0).leaf;
                    c[0].kind = Join::First;
                    c[0].on = None;
                    t
                }
                1 if inner(c[1].kind) => {
                    moved.extend(c[1].on);
                    c.remove(1).leaf
                }
                _ => {
                    return Err(why(
                        "the target is not joined first, by an inner or cross join",
                    ))
                }
            };
            let tkey = tleaf
                .alias
                .clone()
                .or_else(|| tleaf.name.last().cloned())
                .unwrap_or_default();
            let tgt = self.expr(trimmed(tleaf.toks)).trim().to_string();
            (tleaf.name, tkey, tgt)
        } else {
            // The target is not in FROM: SQL Server joins it to the FROM
            // tables, in scope in SET and WHERE but not in ON, as PostgreSQL
            // does (checked on SQL Server 2022). A FROM item that shows the
            // target's name next to it is refused: PostgreSQL rejects the
            // name twice, and the binding was not checked.
            let key = target.last().cloned().unwrap_or_default();
            if chains.iter().flatten().any(|st| {
                st.leaf.alias.as_ref() == Some(&key)
                    || (st.leaf.alias.is_none() && st.leaf.name.last() == Some(&key))
            }) {
                return Err(why(&format!(
                    "the target is not in FROM, and a FROM item is also named `{key}`"
                )));
            }
            let tgt = self.expr(trimmed(&ts[upd + 1..set])).trim().to_string();
            (target.clone(), key, tgt)
        };
        // The conditions left in FROM must not name the target.
        let target_cols = self.table_columns(&tname);
        for st in chains.iter().flatten() {
            let Some(on) = st.on else { continue };
            if qualifies(on, &tkey) {
                return Err(why(&format!(
                    "a join condition other than the target's own names the target `{tkey}`"
                )));
            }
            for name in bare_columns(on) {
                if target_cols
                    .as_ref()
                    .map_or(true, |cols| cols.contains(&name))
                {
                    return Err(why(&format!(
                        "`{name}` in a join condition may be a column of the target (qualify it with its table alias)"
                    )));
                }
            }
        }
        let wh: Option<&[T]> = (end < ts.len()).then(|| &ts[end + 1..]);
        self.unique_join(&tkey, &chains, &moved, wh)
            .map_err(|w| why(&w))?;

        self.step_ts("tsql.dml.update-from", ts);
        // SET list: PostgreSQL takes no table name before a column.
        let mut set_toks: Vec<T> = Vec::new();
        for (k, item) in split_commas(&ts[set + 1..from]).into_iter().enumerate() {
            if k > 0 {
                let at = item.first().map_or(0, |t| t.pos.0);
                set_toks.push(T::made(COMMA, ",", at, "tsql.dml.update-from"));
            }
            let s = sig(item);
            let lhs_end = s
                .iter()
                .position(|&j| item[j].k == OP)
                .ok_or_else(|| why("a SET item that is not `column = value`"))?;
            let lhs: Vec<usize> = s[..lhs_end].to_vec();
            let skip = match name_parts(trimmed(&item[..s[lhs_end]])) {
                Some(p) if p.len() == 1 => 0,
                Some(p) if p.len() == 2 && p[0] == tkey => 2,
                _ => return Err(why("a SET column that is not a column of the target")),
            };
            for (j, t) in item.iter().enumerate() {
                if skip > 0 && lhs[..skip].contains(&j) {
                    continue;
                }
                set_toks.push(t.clone());
            }
        }
        let sets = self.expr(&set_toks).trim().to_string();
        let mut text = format!("UPDATE {tgt} SET {sets}");
        let others: Vec<String> = chains
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| {
                let mut o = String::new();
                for st in c {
                    o.push_str(match st.kind {
                        Join::First => "",
                        Join::Inner => " JOIN ",
                        Join::Left => " LEFT JOIN ",
                        Join::Cross => " CROSS JOIN ",
                    });
                    o.push_str(&self.leaf_text(&st.leaf));
                    if let Some(on) = st.on {
                        o.push_str(" ON ");
                        o.push_str(self.expr(trimmed(on)).trim());
                    }
                }
                o
            })
            .collect();
        if !others.is_empty() {
            text.push_str(&format!(" FROM {}", others.join(", ")));
        }
        let mut conds: Vec<String> = moved
            .iter()
            .map(|c| self.expr(trimmed(c)).trim().to_string())
            .collect();
        if let Some(w) = wh {
            conds.push(self.expr(trimmed(w)).trim().to_string());
        }
        match conds.len() {
            0 => {}
            1 => text.push_str(&format!(" WHERE {}", conds[0])),
            _ => {
                let c: Vec<String> = conds.iter().map(|c| format!("({c})")).collect();
                text.push_str(&format!(" WHERE {}", c.join(" AND ")));
            }
        }
        Ok(text)
    }

    /// Whether every table of an UPDATE … FROM meets at most one row per
    /// target row: walking out from the target, a table is settled once
    /// equalities fix every column of one of its unique keys (by the source
    /// catalog) to columns of settled tables, variables or literals. A LEFT
    /// JOIN's table counts only the equalities of its own ON condition; an
    /// inner-joined one those of WHERE and every inner join's ON condition.
    fn unique_join(
        &self,
        target: &str,
        chains: &[Vec<Step>],
        moved: &[&[T]],
        wh: Option<&[T]>,
    ) -> Result<(), String> {
        let mut all: Vec<&[T]> = moved.to_vec();
        all.extend(wh);
        for st in chains.iter().flatten() {
            if matches!(st.kind, Join::Inner) {
                all.extend(st.on);
            }
        }
        let all_eqs: Vec<(Vec<String>, Vec<String>)> =
            all.iter().flat_map(|c| equalities(c)).collect();
        let leaves: Vec<&Step> = chains.iter().flatten().collect();
        let key_of = |l: &Leaf| {
            l.alias
                .clone()
                .or_else(|| l.name.last().cloned())
                .unwrap_or_default()
        };
        let mut bound = vec![target.to_string()];
        loop {
            let mut progress = false;
            for st in &leaves {
                let k = key_of(&st.leaf);
                if bound.contains(&k) {
                    continue;
                }
                let own;
                let eqs = match (st.kind, st.on) {
                    (Join::Left, Some(on)) => {
                        own = equalities(on);
                        &own
                    }
                    _ => &all_eqs,
                };
                let fixed = |side: &[String]| match side {
                    [] => true,
                    [q, _] => bound.contains(q),
                    _ => false,
                };
                let mut cols = Vec::new();
                for (x, y) in eqs {
                    for (col, other) in [(x, y), (y, x)] {
                        if matches!(col.as_slice(), [q, _] if *q == k) && fixed(other) {
                            cols.push(col[1].clone());
                        }
                    }
                }
                if st.leaf.table && self.has_unique_key(&st.leaf.name, &cols) {
                    bound.push(k);
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        match leaves.iter().find(|st| !bound.contains(&key_of(&st.leaf))) {
            None => Ok(()),
            Some(st) => Err(format!(
                "the source catalog does not show that a target row meets at most one row of `{}` (a unique key matched in full by equalities); with several, T-SQL and PostgreSQL each pick one unpredictably",
                key_of(&st.leaf)
            )),
        }
    }

    /// The source catalog's columns of a table (lower case).
    fn table_columns(&self, name: &[String]) -> Option<Vec<String>> {
        let t = self.catalog_table(name)?;
        t.columns_known.then(|| {
            t.columns
                .iter()
                .map(|c| c.name.to_ascii_lowercase())
                .collect()
        })
    }

    /// Whether `cols` include every column of a primary key, a UNIQUE
    /// constraint or an unfiltered unique index of the table.
    fn has_unique_key(&self, name: &[String], cols: &[String]) -> bool {
        let Some(t) = self.catalog_table(name) else {
            return false;
        };
        t.keys
            .iter()
            .map(|k| &k.columns)
            .chain(t.indexes.iter().filter(|i| i.unique).map(|i| &i.columns))
            .any(|k| !k.is_empty() && k.iter().all(|c| cols.contains(&c.to_ascii_lowercase())))
    }

    fn catalog_table(&self, name: &[String]) -> Option<&sqldiff_core::catalog::Table> {
        use sqldiff_core::catalog::{table_position, Ident};
        let cat = self.type_catalog()?;
        let name = self.image_table(name);
        let parts: Vec<Ident> = match name {
            [.., s, t] if s != "dbo" => vec![Ident::plain(s.clone()), Ident::plain(t.clone())],
            [.., t] => vec![Ident::plain(t.clone())],
            [] => return None,
        };
        Some(&cat.tables[table_position(Dialect::Tsql, cat, &parts)?])
    }

    /// How many of the DELETE's FROM tables have column `name`, by the
    /// source catalog; `None` without a catalog entry for every table.
    fn column_owners(&self, leaves: &[Leaf], name: &str) -> Option<usize> {
        use sqldiff_core::catalog::{table_position, Ident};
        let cat = self.catalog?;
        let mut n = 0;
        for l in leaves {
            if !l.table {
                return None;
            }
            let parts: Vec<Ident> = match l.name.as_slice() {
                [.., s, t] if s != "dbo" => vec![Ident::plain(s.clone()), Ident::plain(t.clone())],
                [.., t] => vec![Ident::plain(t.clone())],
                [] => return None,
            };
            let t = &cat.tables[table_position(Dialect::Tsql, cat, &parts)?];
            n += usize::from(t.column(Dialect::Tsql, &Ident::plain(name)).is_some());
        }
        Some(n)
    }

    fn set(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let a = &ts[*s.get(1).ok_or(No("SET without a target".into()))?];
        if a.k == VARIABLE && !a.s.starts_with("@@") {
            let op = &ts[*s.get(2).ok_or(No("SET without a value".into()))?];
            let target = self.var_ref(a);
            let value = *s.get(3).ok_or(No("SET without a value".into()))?;
            let bit = self
                .vars
                .get(&a.s[1..].to_ascii_lowercase())
                .is_some_and(|v| v.dt == Dt::Bit);
            let rhs = if bit && op.s == "=" {
                self.bit_store_text(&ts[value..])
            } else {
                self.expr(&ts[value..]).trim().to_string()
            };
            self.step_ts_with("tsql.stmt.set-variable", &ts, op.s.clone());
            // `SET @b += 0x…` on a VARBINARY variable appends bytes.
            let bin_concat =
                op.s == "+=" && self.is_varbinary_var(&a.s) && self.binary_chain(&ts[value..]);
            if bin_concat {
                self.binary_concat_note(std::slice::from_ref(op));
            }
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
                    let o = if bin_concat || (op.s == "+=" && self.is_string_var(&a.s)) {
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
        self.step_ts_with("tsql.stmt.set-option", &ts, format!("{opt} {val}"));
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
                        self.step_ts("tsql.merge.by-target", &ts[k..=j]);
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
                kept.push(T::made(WHITESPACE, " ", t.pos.1, "tsql.merge.into"));
                kept.push(T::made(IDENT, "INTO", t.pos.1, "tsql.merge.into"));
            }
            k += 1;
        }
        self.step_ts("tsql.merge", &ts);
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
        let mut pending = Vec::new(); // (variable, initialiser tokens, BIT)
        for item in split_commas(&ts[*s.get(1).ok_or(No("DECLARE without a name".into()))?..]) {
            let it = trimmed(item);
            let Some(v) = it.first().filter(|t| t.k == VARIABLE) else {
                return no("DECLARE form not understood");
            };
            // `@v AS int`: the AS is the first significant token after the name.
            let mut j = 1;
            while it.get(j).is_some_and(|t| !t.sig()) {
                j += 1;
            }
            if it.get(j).is_some_and(|t| t.is("AS")) {
                j += 1;
            } else {
                j = 1;
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
                    dt: Dt::of_tokens(&it[j..eq.unwrap_or(it.len())]),
                },
            );
            let line = if self.db2() {
                format!("DECLARE {name} {ty};")
            } else {
                format!("{name} {ty};")
            };
            self.step_ts_with("tsql.decl.variable", it, line.clone());
            lines.push(line);
            if let Some(eq) = eq {
                let bit = Dt::of_tokens(&it[j..eq]) == Dt::Bit;
                pending.push((name, it[eq + 1..].to_vec(), bit));
            }
        }
        // Type notes belong to the hoisted declaration; initialiser notes to
        // the assignment left in place. Each note is counted once.
        let notes = self.take_notes();
        let trace = self.take_trace();
        self.decls.push(Decl {
            src: Self::range(n),
            lines,
            notes,
            trace,
        });
        let db2 = self.db2();
        let inits: Vec<String> = pending
            .iter()
            .map(|(name, init, bit)| {
                let v = if *bit {
                    self.bit_store_text(init)
                } else {
                    self.expr(init).trim().to_string()
                };
                self.step_ts_with("tsql.decl.init", init, name.clone());
                if db2 {
                    format!("SET {name} = {v};")
                } else {
                    format!("{name} := {v};")
                }
            })
            .collect();
        if inits.is_empty() {
            self.step_ts("tsql.decl.hoist", &ts);
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
        self.step_ts_with("tsql.decl.table-variable", ts, format!("SESSION.{name}"));
        self.note(Status::Assumptions, "ASM-TABLE-VARIABLE", "The table variable becomes a NOT LOGGED global temporary table with ON ROLLBACK PRESERVE ROWS, so a rollback keeps its rows as T-SQL does (fact V16; under *NONE the ON ROLLBACK clause does not apply). It lives until the job ends rather than the batch; WITH REPLACE empties it when the code runs again.");
        Ok(format!(
            "DECLARE GLOBAL TEMPORARY TABLE SESSION.{name} ({}\n) ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE",
            cols.join(",")
        ))
    }

    fn ret(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let val = s.get(1).map(|&j| {
            if self.mode == Mode::Func && self.ret_bit {
                self.bit_store_text(&ts[j..])
            } else {
                self.expr(&ts[j..]).trim().to_string()
            }
        });
        match (self.mode, val) {
            (Mode::Func, None) if self.trigger.is_some() => {
                self.step_ts_with("tsql.stmt.return", &ts, "trigger");
                Ok("RETURN NULL".into())
            }
            (Mode::Func, Some(_)) if self.trigger.is_some() => {
                self.misplaced("RETURN with a value in a trigger")
            }
            (Mode::Func, Some(v)) => Ok(format!("RETURN {v}")),
            (Mode::Proc, Some(v)) if self.db2() => {
                self.return_code = true;
                Ok(format!("RETURN {v}"))
            }
            (Mode::Do, _) if self.db2() => self.misplaced("RETURN in a script: a Db2 for i compound statement cannot return; put a label on the block and LEAVE it"),
            (Mode::Proc, Some(v)) => {
                self.return_code = true;
                self.step_ts_with("tsql.stmt.return", &ts, "return-code");
                // A procedure cannot return NULL: T-SQL returns 0 instead.
                Ok(format!("return_code := COALESCE({v}, 0);\nRETURN;"))
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
                self.step_ts_with("tsql.stmt.throw", &ts, "rethrow");
                return Ok(self.pick("RAISE", "RESIGNAL").into());
            }
            return self.misplaced("THROW without arguments outside CATCH");
        }
        let args = split_commas(&ts[s[1]..]);
        if args.len() != 3 {
            return no("THROW form not understood");
        }
        let msg = self.expr(args[1]).trim().to_string();
        self.step_ts_with("tsql.stmt.throw", &ts, "raise");
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

    /// PostgreSQL: `RAISERROR (format, severity, state [, args]) [WITH
    /// NOWAIT]` with a literal severity. 0 to 10 is an informational message
    /// (`RAISE NOTICE`), 11 to 18 an error (`RAISE EXCEPTION`); whether the
    /// code after it still runs is the engine's to compare (T-SQL carries on
    /// after RAISERROR outside TRY, even under XACT_ABORT ON). The format
    /// may use `%s`, `%d`, `%i` and `%%` only. Checked on SQL Server 2022:
    /// a NULL argument shows as `(null)`, so each value becomes
    /// `COALESCE(value, '(null)')` (PostgreSQL would show `<NULL>`).
    fn raiserror(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let form = || No("RAISERROR form not understood".into());
        let open = *s.get(1).filter(|&&j| ts[j].k == L_PAREN).ok_or_else(form)?;
        let close = matching(&ts, open).ok_or_else(form)?;
        let tail: Vec<String> = ts[close + 1..]
            .iter()
            .filter(|t| t.sig())
            .map(T::up)
            .collect();
        if !tail.is_empty() && tail.join(" ") != "WITH NOWAIT" {
            return no("RAISERROR WITH LOG or SETERROR: also writes the error log or sets @@ERROR; rewrite by hand");
        }
        let args = split_commas(&ts[open + 1..close]);
        if args.len() < 3 {
            return Err(form());
        }
        let error = match trimmed(args[1]) {
            [t] if t.k == NUMBER => match t.s.parse::<u32>() {
                Ok(0..=10) => false,
                Ok(11..=18) => true,
                _ => return no("RAISERROR severity 19 or more: needs WITH LOG and ends the connection from 20; rewrite by hand"),
            },
            _ => return no("RAISERROR with a severity that is not a literal: whether it is an error or a message depends on its value"),
        };
        if error && self.trigger.is_some() {
            return self.misplaced("RAISERROR of severity 11 or more in a trigger: T-SQL carries on with the trigger, PostgreSQL fails the statement that fired it");
        }
        let var = |cx: &Self, t: &T| -> Option<(String, Dt)> {
            let v = cx.vars.get(&t.s.get(1..)?.to_ascii_lowercase())?;
            (t.k == VARIABLE && !t.s.starts_with("@@")).then(|| (v.name.clone(), v.dt))
        };
        // The PostgreSQL format, its values, and whether any value is shown.
        let mut fmt = String::new();
        let mut values = Vec::new();
        let rest = &args[3..];
        match trimmed(args[0]) {
            [t] if t.k == VARIABLE => {
                let Some((name, Dt::Text)) = var(self, t) else {
                    return no("RAISERROR with a message number or a variable that is not a declared string: rewrite by hand");
                };
                if !rest.is_empty() {
                    return no("RAISERROR with a format variable and arguments: the substitutions depend on the variable's value");
                }
                self.note(Status::Assumptions, "ASM-RAISERROR-FORMAT", "The message variable holds no % sign: T-SQL reads it as a format (a % starts a substitution), PostgreSQL shows it as it is. A NULL message shows as an empty one on both sides.");
                fmt.push('%');
                values.push(format!("COALESCE({name}, '')"));
            }
            [t] if t.k == STRING => {
                if !t.s.starts_with(['N', 'n']) && !t.s.is_ascii() {
                    return no("RAISERROR with a non-Unicode format holding characters outside ASCII: the code page decides them; write it as N'…'");
                }
                let mut rest = rest.iter();
                let mut cs = string_value(&t.s).chars().collect::<Vec<_>>().into_iter();
                while let Some(ch) = cs.next() {
                    if ch != '%' {
                        fmt.push(ch);
                        continue;
                    }
                    let spec = cs.next();
                    if spec == Some('%') {
                        fmt.push_str("%%");
                        continue;
                    }
                    if !matches!(spec, Some('s' | 'd' | 'i')) {
                        return no("RAISERROR format with a specification other than %s, %d, %i or %% (width, precision, flags, %u, %o, %x): not converted");
                    }
                    let Some(arg) = rest.next() else {
                        return no("RAISERROR format with more specifications than arguments: T-SQL shows (null) for the missing ones; pass them");
                    };
                    let string = spec == Some('s');
                    match trimmed(arg) {
                        // A constant goes into the format as its text.
                        [a] if string && a.k == STRING && (a.s.starts_with(['N', 'n']) || a.s.is_ascii()) => {
                            fmt.push_str(&string_value(&a.s).replace('%', "%%"));
                        }
                        [a] if !string && a.k == NUMBER && a.s.parse::<i32>().is_ok_and(|n| n.to_string() == a.s) => {
                            fmt.push_str(&a.s);
                        }
                        [a] => match (var(self, a), string) {
                            (Some((name, Dt::Text)), true) => {
                                fmt.push('%');
                                values.push(format!("COALESCE({name}, '(null)')"));
                            }
                            (Some((name, Dt::Int)), false) => {
                                fmt.push('%');
                                values.push(format!("COALESCE(CAST({name} AS text), '(null)')"));
                            }
                            _ => return no("RAISERROR argument that is not a declared variable or constant of the type its specification takes (%s a string, %d/%i an int): T-SQL raises error 2786"),
                        },
                        _ => return Err(form()),
                    }
                }
                if rest.next().is_some() {
                    return no("RAISERROR with more arguments than its format uses: not converted");
                }
            }
            _ => return no("RAISERROR with a message number: the text comes from sys.messages; rewrite by hand"),
        }
        let chars = fmt.chars().count();
        if !values.is_empty() || chars == 0 {
            self.note(Status::Assumptions, "ASM-RAISERROR-TEXT", "The message is 1 to 2,047 characters long: T-SQL cuts a longer one to 2,044 characters and '...', and sends an empty one as a single space; PostgreSQL keeps the text as it is.");
        } else if chars > 2047 {
            return no("RAISERROR message longer than 2,047 characters: T-SQL cuts it; shorten it");
        }
        let fmt = fmt.replace('\'', "''");
        let values: String = values.iter().map(|v| format!(", {v}")).collect();
        if error {
            self.step_ts_with("tsql.stmt.raiserror", &ts, "raise-exception");
            self.note(Status::Assumptions, "ASM-THROW-ERRCODE", "The T-SQL error number (50000), severity and state are not carried over: callers see SQLSTATE P0001 with the same message.");
            return Ok(format!(
                "RAISE EXCEPTION '{fmt}'{values} USING ERRCODE = 'P0001'"
            ));
        }
        self.step_ts_with("tsql.stmt.raiserror", &ts, "raise-notice");
        self.note(Status::Assumptions, "ASM-RAISERROR-INFO", "Callers read the message by its text: a T-SQL informational RAISERROR also carries message number 50000, its severity and state, which a PostgreSQL notice does not.");
        Ok(format!("RAISE NOTICE '{fmt}'{values}"))
    }

    fn txn(&mut self, kw: &str, ts: &[T]) -> R<String> {
        if self.trigger.is_some() {
            return self.misplaced(trigger::TXN);
        }
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
                self.step_ts("tsql.txn.rollback-savepoint", &ts);
                return Ok(format!("ROLLBACK TO SAVEPOINT {n}"));
            }
        }
        let rule = match kw {
            "BEGIN" => "tsql.txn.begin",
            "COMMIT" => "tsql.txn.commit",
            _ => "tsql.txn.rollback",
        };
        self.step_ts(rule, &ts);
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
                self.txn_control = true;
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
        if self.trigger.is_some() {
            return self.misplaced(trigger::TXN);
        }
        if self.mode == Mode::Func {
            return self.misplaced("transaction control inside a function");
        }
        if self.mode == Mode::Top {
            self.script_commit_note();
        }
        let name = ts[j].s.to_ascii_lowercase();
        self.step_ts("tsql.txn.save", &ts);
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
        // name: dotted run of identifiers
        let mut j = s[1];
        let mut name_end = j + 1;
        while let Some(d) = next_sig(&ts, name_end - 1).filter(|&d| ts[d].k == DOT) {
            j = next_sig(&ts, d).ok_or(No("bad name".into()))?;
            name_end = j + 1;
        }
        let raw_name: String = ts[s[1]..name_end].iter().map(|t| t.s.clone()).collect();
        if raw_name
            .rsplit('.')
            .next()
            .is_some_and(|n| tsql_unquote(n).eq_ignore_ascii_case("sp_addextendedproperty"))
        {
            self.step_ts("tsql.stmt.extended-property", &ts);
            return self.extended_property(&ts[name_end..]);
        }
        if raw_name.rsplit('.').next().is_some_and(|n| {
            n.to_ascii_lowercase().starts_with("sp_") || n.to_ascii_lowercase().starts_with("xp_")
        }) {
            return no(format!(
                "system procedures have no {} equivalent",
                self.to.display_name()
            ));
        }
        let name = self.expr(&ts[s[1]..name_end]).trim().to_string();
        let output = ts.iter().any(|t| t.is("OUTPUT") || t.is("OUT"));
        let callee = self.callee(&raw_name);
        if output && callee.is_none() {
            return no("EXEC with OUTPUT arguments: the called procedure is not in the converted tree, so its parameters are unknown; CALL with INOUT parameters into variables");
        }
        let params = callee.map(|r| r.params.as_slice());
        let mut args = Vec::new();
        let mut passed: Vec<usize> = Vec::new();
        for (k, a) in split_commas(&ts[name_end..]).into_iter().enumerate() {
            let mut a = trimmed(a);
            if a.is_empty() {
                continue;
            }
            let marked = a.last().is_some_and(|t| t.is("OUTPUT") || t.is("OUT"));
            if marked {
                a = trimmed(&a[..a.len() - 1]);
            }
            let eq = a.iter().position(|x| x.k == OP && x.s == "=");
            let named = a.first().is_some_and(|x| x.k == VARIABLE)
                && eq.is_some_and(|e| a[1..e].iter().all(|x| !x.sig()));
            let value = if named {
                trimmed(&a[eq.unwrap() + 1..])
            } else {
                a
            };
            // The parameter this argument binds to, when the callee is known.
            let mut bit_param = false;
            if let Some(ps) = params {
                let at = if named {
                    ps.iter().position(|p| p.name.eq_ignore_ascii_case(&a[0].s))
                } else {
                    (k < ps.len()).then_some(k)
                };
                let Some(at) = at else {
                    return no(format!(
                        "EXEC: argument {} matches no parameter of {raw_name}",
                        k + 1
                    ));
                };
                passed.push(at);
                bit_param = ps[at].data_type.base == "BIT" && !marked;
                let out_param = matches!(ps[at].mode, ParamMode::Out | ParamMode::InOut);
                if out_param != marked {
                    return no(if out_param {
                        format!("EXEC: {} of {raw_name} is an OUTPUT parameter but the call does not pass OUTPUT; a CALL would assign the argument back", ps[at].name)
                    } else {
                        format!(
                            "EXEC: OUTPUT passed for {}, which {raw_name} does not declare OUTPUT",
                            ps[at].name
                        )
                    });
                }
                if marked
                    && !(value.len() == 1
                        && value[0].k == VARIABLE
                        && self.declared(&value[0].s).is_some())
                {
                    return no("EXEC: an OUTPUT argument that is not a declared variable");
                }
            }
            if named {
                if callee.is_none() {
                    self.note(Status::Assumptions, "ASM-PARAM-NAMES", "Named arguments assume the called procedure was converted with the same p_ parameter names.");
                }
                let v = if bit_param {
                    self.bit_store_text(value)
                } else {
                    self.expr(value).trim().to_string()
                };
                let arg = format!("p_{}", snake(&a[0].s[1..]));
                self.step_ts_with("tsql.exec.named-argument", a, arg.clone());
                args.push(format!("{arg} => {v}"));
            } else if bit_param {
                args.push(self.bit_store_text(value));
            } else {
                args.push(self.expr(value).trim().to_string());
            }
        }
        if let Some(ps) = params {
            if let Some(p) = ps.iter().enumerate().find(|(i, p)| {
                matches!(p.mode, ParamMode::Out | ParamMode::InOut) && !passed.contains(i)
            }) {
                return no(format!(
                    "EXEC: the OUTPUT parameter {} of {raw_name} is not passed; the target needs a variable for it",
                    p.1.name
                ));
            }
        }
        self.step_ts("tsql.stmt.exec", &ts);
        Ok(format!("CALL {name}({})", args.join(", ")))
    }

    /// The procedure `name` (as written, possibly qualified) in the source
    /// catalog, when exactly one matches. `dbo` and no schema are the same.
    fn callee(&self, name: &str) -> Option<&'a sqldiff_core::catalog::Routine> {
        let cat = self.catalog?;
        let parts: Vec<String> = name.split('.').map(tsql_unquote).collect();
        let (schema, proc) = match parts.as_slice() {
            [p] => (None, p.clone()),
            [.., s, p] => (Some(s.clone()), p.clone()),
            [] => return None,
        };
        let norm = |s: Option<&str>| {
            s.filter(|s| !s.eq_ignore_ascii_case("dbo"))
                .map(str::to_ascii_lowercase)
        };
        let mut hits = cat.routines.iter().filter(|r| {
            r.kind == sqldiff_core::catalog::RoutineKind::Procedure
                && r.name.eq_ignore_ascii_case(&proc)
                && norm(r.schema.as_deref()) == norm(schema.as_deref())
        });
        let hit = hits.next()?;
        hits.next().is_none().then_some(hit)
    }

    /// `sp_addextendedproperty` with literal arguments → `COMMENT ON`. Only
    /// objects whose target name is certain: schemas, tables, views, routines
    /// and columns (PostgreSQL also indexes and constraints, which live in
    /// the table's schema there but not on Db2 for i).
    fn extended_property(&mut self, args: &[T]) -> R<String> {
        const NAMES: [&str; 8] = [
            "name",
            "value",
            "level0type",
            "level0name",
            "level1type",
            "level1name",
            "level2type",
            "level2name",
        ];
        let mut val: HashMap<&str, T> = HashMap::new();
        for (k, a) in split_commas(args).into_iter().enumerate() {
            let a = trimmed(a);
            let (key, v) = match a.iter().position(|x| x.k == OP && x.s == "=") {
                Some(eq) if a.first().is_some_and(|x| x.k == VARIABLE) => {
                    let key = a[0].s[1..].to_ascii_lowercase();
                    let key = NAMES.iter().find(|n| **n == key).ok_or(No(format!(
                        "sp_addextendedproperty: unknown argument {}",
                        a[0].s
                    )))?;
                    (*key, trimmed(&a[eq + 1..]))
                }
                _ => (
                    *NAMES
                        .get(k)
                        .ok_or(No("sp_addextendedproperty: too many arguments".into()))?,
                    a,
                ),
            };
            match v {
                [t] if t.k == STRING => {
                    val.insert(key, t.clone());
                }
                [t] if t.is("NULL") => {}
                _ => return no("sp_addextendedproperty with a non-literal argument: no rule yet"),
            }
        }
        let text = |k: &str| val.get(k).map(|t| string_value(&t.s));
        let ident = |k: &str| text(k).map(|n| quoted(&n));
        let kind = |k: &str| text(k).map(|t| t.to_ascii_uppercase());
        let value = val
            .get("value")
            .ok_or(No("sp_addextendedproperty without a value".into()))?
            .clone();
        let target = match (kind("level0type"), kind("level1type"), kind("level2type")) {
            (Some(s), None, None) if s == "SCHEMA" => {
                format!("SCHEMA {}", ident("level0name").unwrap_or_default())
            }
            (Some(s), Some(o), l2) if s == "SCHEMA" => {
                let schema = ident("level0name")
                    .ok_or(No("sp_addextendedproperty without a schema name".into()))?;
                let obj = format!(
                    "{schema}.{}",
                    ident("level1name")
                        .ok_or(No("sp_addextendedproperty without an object name".into()))?
                );
                let sub = ident("level2name");
                match (o.as_str(), l2.as_deref(), sub) {
                    ("TABLE" | "VIEW", None, _) if self.db2() => format!("TABLE {obj}"),
                    ("TABLE" | "VIEW", None, _) => format!("{o} {obj}"),
                    ("PROCEDURE" | "FUNCTION", None, _) => format!("{o} {obj}"),
                    ("TABLE" | "VIEW", Some("COLUMN"), Some(c)) => format!("COLUMN {obj}.{c}"),
                    ("TABLE", Some("INDEX"), Some(i)) if !self.db2() => {
                        format!("INDEX {schema}.{i}")
                    }
                    ("TABLE", Some("CONSTRAINT"), Some(c)) if !self.db2() => {
                        format!("CONSTRAINT {c} ON {obj}")
                    }
                    _ => {
                        return no(format!(
                            "sp_addextendedproperty on {o}{}: no rule for this object kind yet",
                            l2.map(|l| format!(" {l}")).unwrap_or_default()
                        ))
                    }
                }
            }
            _ => return no("sp_addextendedproperty outside a schema: no rule yet"),
        };
        let pname = text("name").unwrap_or_default();
        let ttext = format!(
            "The extended property `{pname}` becomes the object's comment. {} keeps one unnamed comment per object, so the property name is dropped and a second property on the same object replaces the first.",
            self.to.display_name()
        );
        self.note(Status::Assumptions, "ASM-EXTPROP-COMMENT", &ttext);
        let v = self.expr(std::slice::from_ref(&value)).trim().to_string();
        Ok(format!("COMMENT ON {target} IS {v}"))
    }

    fn create(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        let s = sig(&ts);
        let w1 = ts[*s.get(1).ok_or(No("incomplete CREATE".into()))?].up();
        match w1.as_str() {
            "TABLE" => {
                // The table's own BIT columns, for its CHECK constraints.
                let cols = ts
                    .iter()
                    .position(|t| t.k == L_PAREN)
                    .and_then(|o| Some((o, matching(&ts, o)?)))
                    .map(|(o, c)| {
                        split_commas(&ts[o + 1..c])
                            .iter()
                            .filter_map(|item| {
                                let s = sig(item);
                                let (&n, &t) = (s.first()?, s.get(1)?);
                                (!constraint_item(&item[n])
                                    && tsql_unquote(&item[t].s).eq_ignore_ascii_case("bit"))
                                .then(|| tsql_unquote(&item[n].s).to_ascii_lowercase())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.with_bit_cols(cols, |cx| cx.create_table(&ts))
            }
            "UNIQUE" | "INDEX" | "CLUSTERED" | "NONCLUSTERED" => {
                if let Some(t) = ts
                    .iter()
                    .take_while(|t| !t.is("INDEX"))
                    .find(|t| t.is("COLUMNSTORE") || t.is("XML") || t.is("SPATIAL"))
                {
                    return no(format!(
                        "{} index: {} has no counterpart; create a plain index (or none) after review",
                        t.up(),
                        self.to.display_name()
                    ));
                }
                let ts = self.strip_partition_scheme(&ts);
                let ts = self.strip_storage(&ts)?;
                let ts = self.strip_after_filter(&ts)?;
                if self.db2() {
                    if let Some(w) = ts.iter().find(|t| t.is("INCLUDE") || t.is("WHERE")) {
                        return no(format!("index with {}: not checked for Db2 for i; create a plain index (or an encoded vector or derived-key index) after review", w.up()));
                    }
                }
                for t in ts
                    .iter()
                    .filter(|t| t.is("CLUSTERED") || t.is("NONCLUSTERED"))
                {
                    self.step_ts("tsql.index.clustered", std::slice::from_ref(t));
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
                let text = self.pick(
                    "PostgreSQL index names are schema objects (T-SQL's are per table): no other index, table, view or sequence in the schema may have this name.",
                    "Db2 for i index names are schema objects (T-SQL's are per table): no other index, table, view or sequence in the schema may have this name.",
                ).to_string();
                self.note(Status::Assumptions, "ASM-INDEX-NAME-SCHEMA", &text);
                if find_word(&kept, 0, "WHERE").is_some() {
                    self.note(Status::Assumptions, "ASM-TSQL-FILTERED-INDEX-SET-OPTIONS", "SQL Server rejects changes to a table with a filtered index from sessions with ANSI_NULLS, QUOTED_IDENTIFIER, ANSI_PADDING, ANSI_WARNINGS, ARITHABORT or CONCAT_NULL_YIELDS_NULL OFF (or NUMERIC_ROUNDABORT ON); the target does not, so such sessions are assumed not to exist.");
                }
                // Db2 for i creates an unqualified index name in the default
                // schema; T-SQL's index belongs to its table.
                let kept = if self.db2() {
                    let q = qualify_index(kept.clone());
                    if self.source(&q) != self.source(&kept) {
                        self.step_ts("tsql.index.qualify", &kept);
                    }
                    q
                } else {
                    kept
                };
                // T-SQL's unique index holds one NULL (NULLs are equal keys);
                // PostgreSQL's holds any number unless NULLS NOT DISTINCT.
                let kept = if !self.db2() && w1 == "UNIQUE" {
                    self.note(Status::Converted, "tsql.unique-nulls", "UNIQUE index gets NULLS NOT DISTINCT: in T-SQL a unique index treats NULLs as equal keys, so it holds one row with NULL, as NULLS NOT DISTINCT does.");
                    nulls_not_distinct(kept)
                } else {
                    kept
                };
                // A filtered index's WHERE on BIT columns of the ON table.
                let cols = find_word(&kept, 0, "ON")
                    .map(|on| {
                        let end = (on + 1..kept.len())
                            .find(|&j| kept[j].k == L_PAREN)
                            .unwrap_or(kept.len());
                        self.table_bit_cols(&kept[on + 1..end])
                    })
                    .unwrap_or_default();
                self.with_bit_cols(cols, |cx| Ok(collapse_spaces(&cx.expr(&kept))))
            }
            "SCHEMA" => self.create_schema(&ts),
            "SEQUENCE" => self.create_sequence(&ts, s[2..].to_vec()),
            _ => no(format!("CREATE {w1}: no rule yet")),
        }
    }

    /// `CREATE SCHEMA s [AUTHORIZATION owner]`. T-SQL's `dbo` is the
    /// database owner, a principal the target has no counterpart of by that
    /// name: the target schema goes to the user running the script.
    fn create_schema(&mut self, ts: &[T]) -> R<String> {
        let s = sig(ts);
        let body: Vec<usize> = s
            .iter()
            .copied()
            .filter(|&j| ts[j].k != SEMICOLON)
            .collect();
        match body.as_slice() {
            [_, _, name] => Ok(self.expr(&ts[..=*name])),
            [_, _, name, auth, owner] if ts[*auth].is("AUTHORIZATION") => {
                if tsql_unquote(&ts[*owner].s).eq_ignore_ascii_case("dbo") {
                    self.step_ts("tsql.schema.authorization-dbo", &ts[*auth..=*owner]);
                    let text = self.pick(
                        "AUTHORIZATION dbo is dropped: PostgreSQL has no role dbo, so the schema is owned by the user running the script, which should be the database owner.",
                        "AUTHORIZATION dbo is dropped: Db2 for i has no user profile DBO, so the schema (library) is owned as the user running the script creates it.",
                    ).to_string();
                    self.note(Status::Assumptions, "ASM-SCHEMA-OWNER", &text);
                    Ok(self.expr(&ts[..=*name]))
                } else {
                    let text = format!(
                        "The schema owner {} must exist on {} and hold the privileges it holds in SQL Server.",
                        tsql_unquote(&ts[*owner].s),
                        self.to.display_name()
                    );
                    self.note(Status::Assumptions, "ASM-SCHEMA-OWNER", &text);
                    Ok(self.expr(&ts[..=*owner]))
                }
            }
            _ => no("CREATE SCHEMA with schema elements or options: no rule yet"),
        }
    }

    /// An index's `ON scheme (column)` after its column list: partitioning
    /// places rows, it does not change which rows an index holds or rejects.
    fn strip_partition_scheme(&mut self, ts: &[T]) -> Vec<T> {
        let ds = crate::tok::depths(ts);
        let mut cut = vec![false; ts.len()];
        let mut dropped = false;
        for i in 0..ts.len() {
            if ds[i] != 0 || !ts[i].is("ON") || !prev_sig(ts, i).is_some_and(|p| ts[p].k == R_PAREN)
            {
                continue;
            }
            let Some(name) = next_sig(ts, i) else {
                continue;
            };
            let Some(open) = next_sig(ts, name).filter(|&p| ts[p].k == L_PAREN) else {
                continue;
            };
            let Some(close) = matching(ts, open) else {
                continue;
            };
            cut[i..=close].iter_mut().for_each(|c| *c = true);
            self.step_ts("tsql.partition-scheme", &ts[i..=close]);
            dropped = true;
        }
        if dropped {
            self.note(Status::Converted, "tsql.partition-scheme", "The index's partition scheme (ON scheme (column)) was dropped: partitioning places rows, it does not change which rows the index holds or rejects.");
        }
        ts.iter()
            .zip(cut)
            .filter(|(_, c)| !c)
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// An index's `WITH (…)` and `ON filegroup` after its filter: removed
    /// like `strip_storage` does after the column list (PostgreSQL wants
    /// WITH before WHERE, and a filegroup has no counterpart).
    fn strip_after_filter(&mut self, ts: &[T]) -> R<Vec<T>> {
        let Some(w) = find_word(ts, 0, "WHERE") else {
            return Ok(ts.to_vec());
        };
        let ds = crate::tok::depths(ts);
        let mut cut = vec![false; ts.len()];
        let mut i = w + 1;
        let mut dropped = false;
        while i < ts.len() {
            if ds[i] == 0 && ts[i].is("WITH") {
                let Some(open) = next_sig(ts, i).filter(|&p| ts[p].k == L_PAREN) else {
                    return no("index WITH option without parentheses: no rule yet");
                };
                let r = matching(ts, open).ok_or(No("unbalanced parentheses".into()))?;
                for opt in split_commas(&ts[open + 1..r]) {
                    storage_option(opt)?;
                }
                cut[i..=r].iter_mut().for_each(|c| *c = true);
                self.step_ts("tsql.storage-options", &ts[i..=r]);
                dropped = true;
                i = r + 1;
                continue;
            }
            if ds[i] == 0 && (ts[i].is("ON") || ts[i].is("FILESTREAM_ON")) {
                let nx = next_sig(ts, i).unwrap_or(i);
                if let Some(open) = next_sig(ts, nx).filter(|&p| ts[p].k == L_PAREN) {
                    let r = matching(ts, open).ok_or(No("unbalanced parentheses".into()))?;
                    cut[i..=r].iter_mut().for_each(|c| *c = true);
                    self.step_ts("tsql.storage-options", &ts[i..=r]);
                    dropped = true;
                    i = r + 1;
                    continue;
                }
                if ts[i].is("FILESTREAM_ON") {
                    return no("FILESTREAM_ON: no rule yet");
                }
                cut[i..=nx].iter_mut().for_each(|c| *c = true);
                self.step_ts("tsql.storage-options", &ts[i..=nx]);
                dropped = true;
                i = nx + 1;
                continue;
            }
            i += 1;
        }
        if dropped {
            self.note(Status::Converted, "tsql.storage-options", "Filegroup and storage options (ON [PRIMARY], WITH (…)) were dropped; they do not change results.");
        }
        Ok(ts
            .iter()
            .zip(cut)
            .filter(|(_, c)| !c)
            .map(|(t, _)| t.clone())
            .collect())
    }

    /// Removes `ON [PRIMARY]`, `TEXTIMAGE_ON x` and `WITH (…)` after the main column list.
    fn strip_storage(&mut self, ts: &[T]) -> R<Vec<T>> {
        let ds = crate::tok::depths(ts);
        let mut cut = vec![false; ts.len()];
        let mut i = 0;
        let mut dropped = false;
        while i < ts.len() {
            let after_paren = prev_sig(ts, i).is_some_and(|p| ts[p].k == R_PAREN);
            if ds[i] == 0 && after_paren && (ts[i].is("ON") || ts[i].is("TEXTIMAGE_ON")) {
                let nx = next_sig(ts, i).unwrap_or(i);
                cut[i..=nx].iter_mut().for_each(|c| *c = true);
                self.step_ts("tsql.storage-options", &ts[i..=nx]);
                dropped = true;
                i = nx + 1;
                continue;
            }
            if ds[i] == 0 && after_paren && ts[i].is("WITH") {
                if let Some(r) = next_sig(ts, i).and_then(|p| matching(ts, p)) {
                    let open = next_sig(ts, i).unwrap();
                    for opt in split_commas(&ts[open + 1..r]) {
                        storage_option(opt)?;
                    }
                    cut[i..=r].iter_mut().for_each(|c| *c = true);
                    self.step_ts("tsql.storage-options", &ts[i..=r]);
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
        Ok(ts
            .iter()
            .zip(cut)
            .filter(|(_, c)| !c)
            .map(|(t, _)| t.clone())
            .collect())
    }

    /// `CREATE SEQUENCE` with every bound written out: T-SQL starts an
    /// ascending sequence at the type's minimum (-2147483648 for INT), the
    /// targets at 1, so nothing is left to the target's defaults.
    fn create_sequence(&mut self, ts: &[T], rest: Vec<usize>) -> R<String> {
        let Some((&first, rest)) = rest.split_first() else {
            return no("CREATE SEQUENCE without a name");
        };
        // Name: a dotted run.
        let mut k = 0;
        let mut name_end = first;
        while rest.get(k).is_some_and(|&j| ts[j].k == DOT) {
            name_end = *rest
                .get(k + 1)
                .ok_or(No("unfinished sequence name".into()))?;
            k += 2;
        }
        let name = self.expr(&ts[first..=name_end]).trim().to_string();
        let words: Vec<&T> = rest[k..].iter().map(|&j| &ts[j]).collect();
        let mut ty = "BIGINT".to_string();
        let (mut start, mut inc, mut min, mut max) = (None, None, None, None);
        let mut cycle = false;
        let number = |w: &[&T], at: usize| -> R<(String, usize)> {
            match w.get(at..) {
                Some([op, n, ..]) if op.k == OP && op.s == "-" && n.k == NUMBER => {
                    Ok((format!("-{}", n.s), at + 2))
                }
                Some([n, ..]) if n.k == NUMBER => Ok((n.s.clone(), at + 1)),
                _ => no("CREATE SEQUENCE option without a number"),
            }
        };
        let mut i = 0;
        while i < words.len() {
            let w = words[i].up();
            match w.as_str() {
                "AS" => {
                    ty = words
                        .get(i + 1)
                        .map(|t| tsql_unquote(&t.s).to_ascii_uppercase())
                        .unwrap_or_default();
                    i += 2;
                }
                "START" | "INCREMENT" => {
                    let (n, next) = number(&words, i + 2)?;
                    if w == "START" {
                        start = Some(n)
                    } else {
                        inc = Some(n)
                    }
                    i = next;
                }
                "MINVALUE" | "MAXVALUE" => {
                    let (n, next) = number(&words, i + 1)?;
                    if w == "MINVALUE" {
                        min = Some(n)
                    } else {
                        max = Some(n)
                    }
                    i = next;
                }
                "NO" if words.get(i + 1).is_some_and(|t| {
                    t.is("MINVALUE") || t.is("MAXVALUE") || t.is("CYCLE") || t.is("CACHE")
                }) =>
                {
                    i += 2
                }
                "CYCLE" => {
                    cycle = true;
                    i += 1;
                }
                "CACHE" => {
                    i += 1;
                    if words.get(i).is_some_and(|t| t.k == NUMBER) {
                        i += 1;
                    }
                }
                _ if words[i].k == SEMICOLON => i += 1,
                _ => {
                    return no(format!(
                        "CREATE SEQUENCE option {}: no rule yet",
                        words[i].s
                    ))
                }
            }
        }
        let (target, lo, hi) = match ty.as_str() {
            "INT" | "INTEGER" => ("INTEGER", "-2147483648", "2147483647"),
            "BIGINT" => ("BIGINT", "-9223372036854775808", "9223372036854775807"),
            "SMALLINT" => ("SMALLINT", "-32768", "32767"),
            "TINYINT" => ("SMALLINT", "0", "255"),
            _ => return no(format!("CREATE SEQUENCE AS {ty}: no rule yet")),
        };
        let inc = inc.unwrap_or_else(|| "1".into());
        let min = min.unwrap_or_else(|| lo.into());
        let max = max.unwrap_or_else(|| hi.into());
        let start = start.unwrap_or_else(|| {
            if inc.starts_with('-') {
                max.clone()
            } else {
                min.clone()
            }
        });
        let text = self.pick(
            "The cache setting is left to PostgreSQL's default (CACHE 1): a larger PostgreSQL cache is per session, so values would no longer be handed out in order across sessions.",
            "NO CACHE: Db2 for i hands out values in order, as T-SQL's server-wide cache does.",
        ).to_string();
        self.note(Status::Converted, "tsql.sequence-cache", &text);
        self.step_ts_with("tsql.sequence", ts, target);
        Ok(format!(
            "CREATE SEQUENCE {name}\n    AS {}\n    START WITH {start}\n    INCREMENT BY {inc}\n    MINVALUE {min}\n    MAXVALUE {max}\n    {}{}",
            if self.db2() { target.to_string() } else { target.to_ascii_lowercase() },
            if cycle { "CYCLE" } else { "NO CYCLE" },
            if self.db2() { "\n    NO CACHE" } else { "" }
        ))
    }

    fn create_table(&mut self, ts: &[T]) -> R<String> {
        let (temporal, ts) = if ts
            .iter()
            .any(|t| t.is("SYSTEM_TIME") || t.is("SYSTEM_VERSIONING"))
        {
            let (t, ts) = self.temporal(ts)?;
            (Some(t), ts)
        } else {
            (None, ts.to_vec())
        };
        let ts = self.strip_storage(&ts)?;
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
        let mut names = Vec::new();
        for item in split_commas(&ts[open + 1..close]) {
            let col = self.table_item(item, &pk)?;
            if let Some(&f) = sig(item).first().filter(|&&f| !constraint_item(&item[f])) {
                names.push((
                    tsql_unquote(&item[f].s),
                    self.expr(&item[f..=f]).trim().to_string(),
                ));
            }
            cols.push(col);
        }
        let tail = self.expr(&ts[close + 1..]);
        if let Some(t) = temporal {
            let table = Ok(format!(
                "CREATE TABLE {name} ({}\n){}",
                cols.join(","),
                tail.trim_end()
            ));
            let versioning = self.versioning(&t, &name, &name_t, &names)?;
            return table.map(|t| format!("{t};\n\n{versioning}"));
        }
        if temp {
            let how = self.pick("temp", "declare-global-temporary");
            self.step_ts_with("tsql.table.temp", &ts[..open], how);
        }
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
        let Some(first) = it.first() else {
            // `(a INT, b INT,)`: SQL Server takes the trailing comma; refuse
            // rather than guess what the targets make of an empty item.
            return no("empty item in the column list (a trailing comma)");
        };
        if constraint_item(first) {
            // PRIMARY KEY (a ASC): the targets take no sort order in a key's
            // column list. The order only shapes the key's index.
            let key = it.iter().any(|t| t.is("PRIMARY") || t.is("UNIQUE"));
            let ds = crate::tok::depths(it);
            let order = |j: usize| key && ds[j] == 1 && (it[j].is("ASC") || it[j].is("DESC"));
            if (0..it.len()).any(|j| order(j) && it[j].is("DESC")) {
                let text = self.pick(
                    "DESC in a key's column list was dropped: PostgreSQL keys take no sort order. Results are the same; only the key's index is built ascending.",
                    "DESC in a key's column list was dropped: Db2 for i keys take no sort order. Results are the same; only the key's index is built ascending.",
                ).to_string();
                self.note(Status::Converted, "tsql.key-order", &text);
            }
            for j in 0..it.len() {
                if it[j].is("CLUSTERED") || it[j].is("NONCLUSTERED") {
                    self.step_ts("tsql.index.clustered", &it[j..=j]);
                } else if order(j) {
                    self.step_ts("tsql.key-order", &it[j..=j]);
                }
            }
            let kept: Vec<T> = (0..it.len())
                .filter(|&j| {
                    let t = &it[j];
                    let before_order = !t.sig() && next_sig(it, j).is_some_and(order);
                    !(t.is("CLUSTERED") || t.is("NONCLUSTERED") || order(j) || before_order)
                })
                .map(|j| it[j].clone())
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
        // A qualified type name (`sys.geography`, `dbo.Phone`) is one type.
        let mut name_end = ty_at;
        while let Some(d) = next_sig(it, name_end).filter(|&d| it[d].k == DOT) {
            name_end = next_sig(it, d).ok_or(No("unfinished type name".into()))?;
        }
        let ty_end = if next_sig(it, name_end).is_some_and(|j| it[j].k == L_PAREN) {
            matching(it, next_sig(it, name_end).unwrap()).ok_or(No("unbalanced type".into()))?
        } else {
            name_end
        };
        let (ty, _, _) = self.ty(&it[ty_at..=ty_end])?;
        let mut rest: Vec<T> = Vec::new();
        let mut gen = String::new();
        let mut j = ty_end + 1;
        while j < it.len() {
            let t = &it[j];
            if t.is("IDENTITY") {
                let at_identity = j;
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
                self.step_ts("tsql.table.identity", &it[at_identity..j]);
                self.note(Status::Assumptions, "ASM-IDENTITY", "IDENTITY becomes GENERATED ALWAYS AS IDENTITY: explicit values need OVERRIDING SYSTEM VALUE, as T-SQL needs IDENTITY_INSERT. Values used by rolled-back inserts are lost in both.");
                continue;
            }
            if t.is("CLUSTERED") || t.is("NONCLUSTERED") || t.is("ROWGUIDCOL") {
                self.step_ts("tsql.table.column-option-dropped", &it[j..=j]);
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
                rest.push(T::made(
                    NUMBER,
                    format!("DEFAULT {val}"),
                    t.pos.0,
                    "tsql.table.column-default",
                ));
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
            self.step_ts("tsql.table.primary-key-not-null", it);
            rest = format!("NOT NULL {}", rest.trim());
        }
        if ty == "boolean" {
            let b = bits::bool_default(&rest);
            if b != rest {
                self.step_ts("tsql.bit.boolean-default", it);
            }
            rest = b;
        }
        let rest = rest.trim();
        Ok(format!(
            "{name} {ty}{gen}{}{rest}",
            if rest.is_empty() { "" } else { " " }
        ))
    }

    fn alter_table(&mut self, ts: &[T]) -> R<String> {
        let ts = Self::body_toks(ts);
        if ts
            .iter()
            .any(|t| t.is("SYSTEM_VERSIONING") || t.is("SYSTEM_TIME"))
        {
            return no(self.pick(
                "ALTER TABLE … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME: in PostgreSQL versioning is the history trigger of the converted CREATE TABLE; turning it off, dropping the period or adding it later needs those triggers changed to match; no rule yet",
                "ALTER TABLE … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME: Db2 for i system-period tables are not converted yet (their facts are not verified on IBM i); no rule yet",
            ));
        }
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
                let name_end = find_word(&ts, 0, "ADD").unwrap_or(ts.len());
                let cols = find_word(&ts, 0, "TABLE")
                    .map(|t| self.table_bit_cols(&ts[t + 1..name_end]))
                    .unwrap_or_default();
                return self.with_bit_cols(cols, |cx| Ok(cx.expr(&ts)));
            }
            let head = self.expr(&ts[..nx]);
            let cols: R<Vec<String>> = split_commas(&ts[nx..])
                .iter()
                .map(|c| {
                    self.step_ts("tsql.table.add-column", c);
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

    fn is_varbinary_var(&self, v: &str) -> bool {
        self.vars
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
            .is_some_and(|x| x.dt == Dt::VarBinary)
    }

    /// `ts` is binary operands (`0x…` constants, VARBINARY variables) joined
    /// by `+` and nothing else. Only then is every `+` in it a binary
    /// concatenation: next to a number or a string, T-SQL converts the
    /// binary value instead (int and the string types rank above varbinary).
    fn binary_chain(&self, ts: &[T]) -> bool {
        let s = sig(ts);
        !s.is_empty()
            && s.len() % 2 == 1
            && s.iter().enumerate().all(|(n, &j)| {
                let t = &ts[j];
                if n % 2 == 1 {
                    return t.k == OP && t.s == "+";
                }
                match t.k {
                    NUMBER => t.s.len() >= 2 && t.s[..2].eq_ignore_ascii_case("0x"),
                    VARIABLE => self.is_varbinary_var(&t.s),
                    _ => false,
                }
            })
    }

    fn binary_concat_note(&mut self, at: &[T]) {
        self.step_ts("tsql.expr.binary-concat", at);
        self.note(Status::Assumptions, "ASM-CONCAT-NULL", "Binary + becomes ||; both give NULL when an operand is NULL (CONCAT_NULL_YIELDS_NULL ON, the default).");
    }

    fn is_string_var(&self, v: &str) -> bool {
        self.vars
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
            .is_some_and(|x| x.string)
    }

    /// A variable at `t`, as the output spells it.
    fn var_ref(&mut self, t: &T) -> String {
        let v = t.s.as_str();
        let at = std::slice::from_ref(t);
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
            let rule = format!("tsql.{}", up.trim_start_matches('@').to_ascii_lowercase());
            self.step_ts(&rule, at);
            self.note(Status::NotConverted, &rule, reason);
            return v.to_string();
        }
        if let Some(name) = self.declared(v) {
            self.step_ts_with("tsql.expr.variable", at, name.clone());
            return name;
        }
        if let Some(t) = self
            .tables
            .get(&v.trim_start_matches('@').to_ascii_lowercase())
            .cloned()
        {
            self.step_ts_with("tsql.expr.table-variable", at, t.clone());
            return t;
        }
        self.note(Status::Assumptions, "ASM-UNDECLARED-VAR", UNDECLARED);
        let name = undeclared_name(v);
        self.step_ts_with("tsql.expr.undeclared-variable", at, name.clone());
        name
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
            // A call to a built-in that returns a string: `CHAR(13) + CHAR(10)`.
            IDENT if next_sig(ts, j).is_some_and(|n| ts[n].k == L_PAREN) => {
                (string_builtin(ts, j), false)
            }
            R_PAREN => {
                let open = (0..j)
                    .rev()
                    .find(|&o| ts[o].k == L_PAREN && matching(ts, o) == Some(j));
                let name = open.and_then(|o| prev_sig(ts, o));
                (name.is_some_and(|n| string_builtin(ts, n)), false)
            }
            _ => (false, false),
        }
    }

    fn expr(&mut self, ts: &[T]) -> String {
        let paged = self.paging(ts);
        let ts: &[T] = &paged;
        let mut edits = self.bit_edits(ts);
        if let Some(outer) = self.bit_outer.take() {
            edits.wrap(outer);
        }
        for &(rule, a, b) in &edits.steps {
            if let Some(at) = ts.get(a..=b) {
                self.step_ts(rule, at);
            }
        }
        let mut o = String::new();
        let mut i = 0;
        let mut done = 0;
        while i < ts.len() {
            // After the tokens output since the last step, before this one.
            for k in done..i {
                if let Some(p) = edits.post.get(&k) {
                    o.push_str(p);
                }
            }
            done = i;
            if let Some(p) = edits.pre.get(&i) {
                o.push_str(p);
            }
            if let Some(r) = edits.rep.get(&i) {
                o.push_str(r);
                i += 1;
                continue;
            }
            let (at, stepped) = (o.len(), self.stepped);
            let next = self.expr_at(ts, i, &mut o);
            // Completeness of the trace: a branch that wrote anything but
            // the input it consumed recorded the rule that did it.
            debug_assert!(
                self.stepped > stepped || o.get(at..) == Some(self.source(&ts[i..next]).as_str()),
                "the expression walk rewrote {:?} as {:?} without a trace step",
                self.source(&ts[i..next]),
                o.get(at..)
            );
            i = next;
        }
        for k in done..ts.len() {
            if let Some(p) = edits.post.get(&k) {
                o.push_str(p);
            }
        }
        o
    }

    /// The input text of a token slice (empty for made-up tokens).
    fn source(&self, ts: &[T]) -> String {
        ts.iter()
            .map(|t| self.src.get(t.pos.0..t.pos.1).unwrap_or(""))
            .collect()
    }

    /// One step of the expression walk at `ts[i]`: writes its output to
    /// `o` and returns the index of the next token to walk. Every branch
    /// that writes something else than the input it consumed records the
    /// rule that rewrote it (`expr` checks this in debug builds).
    fn expr_at(&mut self, ts: &[T], i: usize, o: &mut String) -> usize {
        let t = &ts[i];
        if let Some(rule) = t.made {
            // A token a rule added (Db2's FROM SYSIBM.SYSDUMMY1, ...).
            self.step(rule, t.pos);
        }
        if self.db2() && matches!(t.k, IDENT | QUOTED_IDENT) && from_item_start(ts, i) {
            if let Some((text, end)) = self.table_function_db2(ts, i) {
                self.step_ts("tsql.from.table-function", &ts[i..end]);
                o.push_str(&text);
                return end;
            }
        }
        match t.k {
            INFERRED_TERMINATOR => {}
            VARIABLE => {
                let r = self.var_ref(t);
                o.push_str(&r)
            }
            QUOTED_IDENT => {
                let q = quoted(&t.s);
                if q != t.s {
                    self.step_ts("tsql.expr.quoted-ident", &ts[i..=i]);
                }
                o.push_str(&q)
            }
            NUMBER if t.s.len() >= 2 && t.s[..2].eq_ignore_ascii_case("0x") => {
                // A binary constant. PostgreSQL 16+ reads 0x… as an integer.
                let mut hex = t.s[2..].to_ascii_uppercase();
                if hex.len() % 2 == 1 {
                    hex.insert(0, '0');
                }
                let text = self.pick(
                    "0x… is a binary constant and becomes a bytea literal. T-SQL also converts it to a number where one is expected (`flags & 0x01`); there the output fails instead.",
                    "0x… is a binary constant and becomes BX'…'. T-SQL also converts it to a number where one is expected (`flags & 0x01`); there the output fails instead.",
                ).to_string();
                self.note(Status::Assumptions, "ASM-HEX-BINARY", &text);
                self.step_ts("tsql.expr.binary-literal", &ts[i..=i]);
                if self.db2() {
                    o.push_str(&format!("BX'{hex}'"));
                } else {
                    o.push_str(&format!("'\\x{hex}'::bytea"));
                }
            }
            STRING => {
                let lit = t.s.strip_prefix(['N', 'n']).unwrap_or(&t.s);
                if lit != t.s {
                    self.step_ts("tsql.expr.unicode-literal", &ts[i..=i]);
                }
                if !self.db2() && rounds_past_year_9999(&string_value(lit)) {
                    self.step_ts("tsql.end-of-time-literal", &ts[i..=i]);
                    self.note(Status::Differs, "tsql.end-of-time-literal", "This literal is the end of time at 100 ns precision (DATETIME2(7)). PostgreSQL keeps microseconds and rounds it up to 10000-01-01 00:00:00, so it no longer equals the largest stored value (a system-versioned table's current rows end at 9999-12-31 23:59:59.999999). Write '9999-12-31 23:59:59.999999' if it means the end of time.");
                }
                o.push_str(lit);
                self.string_compare(ts, i);
                self.fraction_note(t, lit);
            }
            // `0x01 + @b`: every operand is binary, so `+` concatenates.
            OP if t.s == "+" && self.binary_chain(ts) => {
                self.binary_concat_note(&ts[i..=i]);
                o.push_str("||");
            }
            OP if t.s == "+" => {
                let l = self.operand_kind(ts, prev_sig(ts, i));
                let r = self.operand_kind(ts, next_sig(ts, i));
                let unary = prev_sig(ts, i).map_or(true, |p| {
                    matches!(ts[p].k, OP | L_PAREN | COMMA)
                        || ts[p].k == IDENT && is_clause_word(&ts[p].s)
                });
                // `+` with a number is addition in T-SQL even next to a
                // string (int has the higher precedence: `'1' + 1` is 2), so
                // it stays `+` and fails on the target instead of becoming
                // a different value.
                if !unary && (l.0 || r.0) && !(l.1 || r.1) {
                    self.note(Status::Assumptions, "ASM-CONCAT-NULL", "String + becomes ||; both give NULL when an operand is NULL (CONCAT_NULL_YIELDS_NULL ON, the default).");
                    self.step_ts("tsql.expr.concat", &ts[i..=i]);
                    o.push_str("||");
                } else {
                    if !unary && !(l.1 || r.1) {
                        self.step_ts("tsql.expr.plus-numeric", &ts[i..=i]);
                        self.note(Status::Assumptions, "ASM-PLUS-NUMERIC", "`+` is kept as addition: the operands are assumed to be numbers. If they are strings, use || instead.");
                    }
                    o.push('+');
                }
            }
            OP if matches!(t.s.as_str(), "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=") => {
                o.push_str(&t.s);
                self.string_compare(ts, i);
            }
            IDENT if from_item_start(ts, i) && self.missing_image(ts, i).is_some() => {
                let r = self.missing_image(ts, i).unwrap();
                self.step_ts("tsql.trigger.missing-image", &ts[i..=i]);
                o.push_str(&r);
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
                    self.step_ts("tsql.expr.dbo-schema", &ts[i..=nx.unwrap()]);
                    return nx.unwrap() + 1;
                }
                if up == "NEXT" && !self.db2() {
                    if let Some((text, end)) = self.next_value(ts, i) {
                        self.step_ts("tsql.expr.next-value", &ts[i..=end]);
                        o.push_str(&text);
                        return end + 1;
                    }
                }
                if up == "WITH" && nx.is_some_and(|p| ts[p].k == L_PAREN) {
                    if let Some(r) = self.table_hint(ts, nx.unwrap()) {
                        self.step_ts("tsql.from.table-hint", &ts[i..=r]);
                        // drop trailing whitespace before WITH
                        while o.ends_with(' ') {
                            o.pop();
                        }
                        return r + 1;
                    }
                }
                if up == "STRING_AGG" {
                    if let Some((text, end)) = self.string_agg(ts, nx) {
                        self.step_ts("tsql.func.string_agg", &ts[i..=end]);
                        o.push_str(&text);
                        return end + 1;
                    }
                }
                if let Some(p) = nx.filter(|&p| ts[p].k == L_PAREN) {
                    if let Some(r) = matching(ts, p) {
                        // `dbo.CHAR(5)` is a user function, not the built-in.
                        let qualified = prev_sig(ts, i).is_some_and(|d| ts[d].k == DOT);
                        let rewritten = if qualified {
                            None
                        } else {
                            // A call the rules leave as written keeps no step
                            // of the arguments they looked at on the way.
                            let kept = self.trace.len();
                            let s = self.func(&up, &ts[p + 1..r]);
                            if s.is_none() {
                                self.trace.truncate(kept);
                            }
                            s
                        };
                        if let Some(s) = rewritten {
                            // Every rewritten built-in call is one step,
                            // named after the function.
                            let rule = format!("tsql.func.{}", up.to_ascii_lowercase());
                            self.step_ts(&rule, &ts[i..=r]);
                            o.push_str(&s);
                            return r + 1;
                        }
                        // Only a call: not a qualified name, an object
                        // name before its column list (`INSERT INTO log (…)`)
                        // or a CTE (`x (a) AS (…)`).
                        const OBJECT: [&str; 18] = [
                            "INTO",
                            "INSERT",
                            "UPDATE",
                            "TABLE",
                            "JOIN",
                            "FROM",
                            "REFERENCES",
                            "ON",
                            "VIEW",
                            "PROCEDURE",
                            "PROC",
                            "FUNCTION",
                            "EXEC",
                            "EXECUTE",
                            "MERGE",
                            "USING",
                            "AS",
                            "WITH",
                        ];
                        let call = prev_sig(ts, i).map_or(true, |q| {
                            ts[q].k != DOT && !OBJECT.iter().any(|w| ts[q].is(w))
                        }) && !next_sig(ts, r).is_some_and(|q| {
                            ts[q].is("AS") && next_sig(ts, q).is_some_and(|p| ts[p].k == L_PAREN)
                        });
                        if call {
                            self.unmapped_builtin(&t.s);
                        }
                    }
                }
                if up == "CURRENT_TIMESTAMP" {
                    let now = self.now();
                    self.step_ts("tsql.func.current_timestamp", &ts[i..=i]);
                    o.push_str(&now);
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
                        self.step_ts("tsql.expr.temp-table", &ts[i..=i]);
                        o.push_str("SESSION.");
                        o.push_str(rest);
                    } else {
                        self.note(Status::Assumptions, "ASM-TEMP-TABLE", "#temp tables become TEMP tables: they live until the session ends (T-SQL drops them when the creating procedure ends), so a second CREATE in the same session fails.");
                        self.step_ts("tsql.expr.temp-table", &ts[i..=i]);
                        o.push_str(rest);
                    }
                } else {
                    o.push_str(&t.s);
                }
            }
            _ => o.push_str(&t.s),
        }
        i + 1
    }

    /// A FROM table of a DELETE join; a table function gets Db2's TABLE().
    fn leaf_text(&mut self, leaf: &Leaf) -> String {
        let ts = trimmed(leaf.toks);
        if let Some(r) = self.missing_image(ts, 0) {
            self.step_ts("tsql.trigger.missing-image", &ts[..1]);
            return format!("{r}{}", self.expr(&ts[1..])).trim().to_string();
        }
        if self.db2() && !leaf.table && !leaf.name.is_empty() {
            if let Some((call, end)) = self.table_function_db2(ts, 0) {
                self.step_ts("tsql.from.table-function", &ts[..end]);
                return format!("{call}{}", self.expr(&ts[end..]))
                    .trim()
                    .to_string();
            }
        }
        self.expr(ts).trim().to_string()
    }

    /// Db2: a table-valued function in FROM, `f(args) [AS] x`, becomes
    /// `TABLE(f(args)) [AS] x`, the form IBM i documents for a table function
    /// reference. An unaliased call gets `AS f`, the name T-SQL exposes for
    /// it. T-SQL's own rowset built-ins (OPENJSON, STRING_SPLIT, …) and `sys`
    /// functions have no rule and are left as they are, with a note. Returns
    /// the text and the index after the call.
    fn table_function_db2(&mut self, ts: &[T], i: usize) -> Option<(String, usize)> {
        let mut last = i;
        while let Some(d) = next_sig(ts, last).filter(|&d| ts[d].k == DOT) {
            last = next_sig(ts, d).filter(|&n| matches!(ts[n].k, IDENT | QUOTED_IDENT))?;
        }
        let open = next_sig(ts, last).filter(|&p| ts[p].k == L_PAREN)?;
        let close = matching(ts, open)?;
        // `t (NOLOCK)`: an old-style table hint, not a call.
        let inner: Vec<&T> = ts[open + 1..close].iter().filter(|t| t.sig()).collect();
        if !inner.is_empty()
            && inner
                .iter()
                .all(|t| t.k == COMMA || t.k == IDENT && OLD_STYLE_HINTS.iter().any(|w| t.is(w)))
        {
            return None;
        }
        let part = |j: usize| match ts[j].k {
            QUOTED_IDENT => tsql_unquote(&ts[j].s),
            _ => ts[j].s.clone(),
        };
        let builtin = last == i && {
            use sqldiff_core::catalog::builtins::{self, Availability};
            builtins::of(Dialect::Tsql).function(&part(i), "9999") != Availability::Unknown
        };
        if builtin || last != i && part(i).eq_ignore_ascii_case("sys") {
            let text = format!(
                "{}() in FROM is a T-SQL {} table function; no Db2 for i rule yet.",
                part(last).to_ascii_uppercase(),
                if builtin { "built-in" } else { "system" }
            );
            self.note(Status::NotConverted, "tsql.table-builtin", &text);
            return None;
        }
        let call = self.expr(&ts[i..=close]).trim().to_string();
        let aliased = next_sig(ts, close).is_some_and(|k| match ts[k].k {
            QUOTED_IDENT => true,
            IDENT => !NOT_AN_ALIAS.iter().any(|w| ts[k].is(w)),
            _ => false,
        });
        let mut text = format!("TABLE({call})");
        if !aliased {
            text.push_str(" AS ");
            text.push_str(self.expr(&ts[last..=last]).trim());
        }
        Some((text, close + 1))
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
            self.step_ts("tsql.expr.string-compare", &ts[i..=i]);
            self.ci_note();
        }
    }

    /// A date and time literal with a seventh fractional digit: compared with
    /// or converted to a date and time, PostgreSQL rounds it to microseconds
    /// (checked on PostgreSQL 18: '9999-12-31 23:59:59.9999999' becomes
    /// 10000-01-01 00:00:00), where T-SQL keeps it.
    fn fraction_note(&mut self, t: &T, lit: &str) {
        if self.db2() {
            return;
        }
        let Some(body) = lit.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) else {
            return;
        };
        if sqldiff_core::equiv::literal::fraction_digits(body).is_some_and(|n| n > 6) {
            self.step_ts("tsql.expr.literal-fraction", std::slice::from_ref(t));
            self.note(Status::Assumptions, "ASM-LITERAL-FRACTION", "A date and time literal with 7 fractional digits: where it meets a date and time value, T-SQL keeps the 100 ns digit and PostgreSQL rounds it to microseconds ('9999-12-31 23:59:59.9999999' becomes 10000-01-01 00:00:00). Equal only if no stored value lies within half a microsecond of the literal without being equal to it (data moved with the same rounding).");
        }
    }

    fn ci_note(&mut self) {
        let text = self.pick(
            "String comparison: T-SQL's default collation is case-insensitive, and T-SQL ignores trailing spaces when it compares; PostgreSQL compares case-sensitively and counts trailing spaces in varchar and text. Equal only if the data's case is consistent (or use a case-insensitive collation) and no compared value differs from another only by trailing spaces.",
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
                let x = e(self, 0);
                // ISNULL converts the fallback to the first argument's type.
                let y = if self.is_bit(a[0]) {
                    self.bit_store_text(a[1])
                } else {
                    e(self, 1)
                };
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
            ("DB_NAME", 0) => "current_database()".into(),
            // Code points 1 to 127 are the same character in every code page and in
            // Unicode. 0 (`chr(0)` is an error), 128 to 255 (the database's code
            // page decides) and a value only known at run time stay unconverted.
            ("CHAR", 1) => {
                let text: String = a[0]
                    .iter()
                    .filter(|t| t.sig())
                    .map(|t| t.s.as_str())
                    .collect();
                match text.parse::<u8>() {
                    Ok(code @ 1..=127) => format!("chr({code})"),
                    _ => return None,
                }
            }
            ("QUOTENAME", 1) => {
                self.note(Status::Assumptions, "ASM-QUOTENAME-IDENT", "QUOTENAME becomes quote_ident: both make the value usable as one identifier, but quote_ident adds double quotes only when needed and QUOTENAME returns NULL for more than 128 characters. Equal only where the result is used as a PostgreSQL identifier, never as text.");
                format!("quote_ident({})", e(self, 0))
            }
            ("RAND", 0) => {
                self.note(Status::Assumptions, "ASM-RAND-PER-QUERY", "RAND() becomes random(): T-SQL evaluates RAND() once per query, random() once per row. Equal where the statement produces a single value (an assignment or a query returning one row).");
                "random()".into()
            }
            ("DATEFROMPARTS", 3) => {
                let (y, m, d) = (e(self, 0), e(self, 1), e(self, 2));
                format!("make_date({y}, {m}, {d})")
            }
            ("IIF", 3) => {
                let bits = self.bit_results(&[a[1], a[2]]);
                let r = |cx: &mut Self, k: usize| match bits[k - 1] {
                    Some(b) => b.to_string(),
                    None => e(cx, k),
                };
                let (c, x, y) = (e(self, 0), r(self, 1), r(self, 2));
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
                return self.cast_pg(&a[0][..at], &a[0][at + 1..]);
            }
            ("CONVERT", 2) => return self.cast_pg(a[1], a[0]),
            ("CONVERT", 3) => {
                self.note(Status::NotConverted, "tsql.convert-style", "CONVERT with a style code: use to_char / to_timestamp with the matching format.");
                return None;
            }
            ("DATEADD", 3) => return self.dateadd(&a),
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
                let x = e(self, 0);
                // ISNULL converts the fallback to the first argument's type.
                let y = if self.is_bit(a[0]) {
                    self.bit_store_text(a[1])
                } else {
                    e(self, 1)
                };
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
            ("QUOTENAME", 1) => {
                self.note(Status::Assumptions, "ASM-QUOTENAME-IDENT", "QUOTENAME becomes a double-quoted identifier with embedded quotes doubled; QUOTENAME returns NULL for more than 128 characters. Equal only where the result is used as a Db2 for i identifier, never as text.");
                let x = e(self, 0);
                format!("('\"' || REPLACE({x}, '\"', '\"\"') || '\"')")
            }
            ("RAND", 0) => {
                self.note(Status::Assumptions, "ASM-RAND-PER-QUERY", "RAND() stays RAND(): T-SQL evaluates RAND() once per query, Db2 once per row. Equal where the statement produces a single value (an assignment or a query returning one row).");
                "RAND()".into()
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
            ("DATEADD", 3) => return self.dateadd(&a),
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

    /// PostgreSQL: `NEXT VALUE FOR seq` at `ts[i]` → `nextval('seq')`, and
    /// the index of the name's last token.
    fn next_value(&mut self, ts: &[T], i: usize) -> Option<(String, usize)> {
        let v = next_sig(ts, i).filter(|&j| ts[j].is("VALUE"))?;
        let f = next_sig(ts, v).filter(|&j| ts[j].is("FOR"))?;
        let first = next_sig(ts, f)?;
        let mut end = first;
        while let Some(d) = next_sig(ts, end).filter(|&d| ts[d].k == DOT) {
            end = next_sig(ts, d)?;
        }
        let name = self.expr(&ts[first..=end]).trim().to_string();
        if ts.iter().filter(|t| t.is("NEXT")).count() > 1 {
            self.note(Status::Assumptions, "ASM-NEXT-VALUE-ONCE", "T-SQL gives every NEXT VALUE FOR of one sequence in a row the same value; PostgreSQL's nextval() advances on each call. Equal unless one statement asks for the same sequence twice.");
        }
        Some((format!("nextval('{}')", name.replace('\'', "''")), end))
    }

    /// A call kept as written must exist in the target: a T-SQL built-in the
    /// target does not have refuses the unit instead of passing through.
    fn unmapped_builtin(&mut self, name: &str) {
        use sqldiff_core::catalog::builtins::{self, Availability};
        let tsql = builtins::of(Dialect::Tsql).function(name, "9999") != Availability::Unknown;
        let target = builtins::of(self.to).function(name, "9999") != Availability::Unknown;
        if tsql && !target {
            self.note(
                Status::NotConverted,
                "tsql.builtin",
                &format!(
                    "{}() is a T-SQL built-in that {} does not have; no rule yet.",
                    name.to_ascii_uppercase(),
                    self.to.display_name()
                ),
            );
        }
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

    /// Maps a T-SQL type to the target: (type, is string, is numeric). The
    /// mapping is one step, `tsql.type.<T-SQL type>` (`tsql.type.user-defined`
    /// for a type kept as written), with the target type.
    fn ty(&mut self, ts: &[T]) -> R<(String, bool, bool)> {
        let r = self.ty_map(ts)?;
        let t = trimmed(ts);
        let named = match t {
            [s, d, n, ..] if d.k == DOT && tsql_unquote(&s.s).eq_ignore_ascii_case("sys") => n,
            [n, ..] => n,
            [] => return Ok(r),
        };
        let up = tsql_unquote(&named.s).to_ascii_uppercase();
        let rule = if KNOWN_TYPES.contains(&up.as_str()) {
            format!("tsql.type.{}", up.to_ascii_lowercase())
        } else {
            "tsql.type.user-defined".to_string()
        };
        self.step_ts_with(&rule, t, r.0.clone());
        Ok(r)
    }

    /// Maps a T-SQL type to PostgreSQL: (type, is string, is numeric).
    fn ty_map(&mut self, ts: &[T]) -> R<(String, bool, bool)> {
        let ts = trimmed(ts);
        let Some(first) = ts.first() else {
            return no("missing type");
        };
        if let Some(d) = next_sig(ts, 0).filter(|&d| ts[d].k == DOT) {
            // `sys.geography` is the built-in type; any other schema holds a
            // user-defined type, which has no rule.
            if tsql_unquote(&first.s).eq_ignore_ascii_case("sys") && d + 1 < ts.len() {
                return self.ty_map(&ts[d + 1..]);
            }
            return no("user-defined type (schema-qualified): no rule yet");
        }
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
                self.note(Status::Assumptions, "ASM-BIT-SMALLINT", BIT_SMALLINT);
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

/// The T-SQL types `ty` maps by name (upper case); any other name is a
/// user-defined type, kept as written.
const KNOWN_TYPES: &[&str] = &[
    "INT",
    "INTEGER",
    "BIGINT",
    "SMALLINT",
    "TINYINT",
    "BIT",
    "DECIMAL",
    "NUMERIC",
    "DEC",
    "MONEY",
    "SMALLMONEY",
    "FLOAT",
    "REAL",
    "DATETIME",
    "DATETIME2",
    "SMALLDATETIME",
    "DATE",
    "TIME",
    "DATETIMEOFFSET",
    "CHAR",
    "NCHAR",
    "VARCHAR",
    "NVARCHAR",
    "TEXT",
    "NTEXT",
    "SYSNAME",
    "VARBINARY",
    "BINARY",
    "IMAGE",
    "UNIQUEIDENTIFIER",
    "XML",
];

/// Whether a batch-level unit needs PL/pgSQL (variables or control flow).
/// Note text for a variable used without a declaration.
const UNDECLARED: &str =
    "A variable is used without a declaration in this unit; it was renamed with the v_ prefix.";

/// A trace of one step, for a unit one rule wrote alone.
fn one_step(rule: &str, (start, end): (usize, usize)) -> Vec<crate::Step> {
    vec![crate::Step {
        rule: rule.into(),
        start,
        end,
        inst: None,
    }]
}

fn undeclared_name(v: &str) -> String {
    format!("v_{}", snake(v.trim_start_matches('@')))
}

/// `IF @@TRANCOUNT > 0 ROLLBACK|COMMIT [TRAN]`: the statement it guards.
fn trancount_guard(n: &SyntaxNode) -> Option<&'static str> {
    let words: Vec<String> = sqldiff_syntax::ast::sig_tokens(n)
        .map(|t| t.text().to_ascii_uppercase())
        .filter(|w| !matches!(w.as_str(), ";" | "TRAN" | "TRANSACTION" | "WORK"))
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["IF", "@@TRANCOUNT", ">", "0", "ROLLBACK"] => Some("ROLLBACK"),
        ["IF", "@@TRANCOUNT", ">", "0", "COMMIT"] => Some("COMMIT"),
        _ => None,
    }
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
/// The FROM clause `dummy_from` adds; seen as a FROM when it runs again.
const DUMMY_FROM: &str = " FROM SYSIBM.SYSDUMMY1";

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
            if ts[j].is("FROM") || ts[j].is("INTO") || ts[j].s == DUMMY_FROM {
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
            // Copied as is by the expression walk.
            out.push(T::made(
                NUMBER,
                DUMMY_FROM,
                t.pos.0,
                "tsql.select.dummy-from",
            ));
        }
        out.push(t.clone());
    }
    if at.contains(&ts.len()) {
        let end = ts.last().map_or(0, |t| t.pos.1);
        out.push(T::made(NUMBER, DUMMY_FROM, end, "tsql.select.dummy-from"));
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
    }) || {
        // `EXEC p @name = 'x'` names a parameter; only a variable as a value
        // needs a block.
        let exec = first.is_some_and(|t| t.is("EXEC") || t.is("EXECUTE"));
        ts.iter().enumerate().any(|(i, t)| {
            t.k == VARIABLE
                && !t.s.starts_with("@@")
                && !(exec && next_sig(&ts, i).is_some_and(|j| ts[j].k == OP && ts[j].s == "="))
        })
    }
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

/// One table of a DELETE's FROM list.
struct Leaf<'a> {
    toks: &'a [T],
    /// Name parts, unquoted and lowercased.
    name: Vec<String>,
    alias: Option<String>,
    /// A named table (not a derived table or a function).
    table: bool,
}

impl Leaf<'_> {
    /// Whether the DELETE target `t` names this table: by its alias, or by
    /// a one-part name equal to the last part of an unaliased table.
    fn binds(&self, t: &[String]) -> bool {
        match (&self.alias, t) {
            (Some(a), [x]) => a == x,
            (Some(_), _) => false,
            (None, [x]) => self.table && self.name.last() == Some(x),
            (None, _) => false,
        }
    }
}

/// Whether the T-SQL UPDATE target `t` (one or two name parts) is the FROM
/// table `l`. SQL Server 2022 binds the target to the FROM reference of the
/// same table, aliased or not (`UPDATE dbo.T … FROM dbo.T AS x` and
/// `UPDATE T … FROM dbo.T AS x` change the rows `x` keeps), and a one-part
/// target also to an alias or an unaliased table's last name part. Names
/// whose schema only one side states could be the same table or not (the
/// default schema decides), and a two-part target next to an alias equal to
/// its name has no checked binding: both are errors.
fn update_binds(l: &Leaf, t: &[String]) -> Result<bool, String> {
    if let ([x], Some(a)) = (t, &l.alias) {
        if a == x {
            return Ok(true);
        }
    }
    if let ([_, x], Some(a)) = (t, &l.alias) {
        if a == x {
            return Err(format!(
                "the alias `{a}` of another FROM item has the target's name"
            ));
        }
    }
    if !l.table || l.name.last() != t.last() {
        return Ok(false);
    }
    if l.name == t || (t.len() == 1 && l.alias.is_none()) {
        return Ok(true);
    }
    if l.name.len() > 2 || l.name.len() == t.len() {
        // Another schema, or a database name: another table, or one whose
        // binding was not checked.
        return if l.name.len() == t.len() {
            Ok(false)
        } else {
            Err("a FROM table with a three- or four-part name".into())
        };
    }
    Err(format!(
        "the target `{}` and the FROM table `{}` may or may not be the same table (only one states a schema)",
        t.join("."),
        l.name.join(".")
    ))
}

/// How a table joins its chain in an UPDATE's FROM list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Join {
    /// The first table of a comma item.
    First,
    Inner,
    Left,
    Cross,
}

/// One table of an UPDATE's FROM list, with the join that adds it.
struct Step<'a> {
    kind: Join,
    leaf: Leaf<'a>,
    on: Option<&'a [T]>,
}

/// Splits one comma item of a FROM list, a chain of inner, LEFT and cross
/// joins, into its tables. Anything else is an error.
fn join_steps(ts: &[T]) -> Result<Vec<Step<'_>>, String> {
    let ds = crate::tok::depths(ts);
    let s: Vec<usize> = sig(ts).into_iter().filter(|&i| ds[i] == 0).collect();
    let mut steps = Vec::new();
    let (mut start, mut kind, mut on): (usize, Join, Option<usize>) = (0, Join::First, None);
    let mut close = |a: usize, b: usize, kind: Join, on: Option<usize>| -> Result<(), String> {
        let needs_on = matches!(kind, Join::Inner | Join::Left);
        match (on, needs_on) {
            (Some(o), true) => {
                if find_word(&ts[o + 1..b], 0, "ON").is_some() {
                    return Err("nested ON clauses".into());
                }
                steps.push(Step {
                    kind,
                    leaf: leaf(&ts[a..o])?,
                    on: Some(&ts[o + 1..b]),
                });
            }
            (None, false) => steps.push(Step {
                kind,
                leaf: leaf(&ts[a..b])?,
                on: None,
            }),
            (None, true) => return Err("a JOIN without its ON".into()),
            (Some(_), false) => return Err("ON after a cross join or a comma".into()),
        }
        Ok(())
    };
    let mut k = 0;
    while k < s.len() {
        let i = s[k];
        let t = &ts[i];
        let at = |j: usize, w: &str| s.get(j).is_some_and(|&x| ts[x].is(w));
        let (next, width) = if t.is("JOIN") {
            (Join::Inner, 1)
        } else if t.is("INNER") && at(k + 1, "JOIN") {
            (Join::Inner, 2)
        } else if t.is("CROSS") && at(k + 1, "JOIN") {
            (Join::Cross, 2)
        } else if t.is("LEFT") && at(k + 1, "JOIN") {
            (Join::Left, 2)
        } else if t.is("LEFT") && at(k + 1, "OUTER") && at(k + 2, "JOIN") {
            (Join::Left, 3)
        } else if [
            "LEFT", "RIGHT", "FULL", "OUTER", "INNER", "CROSS", "APPLY", "LOOP", "HASH", "MERGE",
            "REMOTE",
        ]
        .iter()
        .any(|w| t.is(w))
            && !next_sig(ts, i).is_some_and(|j| ts[j].k == L_PAREN)
        {
            return Err("a RIGHT or FULL join, APPLY or a join hint".into());
        } else {
            if t.is("ON") && on.is_none() {
                on = Some(i);
            }
            k += 1;
            continue;
        };
        close(start, i, kind, on)?;
        start = s[k + width - 1] + 1;
        on = None;
        kind = next;
        k += width;
    }
    close(start, ts.len(), kind, on)?;
    Ok(steps)
}

/// A table written with old-style hints, `t (NOLOCK)`.
fn hinted(ts: &[T]) -> bool {
    let s = sig(ts);
    s.windows(2).any(|w| {
        ts[w[1]].k == L_PAREN
            && ts[w[0]].k != DOT
            && next_sig(ts, w[1]).is_some_and(|j| OLD_STYLE_HINTS.iter().any(|h| ts[j].is(h)))
    })
}

/// Whether a condition names `alias.` (or a subquery, which might).
fn qualifies(ts: &[T], alias: &str) -> bool {
    sig(ts).into_iter().any(|i| {
        let t = &ts[i];
        t.is("SELECT")
            || matches!(t.k, IDENT | QUOTED_IDENT)
                && tsql_unquote(&t.s).eq_ignore_ascii_case(alias)
                && next_sig(ts, i).is_some_and(|j| ts[j].k == DOT)
    })
}

/// The equalities among the top-level AND terms of a condition, each side
/// as a two-part name `[table, column]` (lower case), or empty for a
/// variable or a literal. Other terms are left out.
fn equalities(ts: &[T]) -> Vec<(Vec<String>, Vec<String>)> {
    let ds = crate::tok::depths(ts);
    let mut terms = Vec::new();
    let mut start = 0;
    for i in 0..ts.len() {
        if ds[i] == 0 && (ts[i].is("AND") || ts[i].is("OR")) {
            if ts[i].is("OR") {
                return Vec::new();
            }
            terms.push(&ts[start..i]);
            start = i + 1;
        }
    }
    terms.push(&ts[start..]);
    let side = |x: &[T]| -> Option<Vec<String>> {
        let x = trimmed(x);
        match x {
            [t] if matches!(t.k, VARIABLE | NUMBER | STRING) => Some(Vec::new()),
            // `schema.table.column` names an unaliased table, whose key is
            // its last name part (T-SQL rejects two unaliased FROM tables
            // with one last part, Msg 1013).
            _ => name_parts(x)
                .filter(|p| p.len() == 2 || p.len() == 3)
                .map(|p| p[p.len() - 2..].to_vec()),
        }
    };
    let mut out = Vec::new();
    for t in terms {
        let t = trimmed(t);
        let ops: Vec<usize> = (0..t.len()).filter(|&i| t[i].k == OP).collect();
        let [op] = ops[..] else { continue };
        if t[op].s != "=" {
            continue;
        }
        if let (Some(l), Some(r)) = (side(&t[..op]), side(&t[op + 1..])) {
            if !(l.is_empty() && r.is_empty()) {
                out.push((l, r));
            }
        }
    }
    out
}

/// Unqualified column names in a condition: words that are not keywords,
/// not part of a dotted name, not a function name and not a type after AS.
fn bare_columns(ts: &[T]) -> Vec<String> {
    let mut out = Vec::new();
    for i in sig(ts) {
        let t = &ts[i];
        let quoted = t.k == QUOTED_IDENT;
        if !(t.k == IDENT || quoted) || !quoted && sqldiff_core::normalize::is_keyword(&t.up()) {
            continue;
        }
        let prev = prev_sig(ts, i).map(|j| &ts[j]);
        let next = next_sig(ts, i).map(|j| &ts[j]);
        if prev.is_some_and(|p| p.k == DOT || p.is("AS"))
            || next.is_some_and(|n| n.k == DOT || n.k == L_PAREN)
        {
            continue;
        }
        out.push(tsql_unquote(&t.s).to_ascii_lowercase());
    }
    out
}

/// Unquoted, lowercased parts of a dotted name; `None` if the tokens are
/// anything else.
fn name_parts(ts: &[T]) -> Option<Vec<String>> {
    let s = sig(ts);
    let mut parts = Vec::new();
    for (n, &i) in s.iter().enumerate() {
        let t = &ts[i];
        match (n % 2, t.k) {
            (0, IDENT) if !t.s.starts_with('@') => parts.push(t.s.to_ascii_lowercase()),
            (0, QUOTED_IDENT) => parts.push(tsql_unquote(&t.s).to_ascii_lowercase()),
            (1, DOT) => {}
            _ => return None,
        }
    }
    (!parts.is_empty() && s.len() % 2 == 1).then_some(parts)
}

/// Splits one comma item of a FROM list, a chain of inner and cross joins,
/// into its tables and ON conditions. Anything else is an error.
fn join_chain<'a>(
    ts: &'a [T],
    leaves: &mut Vec<Leaf<'a>>,
    conds: &mut Vec<&'a [T]>,
) -> Result<(), String> {
    let ds = crate::tok::depths(ts);
    let s: Vec<usize> = sig(ts).into_iter().filter(|&i| ds[i] == 0).collect();
    // (start, end) of each table, and of each ON condition.
    let mut start = 0;
    let mut on: Option<usize> = None;
    let mut need_on = false;
    let mut close = |a: usize, b: usize, on: Option<usize>, need_on: bool| -> Result<(), String> {
        match (on, need_on) {
            (Some(o), true) => {
                if find_word(&ts[o + 1..b], 0, "ON").is_some() {
                    return Err("nested ON clauses".into());
                }
                leaves.push(leaf(&ts[a..o])?);
                conds.push(&ts[o + 1..b]);
            }
            (None, false) => leaves.push(leaf(&ts[a..b])?),
            (None, true) => return Err("a JOIN without its ON".into()),
            (Some(_), false) => return Err("ON after a cross join or a comma".into()),
        }
        Ok(())
    };
    let mut k = 0;
    while k < s.len() {
        let i = s[k];
        let t = &ts[i];
        let word = |j: usize| s.get(j).is_some_and(|&x| ts[x].is("JOIN"));
        let (kind_on, width) = if t.is("JOIN") {
            (true, 1)
        } else if t.is("INNER") && word(k + 1) {
            (true, 2)
        } else if t.is("CROSS") && word(k + 1) {
            (false, 2)
        } else if ["LEFT", "RIGHT", "FULL", "OUTER", "INNER", "CROSS", "APPLY"]
            .iter()
            .any(|w| t.is(w))
            // LEFT(…) and RIGHT(…) are string functions.
            && !next_sig(ts, i).is_some_and(|j| ts[j].k == L_PAREN)
        {
            return Err("an outer join, APPLY or a join hint".into());
        } else {
            if t.is("ON") && on.is_none() {
                on = Some(i);
            }
            k += 1;
            continue;
        };
        close(start, i, on, need_on)?;
        start = s[k + width - 1] + 1;
        on = None;
        need_on = kind_on;
        k += width;
    }
    close(start, ts.len(), on, need_on)
}

/// One FROM table: `name [[AS] alias] [WITH (hints)]` or a derived table
/// `(SELECT …) [AS] alias`.
fn leaf(ts: &[T]) -> Result<Leaf<'_>, String> {
    let bad =
        || Err("a FROM item that is not a table, a derived table or a table function".to_string());
    let s = sig(ts);
    let Some(&first) = s.first() else {
        return bad();
    };
    // Where the name (or the parenthesised query) ends.
    let (name, table, mut k) = if ts[first].k == L_PAREN {
        let close = matching(ts, first).ok_or("unbalanced parentheses")?;
        if !next_sig(ts, first).is_some_and(|j| ts[j].is("SELECT")) {
            return bad();
        }
        (
            Vec::new(),
            false,
            s.iter().position(|&i| i > close).unwrap_or(s.len()),
        )
    } else {
        let mut k = 1;
        while k + 1 < s.len() && ts[s[k]].k == DOT {
            k += 2;
        }
        let Some(name) = name_parts(&ts[first..s[k - 1] + 1]) else {
            return bad();
        };
        if s.get(k).is_some_and(|&i| ts[i].k == L_PAREN) {
            // A table-valued function call.
            let close = matching(ts, s[k]).ok_or("unbalanced parentheses")?;
            (
                name,
                false,
                s.iter().position(|&i| i > close).unwrap_or(s.len()),
            )
        } else {
            (name, true, k)
        }
    };
    if s.get(k).is_some_and(|&i| ts[i].is("AS")) {
        k += 1;
    }
    let mut alias = None;
    if let Some(&i) = s.get(k) {
        match ts[i].k {
            IDENT if !ts[i].is("WITH") => alias = Some(ts[i].s.to_ascii_lowercase()),
            QUOTED_IDENT => alias = Some(tsql_unquote(&ts[i].s).to_ascii_lowercase()),
            _ => {}
        }
        if alias.is_some() {
            k += 1;
        }
    }
    if table && s.get(k).is_some_and(|&i| ts[i].is("WITH")) {
        let open = s
            .get(k + 1)
            .filter(|&&i| ts[i].k == L_PAREN)
            .ok_or("WITH without hints")?;
        let close = matching(ts, *open).ok_or("unbalanced parentheses")?;
        k = s.iter().position(|&i| i > close).unwrap_or(s.len());
    }
    if k != s.len() || (!table && alias.is_none() && ts[first].k == L_PAREN) {
        return bad();
    }
    Ok(Leaf {
        toks: ts,
        name,
        alias,
        table,
    })
}

/// The CTE names of a statement that starts with WITH (unquoted,
/// lowercased), and the index of the statement word after the list.
fn cte_list(ts: &[T]) -> Option<(Vec<String>, usize)> {
    cte_list_rec(ts).map(|(n, v, ..)| (n, v))
}

/// `cte_list`, whether a CTE names itself in its own query (recursion; a
/// column of the same name counts too, which only costs a note), and
/// whether a CTE's query names a CTE defined after it (qualified or not,
/// again counting columns).
fn cte_list_rec(ts: &[T]) -> Option<(Vec<String>, usize, bool, bool)> {
    let mut recursive = false;
    let mut bodies = Vec::new();
    let s = sig(ts);
    let ds = crate::tok::depths(ts);
    let s: Vec<usize> = s.into_iter().filter(|&i| ds[i] == 0).collect();
    if !ts[*s.first()?].is("WITH") {
        return None;
    }
    let mut names = Vec::new();
    let mut k = 1;
    loop {
        let name = *s.get(k)?;
        names.push(match ts[name].k {
            IDENT => ts[name].s.to_ascii_lowercase(),
            QUOTED_IDENT => tsql_unquote(&ts[name].s).to_ascii_lowercase(),
            _ => return None,
        });
        k += 1;
        if ts[*s.get(k)?].k == L_PAREN {
            k += 2; // the column list, `(` and `)` at depth 0
        }
        if !ts[*s.get(k)?].is("AS") || ts[*s.get(k + 1)?].k != L_PAREN {
            return None;
        }
        let (open, close) = (*s.get(k + 1)?, *s.get(k + 2)?);
        bodies.push(open..close);
        if names_in(&ts[open..close], names.last()?) {
            recursive = true;
        }
        k += 3;
        let next = *s.get(k)?;
        if ts[next].k != COMMA {
            let forward = bodies
                .iter()
                .enumerate()
                .any(|(i, b)| names[i + 1..].iter().any(|n| names_in(&ts[b.clone()], n)));
            return Some((names, next, recursive, forward));
        }
        k += 1;
    }
}

/// Whether a word of `ts` is `name` (unquoted, case-insensitively).
fn names_in(ts: &[T], name: &str) -> bool {
    ts.iter().any(|t| match t.k {
        IDENT => t.s.eq_ignore_ascii_case(name),
        QUOTED_IDENT => tsql_unquote(&t.s).eq_ignore_ascii_case(name),
        _ => false,
    })
}

/// The last part of the dotted name that starts at `i`, unquoted and
/// lowercased.
fn name_last(ts: &[T], i: usize) -> Option<String> {
    let mut last = i;
    while let Some(d) = next_sig(ts, last).filter(|&d| ts[d].k == DOT) {
        last = next_sig(ts, d)?;
    }
    match ts[last].k {
        IDENT => Some(ts[last].s.to_ascii_lowercase()),
        QUOTED_IDENT => Some(tsql_unquote(&ts[last].s).to_ascii_lowercase()),
        _ => None,
    }
}

/// Table hints T-SQL also takes without WITH: `FROM t (NOLOCK)`.
const OLD_STYLE_HINTS: &[&str] = &[
    "NOLOCK",
    "READUNCOMMITTED",
    "READCOMMITTED",
    "READCOMMITTEDLOCK",
    "NOWAIT",
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
    "NOEXPAND",
    "FORCESEEK",
    "FORCESCAN",
];

/// Words that can follow a FROM item without being its alias.
const NOT_AN_ALIAS: &[&str] = &[
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "OUTER",
    "ON",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "OPTION",
    "FOR",
    "WINDOW",
    "PIVOT",
    "UNPIVOT",
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "SET",
    "IF",
    "ELSE",
    "END",
    "BEGIN",
    "RETURN",
    "DECLARE",
    "EXEC",
    "EXECUTE",
    "WITH",
    "WHILE",
    "COMMIT",
    "ROLLBACK",
    "PRINT",
    "THROW",
    "TRUNCATE",
    "CREATE",
    "ALTER",
    "DROP",
    "OUTPUT",
    "INTO",
];

/// Whether the name at `i` begins an item of a FROM list: it follows FROM
/// (not `DELETE FROM`, whose target is not a FROM item), JOIN, or a comma
/// of a FROM list at the same parenthesis depth.
fn from_item_start(ts: &[T], i: usize) -> bool {
    let Some(p) = prev_sig(ts, i) else {
        return false;
    };
    if ts[p].is("JOIN") {
        return true;
    }
    let ds = crate::tok::depths(ts);
    // A query's FROM: not `DELETE FROM`, nor `TRIM(' ' FROM s)` inside a call.
    let query_from = |f: usize| {
        !prev_sig(ts, f).is_some_and(|q| ts[q].is("DELETE"))
            && (ds[f] == 0
                || (0..f)
                    .rev()
                    .find(|&j| ts[j].k == L_PAREN && ds[j] == ds[f] - 1)
                    .and_then(|o| next_sig(ts, o))
                    .is_some_and(|n| ts[n].is("SELECT")))
    };
    if ts[p].is("FROM") {
        return query_from(p);
    }
    if ts[p].k != COMMA {
        return false;
    }
    for j in (0..p).rev() {
        if !ts[j].sig() || ds[j] > ds[p] {
            continue;
        }
        if ds[j] < ds[p] {
            return false;
        }
        if ts[j].is("FROM") {
            return query_from(j);
        }
        const ENDS: [&str; 17] = [
            "SELECT",
            "WHERE",
            "GROUP",
            "HAVING",
            "ORDER",
            "SET",
            "VALUES",
            "INTO",
            "UPDATE",
            "INSERT",
            "DELETE",
            "MERGE",
            "UNION",
            "EXCEPT",
            "INTERSECT",
            "OUTPUT",
            "BY",
        ];
        if ENDS.iter().any(|w| ts[j].is(w)) {
            return false;
        }
    }
    false
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

/// A T-SQL built-in whose result is always a string, so `+` next to a call
/// to it is concatenation.
fn string_builtin(ts: &[T], i: usize) -> bool {
    let qualified = prev_sig(ts, i).is_some_and(|d| ts[d].k == DOT);
    !qualified && ["CHAR", "NCHAR", "DB_NAME"].iter().any(|w| ts[i].is(w))
}

/// A date-time text whose fractional seconds PostgreSQL's microseconds
/// round into the year 10000: `9999-12-31 23:59:59.9999995` and above.
fn rounds_past_year_9999(v: &str) -> bool {
    let v = v.trim();
    let Some(frac) = ["9999-12-31 23:59:59.", "9999-12-31T23:59:59."]
        .iter()
        .find_map(|p| v.strip_prefix(p))
    else {
        return false;
    };
    frac.len() > 6
        && frac.chars().all(|c| c.is_ascii_digit())
        && frac.starts_with("999999")
        && frac.as_bytes()[6] >= b'5'
}

/// A CREATE TABLE item that is a table constraint, not a column.
fn constraint_item(first: &T) -> bool {
    ["CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK"]
        .iter()
        .any(|w| first.is(w))
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

/// One `WITH (…)` option of a table or index. Only options that change
/// performance, never results, may be dropped.
/// `CREATE INDEX name ON s.t`: the name gets the table's schema `s`,
/// unless that is dbo (dropped) or the name is qualified already.
fn qualify_index(ts: Vec<T>) -> Vec<T> {
    let Some(ix) = find_word(&ts, 0, "INDEX") else {
        return ts;
    };
    let Some(name) = next_sig(&ts, ix) else {
        return ts;
    };
    let Some(on) = next_sig(&ts, name).filter(|&j| ts[j].is("ON")) else {
        return ts;
    };
    let (Some(schema), Some(dot)) = (
        next_sig(&ts, on),
        next_sig(&ts, on).and_then(|j| next_sig(&ts, j)),
    ) else {
        return ts;
    };
    if ts[dot].k != DOT
        || next_sig(&ts, dot)
            .and_then(|j| next_sig(&ts, j))
            .is_some_and(|j| ts[j].k == DOT)
        || tsql_unquote(&ts[schema].s).eq_ignore_ascii_case("dbo")
    {
        return ts;
    }
    let mut out = ts[..name].to_vec();
    out.push(ts[schema].clone());
    out.push(ts[dot].clone());
    out.extend(ts[name..].iter().cloned());
    out
}

/// `NULLS NOT DISTINCT` put in a PostgreSQL unique index: before its
/// filter, or at its end.
fn nulls_not_distinct(ts: Vec<T>) -> Vec<T> {
    let at = find_word(&ts, 0, "WHERE").unwrap_or_else(|| {
        let last = (0..ts.len())
            .rev()
            .find(|&j| ts[j].sig() && ts[j].k != SEMICOLON);
        last.map_or(ts.len(), |j| j + 1)
    });
    let mut out = ts[..at].to_vec();
    let pos = ts
        .get(at)
        .map_or_else(|| ts.last().map_or(0, |t| t.pos.1), |t| t.pos.0);
    const RULE: &str = "tsql.index.nulls-not-distinct";
    let word = |s: &str| T::made(IDENT, s, pos, RULE);
    let space = || T::made(WHITESPACE, " ", pos, RULE);
    if !out.last().is_some_and(|t| t.k == WHITESPACE) {
        out.push(space());
    }
    for w in ["NULLS", "NOT", "DISTINCT"] {
        out.push(word(w));
        out.push(space());
    }
    if !ts[at..].iter().any(|t| t.sig() && t.k != SEMICOLON) {
        out.pop();
    }
    out.extend(ts[at..].iter().cloned());
    out
}

fn storage_option(opt: &[T]) -> R<()> {
    let opt = trimmed(opt);
    let Some(name) = opt.first().map(|t| t.up()) else {
        return Ok(());
    };
    let value = opt
        .iter()
        .rev()
        .find(|t| t.sig())
        .map(|t| t.up())
        .unwrap_or_default();
    let harmless = matches!(
        name.as_str(),
        "DATA_COMPRESSION"
            | "XML_COMPRESSION"
            | "FILLFACTOR"
            | "PAD_INDEX"
            | "STATISTICS_NORECOMPUTE"
            | "STATISTICS_INCREMENTAL"
            | "ALLOW_ROW_LOCKS"
            | "ALLOW_PAGE_LOCKS"
            | "SORT_IN_TEMPDB"
            | "ONLINE"
            | "RESUMABLE"
            | "MAXDOP"
            | "OPTIMIZE_FOR_SEQUENTIAL_KEY"
    ) || matches!(name.as_str(), "IGNORE_DUP_KEY" | "DROP_EXISTING")
        && value == "OFF";
    if harmless {
        Ok(())
    } else {
        no(format!(
            "table or index option {name}: it changes behaviour, not just storage; no rule yet"
        ))
    }
}

/// Variables run as dynamic SQL: every variable inside `EXEC (…)` /
/// `EXECUTE (…)`, and the statement argument of `sp_executesql`.
fn dynamic_sql_vars(ts: &[T]) -> Vec<String> {
    let s = sig(ts);
    let mut vars = Vec::new();
    for (k, &i) in s.iter().enumerate() {
        let next = |n: usize| s.get(k + n).map(|&j| &ts[j]);
        if (ts[i].is("EXEC") || ts[i].is("EXECUTE")) && next(1).is_some_and(|t| t.k == L_PAREN) {
            let open = s[k + 1];
            let close = matching(ts, open).unwrap_or(ts.len() - 1);
            vars.extend(
                ts[open..close]
                    .iter()
                    .filter(|t| t.k == VARIABLE)
                    .map(|t| t.s[1..].to_ascii_lowercase()),
            );
        }
        if tsql_unquote(&ts[i].s).eq_ignore_ascii_case("sp_executesql") {
            // First argument, positional or `@stmt = @v`.
            let (a, b, c) = (next(1), next(2), next(3));
            match (a, b, c) {
                (Some(x), Some(eq), Some(v))
                    if x.k == VARIABLE
                        && x.s.eq_ignore_ascii_case("@stmt")
                        && eq.s == "="
                        && v.k == VARIABLE =>
                {
                    vars.push(v.s[1..].to_ascii_lowercase())
                }
                (Some(v), _, _) if v.k == VARIABLE => vars.push(v.s[1..].to_ascii_lowercase()),
                _ => {}
            }
        }
    }
    vars.sort();
    vars.dedup();
    vars
}

/// The text of a T-SQL string literal (`N'it''s'` → `it's`).
fn string_value(s: &str) -> String {
    let s = s.strip_prefix(['N', 'n']).unwrap_or(s);
    s.strip_prefix('\'')
        .and_then(|x| x.strip_suffix('\''))
        .unwrap_or(s)
        .replace("''", "'")
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

/// A T-SQL type, as far as DATEADD needs it: the date argument decides the
/// result type, and the number must become an integer the way T-SQL makes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dt {
    Date,
    /// TIME(p), DATETIME2(p), DATETIMEOFFSET(p): p fractional digits (7 by default).
    Time(u32),
    DateTime,
    SmallDateTime,
    DateTime2(u32),
    Offset(u32),
    /// INT, SMALLINT, TINYINT, or an integer literal.
    Int,
    BigInt,
    /// DECIMAL, NUMERIC, FLOAT, REAL, or a literal with a fraction.
    Fraction,
    /// MONEY, SMALLMONEY: a fraction that T-SQL rounds (not truncates) to an integer.
    Money,
    /// A string (a literal, or a CHAR/VARCHAR value).
    Text,
    Bit,
    /// VARBINARY(n) or VARBINARY(MAX): `+` concatenates.
    VarBinary,
    Other,
}

impl Dt {
    /// From a type's base name (unquoted, any case) and its arguments.
    fn of(base: &str, args: &[u32]) -> Dt {
        let p = args.first().copied().unwrap_or(7);
        match base.to_ascii_uppercase().as_str() {
            "DATE" => Dt::Date,
            "TIME" => Dt::Time(p),
            "DATETIME" => Dt::DateTime,
            "SMALLDATETIME" => Dt::SmallDateTime,
            "DATETIME2" => Dt::DateTime2(p),
            "DATETIMEOFFSET" => Dt::Offset(p),
            "INT" | "INTEGER" | "SMALLINT" | "TINYINT" => Dt::Int,
            "BIGINT" => Dt::BigInt,
            "DECIMAL" | "NUMERIC" | "DEC" | "FLOAT" | "REAL" | "DOUBLE PRECISION" => Dt::Fraction,
            "MONEY" | "SMALLMONEY" => Dt::Money,
            "CHAR" | "NCHAR" | "VARCHAR" | "NVARCHAR" | "CHARACTER" | "CHARACTER VARYING"
            | "TEXT" | "NTEXT" | "SYSNAME" => Dt::Text,
            "BIT" => Dt::Bit,
            "VARBINARY" | "BINARY VARYING" => Dt::VarBinary,
            _ => Dt::Other,
        }
    }

    /// From the tokens of a type as written (`datetime2(3)`, `[date]`).
    fn of_tokens(ts: &[T]) -> Dt {
        let ts = trimmed(ts);
        let Some(first) = ts.first() else {
            return Dt::Other;
        };
        if next_sig(ts, 0).is_some_and(|d| ts[d].k == DOT) {
            return Dt::Other;
        }
        let args: Vec<u32> = match ts.iter().position(|t| t.k == L_PAREN) {
            Some(p) => split_commas(&ts[p + 1..matching(ts, p).unwrap_or(ts.len())])
                .iter()
                .map(|a| trimmed(a).iter().map(|t| t.s.as_str()).collect::<String>())
                .map(|a| a.parse().unwrap_or(u32::MAX))
                .collect(),
            None => Vec::new(),
        };
        Dt::of(&tsql_unquote(&first.s), &args)
    }

    fn is_date(self) -> bool {
        matches!(
            self,
            Dt::Date
                | Dt::Time(_)
                | Dt::DateTime
                | Dt::SmallDateTime
                | Dt::DateTime2(_)
                | Dt::Offset(_)
        )
    }

    /// The fractional digits a value of this type keeps, for the date types.
    fn scale(self) -> u32 {
        match self {
            Dt::Time(p) | Dt::DateTime2(p) | Dt::Offset(p) => p,
            Dt::DateTime => 3,
            _ => 0,
        }
    }

    fn name(self) -> String {
        match self {
            Dt::Date => "date".into(),
            Dt::Time(p) => format!("time({p})"),
            Dt::DateTime => "datetime".into(),
            Dt::SmallDateTime => "smalldatetime".into(),
            Dt::DateTime2(p) => format!("datetime2({p})"),
            Dt::Offset(p) => format!("datetimeoffset({p})"),
            _ => "a non-date type".into(),
        }
    }
}

/// A DATEADD datepart, aliases folded as T-SQL documents them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Year,
    Quarter,
    Month,
    /// day, dayofyear and weekday all add days.
    Day,
    Week,
    Hour,
    Minute,
    Second,
    Milli,
    Micro,
    Nano,
}

impl Unit {
    fn of(p: &str) -> Option<Unit> {
        Some(match p.trim().to_ascii_lowercase().as_str() {
            "year" | "yy" | "yyyy" => Unit::Year,
            "quarter" | "qq" | "q" => Unit::Quarter,
            "month" | "mm" | "m" => Unit::Month,
            "dayofyear" | "dy" | "y" | "day" | "dd" | "d" | "weekday" | "dw" | "w" => Unit::Day,
            "week" | "wk" | "ww" => Unit::Week,
            "hour" | "hh" => Unit::Hour,
            "minute" | "mi" | "n" => Unit::Minute,
            "second" | "ss" | "s" => Unit::Second,
            "millisecond" | "ms" => Unit::Milli,
            "microsecond" | "mcs" => Unit::Micro,
            "nanosecond" | "ns" => Unit::Nano,
            _ => return None,
        })
    }

    /// Year down to week: the parts that move the calendar date.
    fn calendar(self) -> bool {
        matches!(
            self,
            Unit::Year | Unit::Quarter | Unit::Month | Unit::Day | Unit::Week
        )
    }

    fn months(self) -> bool {
        matches!(self, Unit::Year | Unit::Quarter | Unit::Month)
    }
}

/// The tables a statement reads, for the types of its column references.
struct Scope {
    entries: Vec<ScopeEntry>,
}

struct ScopeEntry {
    /// Name parts, unquoted and lowercased.
    name: Vec<String>,
    alias: Option<String>,
    /// A plain name: not a derived table, function or table variable.
    named: bool,
}

/// The FROM, JOIN, UPDATE, MERGE and USING items of a statement, at any
/// depth. `None` when the statement has CTEs, whose names would hide tables.
fn scope_of(ts: &[T]) -> Option<Scope> {
    if cte_list(ts).is_some() {
        return None;
    }
    let s = sig(ts);
    let mut entries = Vec::new();
    let mut updates = Vec::new();
    let mut k = 0;
    while k < s.len() {
        let w = ts[s[k]].up();
        let starts = match w.as_str() {
            "FROM" | "JOIN" | "USING" | "APPLY" => true,
            "UPDATE" => {
                updates.push(entries.len());
                true
            }
            "MERGE" => {
                if s.get(k + 1).is_some_and(|&j| ts[j].is("INTO")) {
                    k += 1;
                }
                true
            }
            _ => false,
        };
        k += 1;
        if !starts {
            continue;
        }
        // One item, then more after commas.
        while let Some(&first) = s.get(k) {
            let mut name = Vec::new();
            let mut named = matches!(ts[first].k, IDENT | QUOTED_IDENT);
            if named {
                name.push(tsql_unquote(&ts[first].s).to_ascii_lowercase());
                k += 1;
                while s.get(k).is_some_and(|&j| ts[j].k == DOT) {
                    match s.get(k + 1) {
                        Some(&j) if matches!(ts[j].k, IDENT | QUOTED_IDENT) => {
                            name.push(tsql_unquote(&ts[j].s).to_ascii_lowercase());
                            k += 2;
                        }
                        _ => {
                            named = false;
                            k += 1;
                        }
                    }
                }
                if s.get(k).is_some_and(|&j| ts[j].k == L_PAREN) {
                    // A function call.
                    named = false;
                }
            }
            if !named {
                // A derived table, VALUES, a function or a table variable:
                // skip to its end.
                if let Some(&j) = s.get(k).filter(|&&j| ts[j].k == L_PAREN) {
                    let end = matching(ts, j).unwrap_or(ts.len() - 1);
                    k = s.partition_point(|&x| x <= end);
                } else if s.get(k).is_some_and(|&j| ts[j].k == VARIABLE) {
                    k += 1;
                }
            }
            if s.get(k).is_some_and(|&j| ts[j].is("AS")) {
                k += 1;
            }
            let alias = s.get(k).and_then(|&j| {
                let t = &ts[j];
                (t.k == QUOTED_IDENT
                    || t.k == IDENT && !sqldiff_core::normalize::is_keyword(&t.up()))
                .then(|| tsql_unquote(&t.s).to_ascii_lowercase())
            });
            if alias.is_some() {
                k += 1;
            }
            entries.push(ScopeEntry { name, alias, named });
            if s.get(k).is_some_and(|&j| ts[j].k == COMMA) && w != "UPDATE" {
                k += 1;
                continue;
            }
            break;
        }
    }
    // `UPDATE x SET … FROM t x`: the UPDATE item names an alias.
    for &u in updates.iter().rev() {
        let alias_of_other = entries.get(u).is_some_and(|e| {
            e.alias.is_none()
                && matches!(e.name.as_slice(), [n] if entries.iter().any(|o| o.alias.as_deref() == Some(n.as_str())))
        });
        if alias_of_other {
            entries.remove(u);
        }
    }
    Some(Scope { entries })
}

/// The type of a numeric literal as T-SQL reads it.
fn number_dt(s: &str) -> Dt {
    if s.len() >= 2 && s[..2].eq_ignore_ascii_case("0x") {
        return Dt::Other;
    }
    if s.contains(['.', 'e', 'E']) {
        return Dt::Fraction;
    }
    match s.parse::<i64>() {
        Ok(n) if i32::try_from(n).is_ok() => Dt::Int,
        _ => Dt::BigInt,
    }
}

/// Why DATEADD of `u` on a `dt` value has no exact rewrite, if it has none.
fn dateadd_refusal(dt: Dt, u: Unit, db2: bool) -> Option<String> {
    use Unit::*;
    let rejects = || {
        Some(format!(
            "T-SQL rejects this datepart on {} (the source fails as written)",
            dt.name()
        ))
    };
    let target = if db2 { "Db2 for i" } else { "PostgreSQL" };
    match (dt, u) {
        (Dt::Date, _) if !u.calendar() => rejects(),
        (Dt::Time(_), _) if u.calendar() => rejects(),
        (Dt::Date | Dt::DateTime | Dt::SmallDateTime, Micro | Nano) => rejects(),
        (_, Nano) => Some(format!("nanoseconds: T-SQL rounds the number to steps of 100 ns (49 adds nothing, 50 adds 100 ns), and {target} keeps microseconds")),
        (Dt::SmallDateTime, Second | Milli) => Some("seconds or milliseconds on smalldatetime: T-SQL adds nothing for small numbers (seconds from -30 to 29) and otherwise starts from a whole minute; no exact equivalent".into()),
        (Dt::DateTime, Milli) => Some("milliseconds on datetime: T-SQL rounds the result to 1/300 s (.000, .003, .007); no exact equivalent".into()),
        (Dt::Offset(_), _) if db2 => Some("datetimeoffset has no Db2 for i type".into()),
        (Dt::Offset(_), _) if u.calendar() => Some("days or longer on datetimeoffset: T-SQL keeps the value's own offset, while PostgreSQL timestamptz adds calendar days in the session time zone, so a daylight-saving change moves the hour".into()),
        (Dt::Time(_), Milli | Micro) if db2 => Some("fractions of a second on time: Db2 for i TIME has no fractional seconds".into()),
        (_, Milli) if dt.scale() < 3 => Some(format!("milliseconds on {}: T-SQL rounds the result to the type's fractional digits; not checked", dt.name())),
        (_, Micro) if dt.scale() < 6 => Some(format!("microseconds on {}: T-SQL rounds the result to the type's fractional digits; not checked", dt.name())),
        _ => None,
    }
}

/// A DATEADD number made an integer: its text, and whether it is one token
/// (or a negative literal) that needs no parentheses.
struct Num {
    text: String,
    simple: bool,
}

impl Num {
    fn paren(&self) -> String {
        if self.simple {
            self.text.clone()
        } else {
            format!("({})", self.text)
        }
    }

    /// `5` for the literal `-5`.
    fn negated_literal(&self) -> Option<&str> {
        let n = self.text.strip_prefix('-')?.trim_start();
        n.bytes().all(|b| b.is_ascii_digit()).then_some(n)
    }

    /// The number times `m`: folded for a literal.
    fn times(&self, m: i64) -> String {
        let lit = self.text.replace(' ', "").parse::<i64>().ok();
        match lit {
            Some(n) => (n * m).to_string(),
            None => format!("{m} * {}", self.paren()),
        }
    }
}

impl Cx<'_> {
    /// The catalog column types come from: the tree's, or else one read
    /// from this input's own CREATE TABLE statements.
    fn type_catalog(&self) -> Option<&sqldiff_core::catalog::Catalog> {
        if let Some(c) = self.catalog {
            return Some(c);
        }
        self.local
            .get_or_init(|| sqldiff_core::catalog::ddl::load(self.src, Dialect::Tsql, "input"))
            .into()
    }

    /// The catalog table of a scope entry whose columns are known.
    fn entry_table(&self, e: &ScopeEntry) -> Option<&sqldiff_core::catalog::Table> {
        use sqldiff_core::catalog::{table_position, Ident};
        if !e.named {
            return None;
        }
        let cat = self.type_catalog()?;
        let parts: Vec<Ident> = match self.image_table(&e.name) {
            [.., s, t] if s != "dbo" => vec![Ident::plain(s.clone()), Ident::plain(t.clone())],
            [.., t] => vec![Ident::plain(t.clone())],
            [] => return None,
        };
        let t = &cat.tables[table_position(Dialect::Tsql, cat, &parts)?];
        t.columns_known.then_some(t)
    }

    /// The type of a column reference, by the statement's tables. A bare
    /// name needs every table of the statement known, and every table that
    /// has the column to agree on its type.
    fn column_dt(&self, ts: &[T]) -> Option<Dt> {
        use sqldiff_core::catalog::Ident;
        let parts = name_parts(ts)?;
        let scope = self.scope.as_ref()?;
        let (q, col) = match parts.as_slice() {
            [c] => (None, c),
            [.., q, c] => (Some(q), c),
            [] => return None,
        };
        let mut found = None;
        for e in &scope.entries {
            let here = q.map_or(true, |q| match &e.alias {
                Some(a) => a == q,
                None => e.name.last() == Some(q),
            });
            if !here {
                continue;
            }
            let t = self.entry_table(e)?;
            if let Some(c) = t.column(Dialect::Tsql, &Ident::plain(col.clone())) {
                let dt = Dt::of(&c.data_type.base, &c.data_type.args);
                if found.is_some_and(|f| f != dt) {
                    return None;
                }
                found = Some(dt);
            }
        }
        found
    }

    /// The T-SQL type of a plain expression: a variable, a column, a
    /// literal, a cast or a date function. `None` when it is not known.
    fn expr_dt(&self, ts: &[T]) -> Option<Dt> {
        let ts = trimmed(ts);
        let s = sig(ts);
        let &first = s.first()?;
        let t = &ts[first];
        let last = ts.len() - 1;
        if t.k == L_PAREN && matching(ts, first) == Some(last) {
            return self.expr_dt(&ts[first + 1..last]);
        }
        match s.as_slice() {
            [_] => match t.k {
                VARIABLE => self
                    .vars
                    .get(&t.s.trim_start_matches('@').to_ascii_lowercase())
                    .map(|v| v.dt),
                STRING => Some(Dt::Text),
                NUMBER => Some(number_dt(&t.s)),
                IDENT if t.is("CURRENT_TIMESTAMP") => Some(Dt::DateTime),
                IDENT | QUOTED_IDENT => self.column_dt(ts),
                _ => None,
            },
            [_, n, ..] if t.k == OP && t.s == "-" => {
                let d = self.expr_dt(&ts[*n..])?;
                matches!(d, Dt::Int | Dt::BigInt | Dt::Fraction | Dt::Money).then_some(d)
            }
            [_, p, ..] if t.k == IDENT && ts[*p].k == L_PAREN && matching(ts, *p) == Some(last) => {
                let a = split_commas(&ts[p + 1..last]);
                match (t.up().as_str(), a.len()) {
                    ("CAST", 1) => {
                        let at = crate::tok::find_word(a[0], 0, "AS")?;
                        Some(Dt::of_tokens(&a[0][at + 1..]))
                    }
                    ("CONVERT", 2 | 3) => Some(Dt::of_tokens(a[0])),
                    ("DATEADD", 3) => Some(match self.expr_dt(a[2])? {
                        Dt::Text => Dt::DateTime,
                        d => d,
                    }),
                    ("GETDATE" | "GETUTCDATE", 0) => Some(Dt::DateTime),
                    ("SYSDATETIME" | "SYSUTCDATETIME", 0) => Some(Dt::DateTime2(7)),
                    ("SYSDATETIMEOFFSET", 0) => Some(Dt::Offset(7)),
                    _ => None,
                }
            }
            _ => self.column_dt(ts),
        }
    }

    /// The DATEADD number as an integer, made the way T-SQL makes it: a
    /// fraction is truncated toward zero, a value outside int is an error.
    fn dateadd_number(&mut self, ts: &[T]) -> Result<Num, String> {
        let x = self.expr(ts).trim().to_string();
        let one = sig(trimmed(ts)).len() == 1;
        let ty = self.expr_dt(ts);
        match ty {
            Some(Dt::Int) => Ok(Num {
                simple: one || number_dt(x.trim_start_matches('-').trim()) == Dt::Int,
                text: x,
            }),
            Some(Dt::Text) => Err("the number is a string; CAST it to int".into()),
            Some(Dt::Other | Dt::Bit) => Err("the number is not a number".into()),
            Some(d) if d.is_date() => Err("the number is a date or time value".into()),
            _ if self.db2() => {
                if ty != Some(Dt::BigInt) {
                    self.note(Status::Assumptions, "ASM-DB2-DATEADD-NUMBER", "T-SQL truncates a DATEADD number with a fraction toward zero (1.9 → 1, -1.9 → -1); CAST(… AS INTEGER) on Db2 for i is assumed to truncate the same way (fact V27, not checked on a real system).");
                }
                Ok(Num {
                    text: format!("CAST({x} AS INTEGER)"),
                    simple: false,
                })
            }
            Some(Dt::BigInt) => Ok(Num {
                text: format!("({x})::integer"),
                simple: true,
            }),
            _ => Ok(Num {
                text: format!("trunc({x})::integer"),
                simple: true,
            }),
        }
    }

    /// DATEADD: the result keeps the type of the date argument, as in
    /// T-SQL. `None` (with a note) where there is no exact rewrite.
    fn dateadd(&mut self, a: &[&[T]]) -> Option<String> {
        let refuse = |cx: &mut Self, why: &str| {
            cx.note(
                Status::NotConverted,
                "tsql.dateadd",
                &format!("DATEADD: {why}."),
            );
            None
        };
        let part = self.expr(a[0]).trim().to_string();
        let Some(u) = Unit::of(&part) else {
            return refuse(self, &format!("`{part}` is not a datepart"));
        };
        let dt = match self.expr_dt(a[2]) {
            Some(Dt::Text) => return refuse(self, "the date is a string, which T-SQL reads as datetime by the session's DATEFORMAT and language; write CAST('…' AS date) or CAST('…' AS datetime2) so its type is explicit"),
            Some(d) if d.is_date() => d,
            Some(_) => return refuse(self, "the date argument is not a date or time value"),
            None => return refuse(self, "the type of the date argument is not known, and the result takes that type (a date stays a date); use a declared variable, a column of a table defined in the converted files, or CAST(… AS date)"),
        };
        if let Some(why) = dateadd_refusal(dt, u, self.db2()) {
            return refuse(self, &why);
        }
        let k = match self.dateadd_number(a[1]) {
            Ok(k) => k,
            Err(why) => return refuse(self, &why),
        };
        let d = self.expr(a[2]).trim().to_string();
        if !matches!(dt, Dt::Time(_)) {
            let text = self.pick(
                "DATEADD raises an error when the result leaves the range of its type (date and datetime2 0001–9999, datetime 1753–9999, smalldatetime 1900 to 2079-06-06); PostgreSQL returns the value instead. Equal while results stay in range.",
                "DATEADD raises an error when the result leaves the range of its type (date and datetime2 0001–9999, datetime 1753–9999, smalldatetime 1900 to 2079-06-06). Db2 for i dates and timestamps also end at 0001 and 9999 and are assumed to raise an error there too (fact V27, not checked on a real system); inside 0001–9999 but outside the datetime or smalldatetime range Db2 returns the value instead. Equal while results stay in range.",
            ).to_string();
            self.note(Status::Assumptions, "ASM-DATEADD-RANGE", &text);
        }
        if matches!(u, Unit::Year | Unit::Quarter) {
            self.note(Status::Assumptions, "ASM-DATEADD-YEAR-END", "Microsoft documents the month-end rule for month only (Jan 31 + 1 month = the last day of February); years and quarters are assumed to clamp the same way (2024-02-29 + 1 year = 2025-02-28), as they do in the target. Not checked on SQL Server.");
        }
        if matches!(dt, Dt::Time(_)) {
            self.note(Status::Assumptions, "ASM-DATEADD-TIME-WRAP", "On time, T-SQL DATEADD is assumed to wrap around midnight (23:00 + 2 hours = 01:00); Microsoft's page does not say. Not checked on SQL Server.");
        }
        if self.db2() {
            if u.months() {
                self.note(Status::Assumptions, "ASM-DB2-MONTH-END", "Adding months, quarters or years: T-SQL moves a day that the result month lacks to that month's last day (Jan 31 + 1 month = Feb 29 in 2024). Db2 for i is assumed to do the same and to return warning 01506, which does not stop the routine (fact V27, not checked on a real system).");
            }
            if matches!(dt, Dt::Time(_)) {
                self.note(Status::Assumptions, "ASM-DB2-TIME-WRAP", "On time, Db2 for i TIME arithmetic is assumed to wrap around midnight as T-SQL does (23:00 + 2 hours = 01:00; fact V27, not checked on a real system).");
            }
            let (n, unit) = match u {
                Unit::Year => (k.paren(), "YEARS"),
                Unit::Quarter => (format!("({})", k.times(3)), "MONTHS"),
                Unit::Month => (k.paren(), "MONTHS"),
                Unit::Day => (k.paren(), "DAYS"),
                Unit::Week => (format!("({})", k.times(7)), "DAYS"),
                Unit::Hour => (k.paren(), "HOURS"),
                Unit::Minute => (k.paren(), "MINUTES"),
                Unit::Second => (k.paren(), "SECONDS"),
                // BIGINT: a million milliseconds overflows INTEGER as microseconds.
                Unit::Milli if k.negated_literal().is_some() || k.text.parse::<i64>().is_ok() => {
                    (format!("({})", k.times(1000)), "MICROSECONDS")
                }
                Unit::Milli => (
                    format!("(CAST({} AS BIGINT) * 1000)", k.text),
                    "MICROSECONDS",
                ),
                Unit::Micro | Unit::Nano => (k.paren(), "MICROSECONDS"),
            };
            // `d - 5 DAYS` for a negative literal (folded or not).
            let lit = n.trim_start_matches('(').trim_end_matches(')');
            return Some(match lit.parse::<i64>() {
                Ok(m) if m < 0 => format!("({d} - {} {unit})", -m),
                Ok(m) => format!("({d} + {m} {unit})"),
                Err(_) => format!("({d} + {n} {unit})"),
            });
        }
        Some(match (dt, u) {
            (Dt::Date, Unit::Day) => match k.negated_literal() {
                Some(m) => format!("({d} - {m})"),
                None => format!("({d} + {})", k.paren()),
            },
            (Dt::Date, Unit::Week) => match k.text.replace(' ', "").parse::<i64>() {
                Ok(n) if n < 0 => format!("({d} - {})", -7 * n),
                Ok(n) => format!("({d} + {})", 7 * n),
                Err(_) => format!("({d} + {})", k.times(7)),
            },
            (_, Unit::Milli) => format!("({d} + {} * interval '1 millisecond')", k.paren()),
            (_, Unit::Micro) => format!("({d} + {} * interval '1 microsecond')", k.paren()),
            _ => {
                let (field, n) = match u {
                    Unit::Year => ("years", k.text.clone()),
                    Unit::Quarter => ("months", k.times(3)),
                    Unit::Month => ("months", k.text.clone()),
                    Unit::Day => ("days", k.text.clone()),
                    Unit::Week => ("weeks", k.text.clone()),
                    Unit::Hour => ("hours", k.text.clone()),
                    Unit::Minute => ("mins", k.text.clone()),
                    _ => ("secs", k.text.clone()),
                };
                let sum = format!("({d} + make_interval({field} => {n}))");
                // date + interval is a timestamp in PostgreSQL.
                if dt == Dt::Date {
                    format!("{sum}::date")
                } else {
                    sum
                }
            }
        })
    }
}

/// Whether a select-list item holds an aggregate call of its own, possibly
/// inside other function calls (`NULLIF(COUNT(*), 0)`) but not inside a
/// subquery, and no window function: a query whose items are like that and has
/// no grouping returns exactly one row.
fn has_aggregate(item: &[T]) -> bool {
    // For every open parenthesis: whether it opens a subquery.
    let mut subquery: Vec<bool> = Vec::new();
    let mut found = false;
    for (j, t) in item.iter().enumerate() {
        if t.k == L_PAREN {
            subquery.push(next_sig(item, j).is_some_and(|n| item[n].is("SELECT")));
        } else if t.k == R_PAREN {
            subquery.pop();
        } else if t.is("OVER") {
            return false;
        } else if ["COUNT", "COUNT_BIG", "SUM", "MIN", "MAX", "AVG"]
            .iter()
            .any(|f| t.is(f))
            && next_sig(item, j).is_some_and(|n| item[n].k == L_PAREN)
            && !subquery.iter().any(|&q| q)
        {
            found = true;
        }
    }
    found
}
