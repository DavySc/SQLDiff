//! Statement-level recursive-descent parser producing a lossless rowan tree.
//!
//! Expressions are kept flat inside their statement; structure is built for
//! statements and procedural compounds, which is what statement alignment,
//! transaction analysis and lints need first.

use std::collections::HashSet;
use std::mem;

use rowan::{Checkpoint, GreenNode, GreenNodeBuilder};

use crate::dialect::Dialect;
use crate::kind::SyntaxKind::{self, *};
use crate::kind::SyntaxNode;
use crate::lexer::{tokenize, tokenize_range, Token};
use crate::Diagnostic;

/// Result of parsing one artifact.
#[derive(Debug, Clone)]
pub struct Parse {
    green: GreenNode,
    pub diagnostics: Vec<Diagnostic>,
    pub dialect: Dialect,
}

impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }
}

/// Parse a complete artifact in the given dialect.
pub fn parse(text: &str, dialect: Dialect) -> Parse {
    let lexed = tokenize(text, dialect);
    let mut p = Parser {
        src: text,
        dialect,
        tokens: lexed.tokens,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diags: lexed.diagnostics,
        mode: Mode::Sql,
        stops: Vec::new(),
    };
    p.parse_file();
    Parse {
        green: p.builder.finish(),
        diagnostics: p.diags,
        dialect,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sql,
    PlPgSql,
}

/// T-SQL keywords that can begin a statement; used to infer missing terminators.
const TSQL_STARTERS: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "SET",
    "DECLARE",
    "IF",
    "WHILE",
    "BEGIN",
    "RETURN",
    "THROW",
    "RAISERROR",
    "PRINT",
    "EXEC",
    "EXECUTE",
    "COMMIT",
    "ROLLBACK",
    "SAVE",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "GOTO",
    "BREAK",
    "CONTINUE",
    "WAITFOR",
    "FETCH",
    "OPEN",
    "CLOSE",
    "DEALLOCATE",
    "USE",
    "GRANT",
    "REVOKE",
    "DENY",
];

/// Words after which a starter keyword continues the current statement.
const TSQL_CONTINUE_AFTER: &[&str] = &[
    "FOR",
    "THEN",
    "OF",
    "AFTER",
    "BEFORE",
    "INSTEAD",
    "UNION",
    "ALL",
    "EXCEPT",
    "INTERSECT",
    "OR",
    "AND",
    "NOT",
    "WITH",
    "GRANT",
    "REVOKE",
    "DENY",
];

struct Parser<'a> {
    src: &'a str,
    dialect: Dialect,
    tokens: Vec<Token>,
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    diags: Vec<Diagnostic>,
    mode: Mode,
    stops: Vec<&'static [&'static str]>,
}

impl<'a> Parser<'a> {
    // ---- token access ----------------------------------------------------

    fn sig_index(&self, n: usize) -> Option<usize> {
        let mut seen = 0;
        for i in self.pos..self.tokens.len() {
            if !self.tokens[i].kind.is_trivia() {
                if seen == n {
                    return Some(i);
                }
                seen += 1;
            }
        }
        None
    }

    fn nth_kind(&self, n: usize) -> Option<SyntaxKind> {
        self.sig_index(n).map(|i| self.tokens[i].kind)
    }

    fn text_of(&self, i: usize) -> &'a str {
        let t = self.tokens[i];
        &self.src[t.start..t.end]
    }

    fn nth_text(&self, n: usize) -> Option<&'a str> {
        self.sig_index(n).map(|i| self.text_of(i))
    }

    fn nth_word(&self, n: usize) -> Option<&'a str> {
        let i = self.sig_index(n)?;
        (self.tokens[i].kind == IDENT).then(|| self.text_of(i))
    }

    fn nth_kw(&self, n: usize, kw: &str) -> bool {
        matches!(self.nth_word(n), Some(w) if w.eq_ignore_ascii_case(kw))
    }

    fn at_kw(&self, kw: &str) -> bool {
        self.nth_kw(0, kw)
    }

    fn at_any_kw(&self, kws: &[&str]) -> bool {
        kws.iter().any(|k| self.at_kw(k))
    }

    fn upper_word(&self) -> String {
        self.nth_word(0)
            .map(|w| w.to_ascii_uppercase())
            .unwrap_or_default()
    }

    fn at_stop(&self) -> bool {
        match self.nth_kind(0) {
            None | Some(BATCH_SEP) => true,
            Some(IDENT) => {
                let w = self.nth_word(0).unwrap_or_default();
                self.stops
                    .iter()
                    .any(|s| s.iter().any(|k| w.eq_ignore_ascii_case(k)))
            }
            _ => false,
        }
    }

    // ---- tree building ---------------------------------------------------

    fn emit_current(&mut self) {
        let t = self.tokens[self.pos];
        self.builder.token(t.kind.into(), &self.src[t.start..t.end]);
        self.pos += 1;
    }

    fn bump_trivia(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind.is_trivia() {
            self.emit_current();
        }
    }

    fn bump(&mut self) {
        self.bump_trivia();
        if self.pos < self.tokens.len() {
            self.emit_current();
        }
    }

    fn expect_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            self.diag_here(format!("expected {kw}"));
            false
        }
    }

    fn start(&mut self, kind: SyntaxKind) {
        self.builder.start_node(kind.into());
    }

    fn start_at(&mut self, cp: Checkpoint, kind: SyntaxKind) {
        self.builder.start_node_at(cp, kind.into());
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    fn diag_here(&mut self, message: impl Into<String>) {
        let offset = self
            .sig_index(0)
            .map(|i| self.tokens[i].start)
            .unwrap_or(self.src.len());
        self.diags.push(Diagnostic {
            offset,
            message: message.into(),
        });
    }

    fn with_stops<R>(
        &mut self,
        stops: &'static [&'static str],
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.stops.push(stops);
        let r = f(self);
        self.stops.pop();
        r
    }

    // ---- grammar ---------------------------------------------------------

    fn parse_file(&mut self) {
        self.start(SOURCE_FILE);
        if self.dialect == Dialect::Tsql {
            loop {
                self.bump_trivia();
                match self.nth_kind(0) {
                    None => break,
                    Some(BATCH_SEP) => {
                        self.bump();
                        continue;
                    }
                    _ => {}
                }
                self.start(BATCH);
                self.parse_list();
                self.finish();
            }
        } else {
            self.parse_list();
        }
        self.bump_trivia();
        while self.pos < self.tokens.len() {
            self.emit_current();
        }
        self.finish();
    }

    fn parse_list(&mut self) {
        loop {
            if self.at_stop() {
                break;
            }
            let before = self.pos;
            self.parse_statement();
            if self.pos == before {
                self.bump_trivia();
                self.diag_here("unexpected token");
                self.start(ERROR);
                self.bump();
                self.finish();
            }
        }
    }

    fn parse_one(&mut self) {
        if self.at_stop() {
            self.diag_here("expected a statement");
            return;
        }
        self.parse_statement();
    }

    fn parse_branch(&mut self, stops: &'static [&'static str]) {
        self.bump_trivia();
        self.start(BRANCH);
        self.with_stops(stops, |p| p.parse_list());
        self.finish();
    }

    fn parse_statement(&mut self) {
        self.bump_trivia();
        let cp = self.builder.checkpoint();
        if self.try_label() && self.dialect == Dialect::Tsql {
            return;
        }
        let d = self.dialect;
        let pl = d == Dialect::Postgres && self.mode == Mode::PlPgSql;
        let w = self.upper_word();
        match w.as_str() {
            "BEGIN" => self.parse_begin(cp),
            "DECLARE" if pl => self.parse_pg_block(cp),
            "DECLARE" if d == Dialect::Db2i && self.nth_kw(2, "HANDLER") => {
                self.parse_db2_handler(cp)
            }
            "IF" if d == Dialect::Tsql => self.parse_tsql_if(cp),
            "IF" if d == Dialect::Db2i || pl => self.parse_if(cp),
            "WHILE" if d == Dialect::Tsql => self.parse_tsql_while(cp),
            "WHILE" if pl => self.parse_loop(cp, Some("LOOP"), "LOOP"),
            "WHILE" if d == Dialect::Db2i => self.parse_loop(cp, Some("DO"), "WHILE"),
            "LOOP" if pl || d == Dialect::Db2i => self.parse_loop(cp, None, "LOOP"),
            "FOR" | "FOREACH" if pl => self.parse_loop(cp, Some("LOOP"), "LOOP"),
            "FOR" if d == Dialect::Db2i => self.parse_loop(cp, Some("DO"), "FOR"),
            "REPEAT" if d == Dialect::Db2i => self.parse_repeat(cp),
            "CASE" if pl || d == Dialect::Db2i => self.parse_case_stmt(cp),
            "CREATE" | "ALTER" if self.is_routine_start() => match d {
                Dialect::Tsql => self.parse_tsql_routine(cp),
                Dialect::Postgres => self.parse_pg_routine(cp, false),
                Dialect::Db2i => self.parse_db2_routine(cp),
            },
            "DO" if d == Dialect::Postgres
                && (self.nth_kind(1) == Some(DOLLAR_STRING) || self.nth_kw(1, "LANGUAGE")) =>
            {
                self.parse_pg_routine(cp, true)
            }
            _ => self.parse_simple(cp),
        }
    }

    /// Statement labels: `lbl:` (T-SQL GOTO target, Db2 compound label) and `<<lbl>>` (PL/pgSQL).
    fn try_label(&mut self) -> bool {
        let is_label = match self.dialect {
            Dialect::Tsql => self.nth_kind(0) == Some(IDENT) && self.nth_kind(1) == Some(COLON),
            Dialect::Db2i => {
                self.nth_kind(0) == Some(IDENT)
                    && self.nth_kind(1) == Some(COLON)
                    && ["BEGIN", "LOOP", "WHILE", "FOR", "REPEAT"]
                        .iter()
                        .any(|k| self.nth_kw(2, k))
            }
            Dialect::Postgres => {
                self.nth_text(0) == Some("<<")
                    && self.nth_kind(1) == Some(IDENT)
                    && self.nth_text(2) == Some(">>")
            }
        };
        if !is_label {
            return false;
        }
        self.start(LABEL);
        let n = if self.dialect == Dialect::Postgres {
            3
        } else {
            2
        };
        for _ in 0..n {
            self.bump();
        }
        self.finish();
        true
    }

    fn is_routine_start(&self) -> bool {
        let n = if self.nth_kw(1, "OR") { 3 } else { 1 };
        let Some(w) = self.nth_word(n) else {
            return false;
        };
        let w = w.to_ascii_uppercase();
        let create = self.at_kw("CREATE");
        match self.dialect {
            Dialect::Tsql => matches!(
                w.as_str(),
                "PROCEDURE" | "PROC" | "FUNCTION" | "TRIGGER" | "VIEW"
            ),
            Dialect::Postgres => create && matches!(w.as_str(), "PROCEDURE" | "FUNCTION"),
            Dialect::Db2i => create && matches!(w.as_str(), "PROCEDURE" | "FUNCTION" | "TRIGGER"),
        }
    }

    fn expect_end(&mut self, word: Option<&str>) {
        if !self.at_kw("END") {
            self.diag_here(match word {
                Some(w) => format!("expected END {w}"),
                None => "expected END".to_string(),
            });
            return;
        }
        self.bump();
        if let Some(w) = word {
            self.expect_kw(w);
        }
        if self.dialect != Dialect::Tsql
            && self.nth_kind(0) == Some(IDENT)
            && matches!(self.nth_kind(1), None | Some(SEMICOLON))
        {
            self.bump(); // END label
        }
        if self.nth_kind(0) == Some(SEMICOLON) {
            self.bump();
        }
    }

    /// Consume tokens up to (not including) one of `terms` at nesting depth 0.
    fn parse_condition(&mut self, terms: &[&str]) {
        self.bump_trivia();
        self.start(CONDITION);
        let mut depth = 0usize;
        let mut case = 0usize;
        while let Some(k) = self.nth_kind(0) {
            if matches!(k, SEMICOLON | BATCH_SEP) {
                break;
            }
            if depth == 0 && case == 0 && k == IDENT && self.at_any_kw(terms) {
                break;
            }
            match k {
                L_PAREN | L_BRACKET => depth += 1,
                R_PAREN | R_BRACKET => depth = depth.saturating_sub(1),
                IDENT if self.at_kw("CASE") => case += 1,
                IDENT if self.at_kw("END") && case > 0 => case -= 1,
                _ => {}
            }
            self.bump();
        }
        self.finish();
    }

    fn bump_balanced_parens(&mut self) {
        if self.nth_kind(0) != Some(L_PAREN) {
            return;
        }
        let mut depth = 0usize;
        while let Some(k) = self.nth_kind(0) {
            match k {
                L_PAREN => depth += 1,
                R_PAREN => depth -= 1,
                _ => {}
            }
            self.bump();
            if depth == 0 {
                break;
            }
        }
    }

    fn parse_begin(&mut self, cp: Checkpoint) {
        match self.dialect {
            Dialect::Tsql => {
                if [
                    "TRAN",
                    "TRANSACTION",
                    "DISTRIBUTED",
                    "DIALOG",
                    "CONVERSATION",
                ]
                .iter()
                .any(|k| self.nth_kw(1, k))
                {
                    self.parse_simple(cp);
                } else if self.nth_kw(1, "TRY") {
                    self.parse_try_catch(cp);
                } else {
                    self.start_at(cp, BLOCK);
                    self.bump();
                    self.with_stops(&["END"], |p| p.parse_list());
                    self.expect_end(None);
                    self.finish();
                }
            }
            Dialect::Postgres => {
                if self.mode == Mode::PlPgSql {
                    self.parse_pg_block(cp);
                } else if self.nth_kw(1, "ATOMIC") {
                    self.start_at(cp, BLOCK);
                    self.bump();
                    self.bump();
                    self.with_stops(&["END"], |p| p.parse_list());
                    self.expect_end(None);
                    self.finish();
                } else {
                    self.parse_simple(cp); // BEGIN [TRANSACTION]
                }
            }
            Dialect::Db2i => {
                self.start_at(cp, BLOCK);
                self.bump();
                if self.at_kw("NOT") && self.nth_kw(1, "ATOMIC") {
                    self.bump();
                    self.bump();
                } else if self.at_kw("ATOMIC") {
                    self.bump();
                }
                self.with_stops(&["END"], |p| p.parse_list());
                self.expect_end(None);
                self.finish();
            }
        }
    }

    fn parse_try_catch(&mut self, cp: Checkpoint) {
        self.start_at(cp, TRY_CATCH);
        self.start(TRY_BLOCK);
        self.bump(); // BEGIN
        self.bump(); // TRY
        self.with_stops(&["END"], |p| p.parse_list());
        self.expect_end(Some("TRY"));
        self.finish();
        self.bump_trivia();
        if self.at_kw("BEGIN") && self.nth_kw(1, "CATCH") {
            self.start(CATCH_BLOCK);
            self.bump();
            self.bump();
            self.with_stops(&["END"], |p| p.parse_list());
            self.expect_end(Some("CATCH"));
            self.finish();
        } else {
            self.diag_here("expected BEGIN CATCH");
        }
        self.finish();
    }

    fn parse_pg_block(&mut self, cp: Checkpoint) {
        self.start_at(cp, BLOCK);
        if self.at_kw("DECLARE") {
            self.bump_trivia();
            self.start(DECLARE_SECTION);
            self.bump();
            self.with_stops(&["BEGIN"], |p| loop {
                if p.at_stop() {
                    break;
                }
                p.bump_trivia();
                let before = p.pos;
                let cp = p.builder.checkpoint();
                p.parse_simple(cp);
                if p.pos == before {
                    break;
                }
            });
            self.finish();
        }
        if !self.expect_kw("BEGIN") {
            self.finish();
            return;
        }
        self.with_stops(&["END", "EXCEPTION"], |p| p.parse_list());
        if self.at_kw("EXCEPTION") {
            self.bump_trivia();
            self.start(EXCEPTION_SECTION);
            self.bump();
            while self.at_kw("WHEN") {
                self.bump_trivia();
                self.start(EXCEPTION_HANDLER);
                self.bump();
                self.parse_condition(&["THEN"]);
                self.expect_kw("THEN");
                self.parse_branch(&["WHEN", "END"]);
                self.finish();
            }
            self.finish();
        }
        self.expect_end(None);
        self.finish();
    }

    fn parse_db2_handler(&mut self, cp: Checkpoint) {
        self.start_at(cp, HANDLER);
        self.bump(); // DECLARE
        self.bump(); // CONTINUE | EXIT | UNDO
        self.bump(); // HANDLER
        self.expect_kw("FOR");
        self.bump_trivia();
        self.start(CONDITION);
        loop {
            if self.at_kw("SQLSTATE") {
                self.bump();
                if self.at_kw("VALUE") {
                    self.bump();
                }
                if self.nth_kind(0) == Some(STRING) {
                    self.bump();
                }
            } else if self.at_kw("NOT") {
                self.bump();
                if self.at_kw("FOUND") {
                    self.bump();
                }
            } else if self.nth_kind(0) == Some(IDENT) {
                self.bump();
            } else {
                break;
            }
            if self.nth_kind(0) == Some(COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        self.finish();
        self.bump_trivia();
        self.start(BRANCH);
        self.parse_one();
        self.finish();
        self.finish();
    }

    fn parse_tsql_condition(&mut self) {
        self.bump_trivia();
        self.start(CONDITION);
        let mut depth = 0usize;
        let mut case = 0usize;
        let mut first = true;
        let mut prev_word = String::new();
        let mut prev_kind = None;
        while let Some(k) = self.nth_kind(0) {
            if matches!(k, SEMICOLON | BATCH_SEP) {
                break;
            }
            if !first && depth == 0 && case == 0 && k == IDENT {
                let w = self.upper_word();
                let starter = TSQL_STARTERS.contains(&w.as_str()) || w == "ELSE" || w == "END";
                let continues = matches!(prev_word.as_str(), "AND" | "OR" | "NOT")
                    || matches!(prev_kind, Some(COMMA | L_PAREN));
                if starter && !continues {
                    break;
                }
            }
            match k {
                L_PAREN => depth += 1,
                R_PAREN => depth = depth.saturating_sub(1),
                IDENT if self.at_kw("CASE") => case += 1,
                IDENT if self.at_kw("END") && case > 0 => case -= 1,
                _ => {}
            }
            prev_word = if k == IDENT {
                self.upper_word()
            } else {
                String::new()
            };
            prev_kind = Some(k);
            self.bump();
            first = false;
        }
        self.finish();
    }

    fn parse_tsql_if(&mut self, cp: Checkpoint) {
        self.start_at(cp, IF_STMT);
        self.bump(); // IF
        self.parse_tsql_condition();
        self.bump_trivia();
        self.start(BRANCH);
        self.with_stops(&["ELSE"], |p| p.parse_one());
        self.finish();
        if self.at_kw("ELSE") {
            self.bump();
            self.bump_trivia();
            self.start(BRANCH);
            self.parse_one();
            self.finish();
        }
        self.finish();
    }

    fn parse_tsql_while(&mut self, cp: Checkpoint) {
        self.start_at(cp, LOOP_STMT);
        self.bump(); // WHILE
        self.parse_tsql_condition();
        self.bump_trivia();
        self.start(BRANCH);
        self.parse_one();
        self.finish();
        self.finish();
    }

    fn parse_if(&mut self, cp: Checkpoint) {
        self.start_at(cp, IF_STMT);
        self.bump(); // IF
        self.parse_condition(&["THEN"]);
        self.expect_kw("THEN");
        self.parse_branch(&["ELSE", "ELSIF", "ELSEIF", "END"]);
        while self.at_any_kw(&["ELSIF", "ELSEIF"]) {
            self.bump();
            self.parse_condition(&["THEN"]);
            self.expect_kw("THEN");
            self.parse_branch(&["ELSE", "ELSIF", "ELSEIF", "END"]);
        }
        if self.at_kw("ELSE") {
            self.bump();
            self.parse_branch(&["END"]);
        }
        self.expect_end(Some("IF"));
        self.finish();
    }

    fn parse_loop(&mut self, cp: Checkpoint, opener: Option<&'static str>, end_word: &str) {
        self.start_at(cp, LOOP_STMT);
        self.bump(); // WHILE | FOR | FOREACH | LOOP
        if let Some(op) = opener {
            self.parse_condition(&[op]);
            self.expect_kw(op);
        }
        self.parse_branch(&["END"]);
        self.expect_end(Some(end_word));
        self.finish();
    }

    fn parse_repeat(&mut self, cp: Checkpoint) {
        self.start_at(cp, LOOP_STMT);
        self.bump(); // REPEAT
        self.parse_branch(&["UNTIL"]);
        self.expect_kw("UNTIL");
        self.parse_condition(&["END"]);
        self.expect_end(Some("REPEAT"));
        self.finish();
    }

    fn parse_case_stmt(&mut self, cp: Checkpoint) {
        self.start_at(cp, CASE_STMT);
        self.bump(); // CASE
        if !self.at_kw("WHEN") {
            self.parse_condition(&["WHEN"]);
        }
        while self.at_kw("WHEN") {
            self.bump();
            self.parse_condition(&["THEN"]);
            self.expect_kw("THEN");
            self.parse_branch(&["WHEN", "ELSE", "END"]);
        }
        if self.at_kw("ELSE") {
            self.bump();
            self.parse_branch(&["END"]);
        }
        self.expect_end(Some("CASE"));
        self.finish();
    }

    fn parse_tsql_routine(&mut self, cp: Checkpoint) {
        self.start_at(cp, ROUTINE);
        let mut depth = 0usize;
        let mut prev_word = String::new();
        let mut prev_kind = None;
        // `AS` is optional in a function, before the body of a scalar or
        // multi-statement table-valued one (`RETURNS … BEGIN`) and of an inline
        // one (`RETURNS TABLE RETURN …`). Procedures and triggers need it.
        let mut is_function = false;
        let mut saw_returns = false;
        while let Some(k) = self.nth_kind(0) {
            if k == BATCH_SEP {
                break;
            }
            if depth == 0
                && self.at_kw("AS")
                && prev_word != "EXECUTE"
                && prev_kind != Some(VARIABLE)
            {
                break;
            }
            if depth == 0 && k == IDENT {
                if self.at_kw("FUNCTION") {
                    is_function = true;
                } else if self.at_kw("RETURNS") {
                    saw_returns = true;
                } else if is_function && saw_returns && self.at_any_kw(&["BEGIN", "RETURN"]) {
                    break;
                }
            }
            match k {
                L_PAREN => depth += 1,
                R_PAREN => depth = depth.saturating_sub(1),
                _ => {}
            }
            prev_word = if k == IDENT {
                self.upper_word()
            } else {
                String::new()
            };
            prev_kind = Some(k);
            self.bump();
        }
        if self.at_kw("AS") {
            self.bump();
            self.bump_trivia();
            self.start(ROUTINE_BODY);
            self.parse_list();
            self.finish();
        } else if is_function && saw_returns && self.at_any_kw(&["BEGIN", "RETURN"]) {
            self.bump_trivia();
            self.start(ROUTINE_BODY);
            self.parse_list();
            self.finish();
        } else {
            self.diag_here("expected AS before the routine body");
        }
        self.finish();
    }

    fn parse_db2_routine(&mut self, cp: Checkpoint) {
        self.start_at(cp, ROUTINE);
        let mut depth = 0usize;
        let mut after_each = false;
        let mut prev_word = String::new();
        while let Some(k) = self.nth_kind(0) {
            if depth == 0 {
                if k == SEMICOLON {
                    self.bump();
                    self.finish();
                    return;
                }
                let labelled_begin =
                    k == IDENT && self.nth_kind(1) == Some(COLON) && self.nth_kw(2, "BEGIN");
                if self.at_kw("BEGIN") || labelled_begin || self.at_kw("RETURN") {
                    break;
                }
                if after_each {
                    if self.at_kw("MODE") {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    if self.at_kw("WHEN") {
                        self.bump();
                        self.bump_balanced_parens();
                        continue;
                    }
                    if self.at_kw("SET") && self.nth_kw(1, "OPTION") {
                        after_each = false;
                    } else {
                        break; // single-statement trigger body
                    }
                }
            }
            match k {
                L_PAREN => depth += 1,
                R_PAREN => depth = depth.saturating_sub(1),
                _ => {}
            }
            let w = if k == IDENT {
                self.upper_word()
            } else {
                String::new()
            };
            if prev_word == "EACH" && (w == "ROW" || w == "STATEMENT") {
                after_each = true;
            }
            prev_word = w;
            self.bump();
        }
        self.bump_trivia();
        self.start(ROUTINE_BODY);
        self.parse_one();
        self.finish();
        self.finish();
    }

    /// Finds `LANGUAGE x` in the remainder of a PostgreSQL routine definition.
    fn prescan_language(&self) -> Option<String> {
        let mut depth = 0usize;
        let mut i = self.pos;
        while i < self.tokens.len() {
            let t = self.tokens[i];
            i += 1;
            if t.kind.is_trivia() {
                continue;
            }
            match t.kind {
                SEMICOLON if depth == 0 => return None,
                L_PAREN => depth += 1,
                R_PAREN => depth = depth.saturating_sub(1),
                IDENT if self.text_of(i - 1).eq_ignore_ascii_case("LANGUAGE") => {
                    let j = (i..self.tokens.len()).find(|&j| !self.tokens[j].kind.is_trivia())?;
                    let text = self.text_of(j);
                    return Some(text.trim_matches('\'').to_ascii_lowercase());
                }
                _ => {}
            }
        }
        None
    }

    fn parse_pg_routine(&mut self, cp: Checkpoint, is_do: bool) {
        self.start_at(cp, ROUTINE);
        let lang = self
            .prescan_language()
            .unwrap_or_else(|| if is_do { "plpgsql" } else { "sql" }.to_string());
        let body_mode = match lang.as_str() {
            "plpgsql" => Some(Mode::PlPgSql),
            "sql" => Some(Mode::Sql),
            _ => None,
        };
        let mut depth = 0usize;
        let mut prev_word = String::new();
        while let Some(k) = self.nth_kind(0) {
            if depth == 0 && k == SEMICOLON {
                self.bump();
                break;
            }
            if depth == 0 && self.at_kw("BEGIN") && self.nth_kw(1, "ATOMIC") {
                self.bump_trivia();
                self.start(ROUTINE_BODY);
                self.start(BLOCK);
                self.bump();
                self.bump();
                let saved = mem::replace(&mut self.mode, Mode::Sql);
                self.with_stops(&["END"], |p| p.parse_list());
                self.mode = saved;
                if !self.expect_kw("END") {
                    self.finish();
                    self.finish();
                    break;
                }
                self.finish();
                self.finish();
                continue;
            }
            if k == DOLLAR_STRING && (prev_word == "AS" || is_do) {
                if let Some(mode) = body_mode {
                    self.nested_body(mode);
                    prev_word.clear();
                    continue;
                }
            }
            match k {
                L_PAREN => depth += 1,
                R_PAREN => depth = depth.saturating_sub(1),
                _ => {}
            }
            prev_word = if k == IDENT {
                self.upper_word()
            } else {
                String::new()
            };
            self.bump();
        }
        self.finish();
    }

    /// Parse the content of a dollar-quoted string as a nested language.
    fn nested_body(&mut self, mode: Mode) {
        self.bump_trivia();
        let t = self.tokens[self.pos];
        let text = &self.src[t.start..t.end];
        let delim_len = text[1..].find('$').map(|i| i + 2);
        let ok = delim_len.is_some_and(|dl| text.len() >= 2 * dl && text.ends_with(&text[..dl]));
        let Some(dl) = delim_len.filter(|_| ok) else {
            self.bump();
            return;
        };
        self.start(ROUTINE_BODY);
        self.builder.token(DOLLAR_QUOTE.into(), &text[..dl]);
        let inner = tokenize_range(self.src, t.start + dl, t.end - dl, Dialect::Postgres);
        self.diags.extend(inner.diagnostics);
        let saved_tokens = mem::replace(&mut self.tokens, inner.tokens);
        let saved_pos = mem::replace(&mut self.pos, 0);
        let saved_mode = mem::replace(&mut self.mode, mode);
        let saved_stops = mem::take(&mut self.stops);
        self.parse_list();
        self.bump_trivia();
        while self.pos < self.tokens.len() {
            self.emit_current();
        }
        self.tokens = saved_tokens;
        self.pos = saved_pos + 1;
        self.mode = saved_mode;
        self.stops = saved_stops;
        self.builder
            .token(DOLLAR_QUOTE.into(), &text[text.len() - dl..]);
        self.finish();
    }

    fn parse_simple(&mut self, cp: Checkpoint) {
        self.start_at(cp, STATEMENT);
        let tsql = self.dialect == Dialect::Tsql;
        let mut depth = 0usize;
        let mut case = 0usize;
        let mut first = true;
        let mut head = String::new();
        let mut prev_word = String::new();
        let mut prev_kind = None;
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(k) = self.nth_kind(0) {
            if k == BATCH_SEP {
                break;
            }
            if k == SEMICOLON {
                self.bump();
                break;
            }
            if tsql && !first && depth == 0 && case == 0 && k == IDENT {
                let w = self.upper_word();
                if w == "END" || w == "ELSE" {
                    break;
                }
                if tsql_starts_statement(&w, &head, &prev_word, prev_kind, &seen) {
                    self.builder.token(INFERRED_TERMINATOR.into(), "");
                    break;
                }
            }
            match k {
                L_PAREN | L_BRACKET => depth += 1,
                R_PAREN | R_BRACKET => depth = depth.saturating_sub(1),
                _ => {}
            }
            if k == IDENT {
                let w = self.upper_word();
                if w == "CASE" {
                    case += 1;
                } else if w == "END" && case > 0 {
                    case -= 1;
                }
                if depth == 0 {
                    // `WITH cte AS (…) UPDATE …` continues as an UPDATE does.
                    if head.is_empty()
                        || tsql
                            && head == "WITH"
                            && matches!(
                                w.as_str(),
                                "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                            )
                    {
                        head = w.clone();
                    }
                    seen.insert(w.clone());
                }
                prev_word = w;
            } else {
                prev_word.clear();
            }
            prev_kind = Some(k);
            self.bump();
            first = false;
        }
        self.finish();
    }
}

/// Whether `w` begins a new T-SQL statement, given the current statement so far.
fn tsql_starts_statement(
    w: &str,
    head: &str,
    prev_word: &str,
    prev_kind: Option<SyntaxKind>,
    seen: &HashSet<String>,
) -> bool {
    if !TSQL_STARTERS.contains(&w) {
        return false;
    }
    if matches!(prev_kind, Some(COMMA | L_PAREN | DOT)) || TSQL_CONTINUE_AFTER.contains(&prev_word)
    {
        return false;
    }
    // `ON DELETE` / `ON UPDATE` in constraint clauses; `SET NOCOUNT ON` must still end.
    if prev_word == "ON" && matches!(head, "CREATE" | "ALTER") {
        return false;
    }
    let seen_any = |ws: &[&str]| ws.iter().any(|x| seen.contains(*x));
    const DML: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE"];
    match w {
        "SELECT" => {
            !(head == "INSERT" && !seen_any(&["SELECT", "VALUES", "EXEC", "EXECUTE"])
                || head == "WITH" && !seen_any(DML))
        }
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" => !(head == "WITH" && !seen_any(DML)),
        "SET" => {
            !(head == "UPDATE" && !seen.contains("SET") || prev_word == "UPDATE" || head == "ALTER")
        }
        "EXEC" | "EXECUTE" => !(head == "INSERT" && !seen_any(&["SELECT", "VALUES"])),
        "IF" => !matches!(head, "DROP" | "CREATE" | "ALTER"),
        "FETCH" => !matches!(prev_word, "ROWS" | "ROW"),
        "ALTER" | "DROP" => head != "ALTER",
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast;

    fn units(text: &str, d: Dialect) -> Vec<(SyntaxKind, String)> {
        let p = parse(text, d);
        assert_eq!(p.syntax().text().to_string(), text, "lossless");
        p.syntax()
            .descendants()
            .filter(|n| n.kind().is_unit())
            .map(|n| {
                let words: Vec<String> = ast::first_words(&n, 2)
                    .into_iter()
                    .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
                    .collect();
                (n.kind(), words.join(" "))
            })
            .collect()
    }

    #[test]
    fn tsql_procedure_structure() {
        let text = "CREATE PROCEDURE dbo.P @a int AS\nBEGIN\n  SET NOCOUNT ON\n  BEGIN TRAN\n  UPDATE t SET x = 1 WHERE id = @a\n  IF @@ERROR <> 0 BEGIN ROLLBACK; RETURN 1; END\n  COMMIT\nEND\nGO\nSELECT 1";
        let p = parse(text, Dialect::Tsql);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let u = units(text, Dialect::Tsql);
        let kinds: Vec<_> = u.iter().map(|(k, w)| (*k, w.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (ROUTINE, "CREATE PROCEDURE"),
                (BLOCK, "BEGIN SET"),
                (STATEMENT, "SET NOCOUNT"),
                (STATEMENT, "BEGIN TRAN"),
                (STATEMENT, "UPDATE T"),
                (IF_STMT, "IF @@ERROR"),
                (BLOCK, "BEGIN ROLLBACK"),
                (STATEMENT, "ROLLBACK"),
                (STATEMENT, "RETURN 1"),
                (STATEMENT, "COMMIT"),
                (STATEMENT, "SELECT 1"),
            ]
        );
    }

    #[test]
    fn tsql_try_catch_and_terminator_inference() {
        let text = "BEGIN TRY\n INSERT INTO t VALUES (1)\nEND TRY\nBEGIN CATCH\n ROLLBACK\n THROW\nEND CATCH";
        let p = parse(text, Dialect::Tsql);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let root = p.syntax();
        assert!(root.descendants().any(|n| n.kind() == TRY_CATCH));
        let rollback = root
            .descendants()
            .find(|n| n.kind() == STATEMENT && ast::first_words(n, 1) == ["ROLLBACK"])
            .unwrap();
        assert!(ast::ends_with_inferred_terminator(&rollback));
    }

    #[test]
    fn tsql_insert_select_is_one_statement() {
        let u = units(
            "INSERT INTO t (a) SELECT a FROM s WHERE x IN (SELECT 1) UNION SELECT 2",
            Dialect::Tsql,
        );
        assert_eq!(u.len(), 1);
        let u = units(
            "WITH c AS (SELECT 1 AS a) SELECT a FROM c SELECT 2",
            Dialect::Tsql,
        );
        assert_eq!(u.len(), 2);
        let u = units("UPDATE t SET a = 1 WHERE b = 2 SET @x = 1", Dialect::Tsql);
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn tsql_with_data_change_is_one_statement() {
        for (text, n) in [
            (
                "WITH c AS (SELECT a FROM s) UPDATE c SET a = 2 SET @x = 1",
                2,
            ),
            (
                "WITH c AS (SELECT a FROM s) INSERT INTO t (a) SELECT a FROM c SELECT 2",
                2,
            ),
            (
                "WITH c AS (SELECT a FROM s) DELETE c FROM c JOIN t ON t.a = c.a",
                1,
            ),
            (
                "WITH c (a) AS (SELECT 1), d AS (SELECT a FROM c) INSERT t SELECT a FROM d",
                1,
            ),
        ] {
            assert_eq!(units(text, Dialect::Tsql).len(), n, "{text}");
        }
    }

    #[test]
    fn pg_function_body_is_parsed() {
        let text = "CREATE OR REPLACE FUNCTION f(a int) RETURNS void AS $body$\nDECLARE\n  n int := 0;\nBEGIN\n  IF a > 0 THEN\n    UPDATE t SET x = a;\n  ELSIF a < 0 THEN\n    RAISE EXCEPTION 'neg';\n  END IF;\n  FOR i IN 1..3 LOOP n := n + i; END LOOP;\nEXCEPTION WHEN others THEN\n  RAISE NOTICE 'x';\nEND;\n$body$ LANGUAGE plpgsql;";
        let p = parse(text, Dialect::Postgres);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let root = p.syntax();
        for k in [
            ROUTINE_BODY,
            BLOCK,
            DECLARE_SECTION,
            IF_STMT,
            LOOP_STMT,
            EXCEPTION_SECTION,
            EXCEPTION_HANDLER,
        ] {
            assert!(root.descendants().any(|n| n.kind() == k), "missing {k:?}");
        }
        let upd = root
            .descendants()
            .find(|n| n.kind() == STATEMENT && ast::first_words(n, 1) == ["UPDATE"])
            .unwrap();
        // Offsets map into the outer artifact.
        let start: usize = upd.text_range().start().into();
        assert!(text[start..].starts_with("UPDATE t SET x = a;"));
    }

    #[test]
    fn pg_sql_body_and_do() {
        let text = "CREATE FUNCTION g() RETURNS int LANGUAGE sql AS $$ SELECT 1; $$;\nDO $$ BEGIN PERFORM 1; END $$;\nBEGIN;\nCOMMIT;";
        let p = parse(text, Dialect::Postgres);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let u = units(text, Dialect::Postgres);
        let words: Vec<_> = u.iter().map(|(_, w)| w.as_str()).collect();
        assert_eq!(
            words,
            vec![
                "CREATE FUNCTION",
                "SELECT 1",
                "DO",
                "BEGIN PERFORM",
                "PERFORM 1",
                "BEGIN",
                "COMMIT"
            ]
        );
    }

    #[test]
    fn db2_procedure_structure() {
        let text = "CREATE PROCEDURE P (IN a INT)\nLANGUAGE SQL\nSET OPTION COMMIT = *NONE\nP1: BEGIN ATOMIC\n  DECLARE v INT DEFAULT 0;\n  DECLARE EXIT HANDLER FOR SQLEXCEPTION, SQLSTATE '23505'\n    BEGIN ROLLBACK; RESIGNAL; END;\n  WHILE v < 3 DO SET v = v + 1; END WHILE;\n  REPEAT SET v = v - 1; UNTIL v = 0 END REPEAT;\n  CASE WHEN v = 0 THEN SET v = 1; ELSE SET v = 2; END CASE;\n  UPDATE t SET x = CASE WHEN a = 1 THEN 1 ELSE 0 END;\nEND P1;";
        let p = parse(text, Dialect::Db2i);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let root = p.syntax();
        for k in [
            ROUTINE,
            ROUTINE_BODY,
            BLOCK,
            LABEL,
            HANDLER,
            LOOP_STMT,
            CASE_STMT,
        ] {
            assert!(root.descendants().any(|n| n.kind() == k), "missing {k:?}");
        }
        assert!(root
            .descendants()
            .any(|n| n.kind() == STATEMENT && ast::first_words(&n, 1) == ["UPDATE"]));
    }

    #[test]
    fn errors_recover() {
        for d in Dialect::ALL {
            let text = "BEGIN IF x THEN SELECT 1; END; ) ( ; END END CASE";
            let p = parse(text, d);
            assert_eq!(p.syntax().text().to_string(), text);
        }
    }
}
