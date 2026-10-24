//! Routine options in the header (everything between the parameter list
//! and the body). Each option is either compared (the security context),
//! known to have no effect on results (plan and debug settings), covered by a
//! named assumption, or left uncovered so the verdict stays unknown. An
//! option this module does not know is uncovered, never skipped.

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{Parser, Stmt, Tok, K};
use super::canon::asm;
use super::lower::{matching, split_commas, Security, Unit, UnitKind};
use super::schema::Ty;
use crate::classify::{stmt_kind, StmtKind};

/// PostgreSQL settings that only steer planning, memory or logging.
const PG_PLAN_SETTINGS: &[&str] = &[
    "work_mem",
    "maintenance_work_mem",
    "temp_buffers",
    "client_min_messages",
    "log_min_messages",
    "log_min_duration_statement",
    "application_name",
    "jit",
    "plan_cache_mode",
    "random_page_cost",
    "seq_page_cost",
    "cpu_tuple_cost",
    "cpu_index_tuple_cost",
    "cpu_operator_cost",
    "effective_cache_size",
    "from_collapse_limit",
    "join_collapse_limit",
];
const PG_PLAN_PREFIXES: &[&str] = &["enable_", "max_parallel_", "parallel_", "geqo"];

/// Db2 for i SET OPTION entries that only steer compilation and debugging.
const DB2_BUILD_OPTIONS: &[&str] = &["DBGVIEW", "DBGENCKEY", "OUTPUT", "TGTRLS", "COMPILEOPT", "DLYPRP"];

struct Opts<'x> {
    r: &'x SyntaxNode,
    t: &'x [Tok],
    i: usize,
    u: &'x mut Unit,
}

impl Opts<'_> {
    fn up(&self, k: usize) -> &str {
        self.t.get(self.i + k).map(|t| t.up.as_str()).unwrap_or("")
    }

    fn uncovered(&mut self, what: String) {
        self.u.uncovered.push((self.r.clone(), what));
    }

    fn note(&mut self, s: &str) {
        if !self.u.notes.iter().any(|x| x == s) {
            self.u.notes.push(s.to_string());
        }
    }

    fn text(&self, from: usize, to: usize) -> String {
        self.t[from..to.min(self.t.len())]
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Records the security context the tokens `from..to` set.
    fn security(&mut self, s: Security, from: usize, to: usize) {
        let to = to.min(self.t.len());
        let range = (
            self.t[from].off,
            self.t[to - 1].off + self.t[to - 1].text.len(),
        );
        self.u.security = s;
        self.u.security_cite = Some((self.text(from, to), range));
    }

    /// One word of a fixed phrase: true and consumed if all words match.
    fn phrase(&mut self, words: &[&str]) -> bool {
        if words.iter().enumerate().all(|(k, w)| self.up(k) == *w) {
            self.i += words.len();
            true
        } else {
            false
        }
    }

    fn run(&mut self) {
        let d = self.u.dialect;
        let mut usrprf = false;
        while self.i < self.t.len() {
            let t = &self.t[self.i];
            if t.kind != K::Word {
                self.i += 1;
                continue;
            }
            let w = t.up.clone();
            match (d, w.as_str()) {
                (_, "RETURNS") => self.returns(),
                (_, "AS") => self.i += 1,
                (_, "LANGUAGE") => {
                    let lang = cst::unquote(&self.t.get(self.i + 1).map(|t| t.text.clone()).unwrap_or_default())
                        .to_uppercase();
                    match (d, lang.as_str()) {
                        (Dialect::Postgres, "SQL") => self.u.sql_body = true,
                        (Dialect::Postgres, "PLPGSQL") | (Dialect::Db2i, "SQL") => {}
                        _ => self.uncovered(format!("LANGUAGE {lang}")),
                    }
                    self.i += 2;
                }
                (Dialect::Tsql, "WITH") => self.tsql_with(),
                (Dialect::Tsql, "FOR") if self.up(1) == "REPLICATION" => {
                    self.uncovered("FOR REPLICATION".into());
                    self.i += 2;
                }
                (Dialect::Postgres | Dialect::Db2i, "CALLED") if self.phrase(&["CALLED", "ON", "NULL", "INPUT"]) => {}
                (Dialect::Postgres, "STRICT") => {
                    self.uncovered("STRICT (returns NULL on NULL input without running)".into());
                    self.i += 1;
                }
                (Dialect::Postgres, "SECURITY" | "EXTERNAL") => {
                    let at = self.i;
                    let k = if w == "EXTERNAL" { 1 } else { 0 };
                    if self.up(k) != "SECURITY" {
                        self.uncovered(format!("routine option {w}"));
                        self.i += 1;
                        continue;
                    }
                    match self.up(k + 1) {
                        "DEFINER" => self.security(Security::Owner, at, at + k + 2),
                        "INVOKER" => self.security(Security::Caller, at, at + k + 2),
                        other => self.uncovered(format!("SECURITY {other}")),
                    }
                    self.i += k + 2;
                }
                (Dialect::Postgres, "SET") => self.pg_set(),
                (Dialect::Postgres, "VOLATILE") => self.i += 1,
                (Dialect::Postgres, "STABLE" | "IMMUTABLE") => {
                    self.u.non_volatile = Some(w.clone());
                    self.i += 1;
                }
                (Dialect::Postgres, "COST" | "ROWS") => {
                    self.note("COST and ROWS (planner estimates only)");
                    self.i += 2;
                }
                (Dialect::Postgres, "PARALLEL") => {
                    if self.up(1) != "UNSAFE" {
                        self.uncovered(format!("PARALLEL {}", self.up(1)));
                    }
                    self.i += 2;
                }
                (Dialect::Postgres, "NOT") if self.up(1) == "LEAKPROOF" => self.i += 2,
                (Dialect::Db2i, "SPECIFIC") => {
                    self.i += 1;
                    let mut p = Parser::new(&self.t[self.i..], d);
                    if p.qname().is_ok() {
                        self.i += p.i;
                    }
                }
                (Dialect::Db2i, "NOT") if matches!(self.up(1), "DETERMINISTIC" | "FENCED" | "SECURED") => self.i += 2,
                (Dialect::Db2i, "DETERMINISTIC") => {
                    self.u.non_volatile = Some(w.clone());
                    self.i += 1;
                }
                (Dialect::Db2i, "FENCED") => self.i += 1,
                (Dialect::Db2i, "MODIFIES" | "READS" | "CONTAINS") => {
                    if w != "MODIFIES" {
                        // Checked against the body below.
                        self.u.non_volatile.get_or_insert(format!("{w} SQL DATA"));
                    }
                    self.i += if w == "CONTAINS" { 2 } else { 3 };
                }
                (Dialect::Db2i, "NO") if self.up(1) == "SQL" => {
                    self.u.non_volatile.get_or_insert("NO SQL".into());
                    self.i += 2;
                }
                (Dialect::Db2i, "NO") if self.up(1) == "EXTERNAL" => self.i += 3,
                (Dialect::Db2i, "EXTERNAL") if self.up(1) == "ACTION" => self.i += 2,
                (Dialect::Db2i, "INHERIT") if self.phrase(&["INHERIT", "SPECIAL", "REGISTERS"]) => {}
                (Dialect::Db2i, "OLD") if self.phrase(&["OLD", "SAVEPOINT", "LEVEL"]) => {}
                (Dialect::Db2i, "COMMIT") if self.phrase(&["COMMIT", "ON", "RETURN", "NO"]) => {}
                (Dialect::Db2i, "PROGRAM") if self.up(1) == "TYPE" => {
                    self.note("PROGRAM TYPE (the program object's form only)");
                    self.i += 3;
                }
                (Dialect::Db2i, "DYNAMIC") if self.up(1) == "RESULT" => {
                    let n = self.t.get(self.i + 3).map(|t| t.text.clone()).unwrap_or_default();
                    if n != "0" {
                        self.uncovered(format!("DYNAMIC RESULT SETS {n} (result sets returned to the caller)"));
                    }
                    self.i += 4;
                }
                (Dialect::Db2i, "SET") if self.up(1) == "OPTION" => {
                    if self.db2_set_option() {
                        usrprf = true;
                    }
                }
                _ => {
                    let what = self.text(self.i, self.i + 3);
                    self.uncovered(format!("routine option `{what}`"));
                    self.i += 1;
                }
            }
        }
        if d == Dialect::Db2i && !usrprf {
            self.u.security = Security::Naming;
        }
    }

    fn returns(&mut self) {
        let d = self.u.dialect;
        let r = self.r.clone();
        let toks = self.t;
        let mut i = self.i + 1;
        let u = &mut *self.u;
        if toks.get(i).is_some_and(|t| t.up == "TABLE") {
            u.kind = UnitKind::TableFn;
            i += 1;
            if toks.get(i).is_some_and(|t| t.kind == K::LParen) {
                let close = matching(toks, i);
                for c in split_commas(&toks[i + 1..close]) {
                    if let Some(n) = c.first() {
                        let mut p = Parser::new(&c[1..], d);
                        let ty = p.ty().ok().map(|t| Ty::from_type(&t, d, false));
                        u.ret_cols.push((cst::unquote(&n.text).to_lowercase(), ty));
                    }
                }
                i = close + 1;
            }
        } else if toks.get(i).is_some_and(|t| t.up == "NULL") {
            u.uncovered.push((r, "RETURNS NULL ON NULL INPUT".into()));
            i += 4;
        } else if toks.get(i).is_some_and(|t| t.up == "SETOF") {
            u.uncovered.push((r, "RETURNS SETOF".into()));
            i += 1;
        } else if toks.get(i).is_some_and(|t| t.kind == K::Var) {
            u.uncovered.push((r, "RETURNS @table TABLE (a multi-statement table function)".into()));
            i += 1;
            if toks.get(i).is_some_and(|t| t.up == "TABLE") {
                i += 1;
            }
            if toks.get(i).is_some_and(|t| t.kind == K::LParen) {
                i = matching(toks, i) + 1;
            }
        } else {
            let mut p = Parser::new(&toks[i..], d);
            match p.ty() {
                Ok(ty) => {
                    if ty.name == "TRIGGER" || ty.name == "VOID" || ty.name == "RECORD" {
                        u.uncovered.push((r, format!("RETURNS {}", ty.name)));
                    }
                    u.returns = Some(Ty::from_type(&ty, d, false));
                    i += p.i;
                }
                Err(_) => {
                    u.uncovered.push((r, "an unrecognised RETURNS clause".into()));
                    i += 1;
                }
            }
        }
        self.i = i;
    }

    /// T-SQL `WITH option [, option]...`.
    fn tsql_with(&mut self) {
        self.i += 1;
        loop {
            let at = self.i;
            let w = self.up(0).to_string();
            match w.as_str() {
                "EXECUTE" | "EXEC" if self.up(1) == "AS" => {
                    let who = self.up(2).to_string();
                    match who.as_str() {
                        "CALLER" => self.security(Security::Caller, at, at + 3),
                        "OWNER" => self.security(Security::Owner, at, at + 3),
                        "SELF" => self.uncovered(
                            "EXECUTE AS SELF (runs as whoever last created or altered the routine)".into(),
                        ),
                        _ => {
                            let name = self.t.get(at + 2).map(|t| t.text.clone()).unwrap_or_default();
                            self.uncovered(format!("EXECUTE AS {name} (runs as a named user)"));
                        }
                    }
                    self.i += 3;
                }
                "RECOMPILE" => {
                    self.note("WITH RECOMPILE (plan caching only)");
                    self.i += 1;
                }
                "ENCRYPTION" | "SCHEMABINDING" | "VIEW_METADATA" => {
                    self.note("WITH ENCRYPTION / SCHEMABINDING / VIEW_METADATA (no effect on results)");
                    self.i += 1;
                }
                "CALLED" if self.phrase(&["CALLED", "ON", "NULL", "INPUT"]) => {}
                "RETURNS" if self.up(1) == "NULL" => {
                    self.uncovered("RETURNS NULL ON NULL INPUT".into());
                    self.i += 5;
                }
                "INLINE" if self.up(2) == "OFF" => self.i += 3,
                _ => {
                    let what = self.text(at, at + 3);
                    self.uncovered(format!("routine option `WITH {what}`"));
                    self.i += 1;
                    // Skip to the next comma or the body.
                    while self.i < self.t.len()
                        && self.t[self.i].kind != K::Comma
                        && !matches!(self.up(0), "AS" | "FOR")
                    {
                        self.i += 1;
                    }
                }
            }
            if self.t.get(self.i).is_some_and(|t| t.kind == K::Comma) {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    /// PostgreSQL `SET name { TO | = } value [, ...]` or `SET name FROM CURRENT`.
    fn pg_set(&mut self) {
        self.u.sets_config = true;
        let name = self
            .t
            .get(self.i + 1)
            .map(|t| cst::unquote(&t.text).to_lowercase())
            .unwrap_or_default();
        let at = self.i;
        self.i += 2;
        if self.phrase(&["FROM", "CURRENT"]) {
            self.uncovered(format!("SET {name} FROM CURRENT (the value at creation time)"));
            return;
        }
        if matches!(self.up(0), "TO" | "=") {
            self.i += 1;
        }
        // Values: one token each (a leading `-` aside), separated by commas.
        loop {
            if self.t.get(self.i).is_some_and(|t| t.text == "-") {
                self.i += 1;
            }
            self.i += 1;
            if self.t.get(self.i).is_some_and(|t| t.kind == K::Comma) {
                self.i += 1;
            } else {
                break;
            }
        }
        let text = self.text(at, self.i);
        if name == "search_path" {
            self.note(&format!("{text} (unqualified names resolve through it)"));
            let a = asm::make(asm::DEFAULT_SCHEMA);
            if !self.u.assumptions.iter().any(|x| x.id == a.id) {
                self.u.assumptions.push(a);
            }
        } else if PG_PLAN_SETTINGS.contains(&name.as_str())
            || PG_PLAN_PREFIXES.iter().any(|p| name.starts_with(p))
        {
            self.note(&format!("{text} (planning, memory or logging only)"));
        } else {
            self.uncovered(format!("{text} (a setting in effect while the routine runs)"));
        }
    }

    /// Db2 for i `SET OPTION name = value [, ...]`; true if USRPRF was set.
    fn db2_set_option(&mut self) -> bool {
        self.i += 2;
        let mut usrprf = false;
        loop {
            let at = self.i;
            let name = self.up(0).to_string();
            if self.up(1) != "=" {
                self.uncovered("an unrecognised SET OPTION clause".into());
                self.i += 1;
                return usrprf;
            }
            self.i += 2;
            // `*NAME`, a word, a string or a number, maybe `LIB/NAME`.
            if self.up(0) == "*" {
                self.i += 1;
            }
            self.i += 1;
            while self.up(0) == "/" {
                self.i += 2;
            }
            let value = self.text(at + 2, self.i).replace("* ", "*").to_uppercase();
            match name.as_str() {
                // The transaction model reads the commit level (ir::interp).
                "COMMIT" => {}
                "USRPRF" => {
                    usrprf = true;
                    match value.as_str() {
                        "*OWNER" => self.security(Security::Owner, at, self.i),
                        "*USER" => self.security(Security::Caller, at, self.i),
                        _ => {
                            self.u.security = Security::Naming;
                            self.u.security_cite = Some((format!("USRPRF = {value}"), (self.t[at].off, self.t[at].off)));
                        }
                    }
                }
                // Only dynamic SQL, which the comparison does not cover.
                "DYNUSRPRF" => {}
                "SRTSEQ" if value == "*HEX" => {}
                n if DB2_BUILD_OPTIONS.contains(&n) => {
                    self.note("SET OPTION debug and compile settings (no effect on results)");
                }
                _ => self.uncovered(format!("SET OPTION {name} = {value}")),
            }
            if self.t.get(self.i).is_some_and(|t| t.kind == K::Comma) {
                self.i += 1;
            } else {
                return usrprf;
            }
        }
    }
}

/// Options after the parameter list, from token `i` of the header.
pub(super) fn options(r: &SyntaxNode, toks: &[Tok], i: usize, u: &mut Unit) {
    Opts { r, t: toks, i, u }.run();
}

/// Checks that need the body: labels that promise no writes, and
/// PostgreSQL procedures that cannot run transaction control.
pub(super) fn body_checks(r: &SyntaxNode, u: &mut Unit) {
    if let Some(label) = u.non_volatile.clone() {
        let writes = u.stmts.iter().any(|s| {
            matches!(
                s.ast,
                Ok(Stmt::Insert { .. } | Stmt::Update { .. } | Stmt::Delete { .. } | Stmt::Merge { .. })
            )
        });
        if writes {
            u.uncovered.push((
                r.clone(),
                format!("a routine labelled {label} that writes (the label promises no writes)"),
            ));
        } else if !label.ends_with("SQL DATA") && label != "NO SQL" {
            u.assumptions.push(asm::make(asm::ROUTINE_LABEL));
        }
    }
    let restricted = u.dialect == Dialect::Postgres
        && u.kind == UnitKind::Proc
        && (u.sets_config || u.security == Security::Owner);
    if restricted {
        let d = u.dialect;
        let txn = r.descendants().any(|n| {
            n.kind() == SyntaxKind::STATEMENT
                && matches!(stmt_kind(&n, d), StmtKind::TxnCommit | StmtKind::TxnRollback)
        });
        if txn {
            u.uncovered.push((
                r.clone(),
                "COMMIT or ROLLBACK in a procedure with SECURITY DEFINER or a SET clause (PostgreSQL raises invalid transaction termination)".into(),
            ));
        }
    }
}
