//! Binding inside one statement or expression: relations (tables, CTEs,
//! derived tables, pseudo tables), column references with SQL scoping
//! (innermost query first, set-operation branches apart), function calls
//! and their signatures, casts, and the ISNULL type hazard.
//!
//! The parser keeps expressions flat, so this works on the statement's
//! significant tokens with a paren-structure pre-pass.

use std::collections::HashSet;

use sqldiff_syntax::{Dialect, SyntaxKind};

use super::{Binder, CallArg, CallStyle, DeclKind, Issue, RefKind, Resolution, Target};
use crate::catalog::builtins::{self, Availability};
use crate::catalog::ddl::{self, OutputCol};
use crate::catalog::{CatalogView, DataType, Ident, Lookup, RoutineKind, Table, TableKind};
use crate::model::{ChangeClass, Severity};
use crate::tokens::{self, close_paren, split_commas, Tok};

/// A relation visible to column references.
#[derive(Debug, Clone)]
struct Source {
    alias: Option<String>,
    /// Key of the last name part (for `table.col` without alias).
    name: Option<String>,
    /// Key of the schema part, if written.
    schema: Option<String>,
    display: String,
    /// `None` when the columns cannot be listed.
    columns: Option<Vec<Col>>,
    /// Columns come from a catalog or definition, so a missing one is an error.
    authoritative: bool,
    group: usize,
    /// INSERT targets are not in scope for the inserted expressions.
    in_scope: bool,
    /// First token index the source is visible from (`ON CONFLICT`,
    /// `RETURNING`: the INSERT target is visible there, not in `VALUES`).
    from: usize,
    /// The relation itself failed to bind (issue index): its columns fail with it.
    failed: Option<usize>,
    /// Query group of a derived table's own query, which does not see the derived table.
    body: Option<usize>,
}

impl Source {
    fn visible_at(&self, at: usize) -> bool {
        self.in_scope && at >= self.from
    }
}

#[derive(Debug, Clone)]
struct Col {
    name: String,
    key: String,
    data_type: DataType,
    nullable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paren {
    Query,
    Call,
    Plain,
}

/// Paren structure of a token slice.
struct Shape {
    /// Query group of each token.
    group: Vec<usize>,
    /// Parent of each query group.
    parent: Vec<Option<usize>>,
    /// Whether the token sits directly at query level (not inside a call or plain parens).
    clause: Vec<bool>,
    /// Kind of the innermost paren around each token.
    inner: Vec<Option<Paren>>,
}

const SET_OPS: &[&str] = &["UNION", "EXCEPT", "INTERSECT", "MINUS"];

/// Words that end a relation item when no alias follows.
const CLAUSE_WORDS: &[&str] = &[
    "WHERE",
    "ON",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "OUTER",
    "SET",
    "GROUP",
    "ORDER",
    "HAVING",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "MINUS",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "FOR",
    "WITH",
    "USING",
    "WHEN",
    "VALUES",
    "SELECT",
    "RETURNING",
    "OUTPUT",
    "OPTION",
    "NATURAL",
    "APPLY",
    "PIVOT",
    "UNPIVOT",
    "WINDOW",
    "DEFAULT",
    "INTO",
    "TABLESAMPLE",
    "CONNECT",
    "START",
    "ORDINALITY",
    "LATERAL",
    "AS",
    "DO",
    "THEN",
    "END",
    "AND",
    "OR",
    "NOT",
    "OVERRIDING",
    "QUALIFY",
    "FROM",
];

fn shape(t: &[Tok]) -> Shape {
    let mut group = Vec::with_capacity(t.len());
    let mut parent: Vec<Option<usize>> = vec![None];
    let mut clause = Vec::with_capacity(t.len());
    let mut inner = Vec::with_capacity(t.len());
    // Stack of (paren kind, query group in force inside it).
    let mut stack: Vec<(Paren, usize)> = Vec::new();
    let mut cur = 0usize;
    for (i, x) in t.iter().enumerate() {
        match x.kind {
            SyntaxKind::L_PAREN => {
                group.push(cur);
                clause.push(stack.last().map_or(true, |s| s.0 == Paren::Query));
                inner.push(stack.last().map(|s| s.0));
                let next_query = t.get(i + 1).is_some_and(|n| {
                    n.is_any_kw(&[
                        "SELECT", "WITH", "VALUES", "INSERT", "UPDATE", "DELETE", "MERGE",
                    ])
                });
                let prev_name = i > 0
                    && (t[i - 1].is_name() || t[i - 1].kind == SyntaxKind::VARIABLE)
                    && !t[i - 1].is_any_kw(&[
                        "IN", "AS", "ON", "AND", "OR", "NOT", "EXISTS", "WHERE", "WHEN", "THEN",
                        "ELSE", "VALUES", "USING", "SET", "BY", "SELECT", "FROM", "JOIN", "ANY",
                        "ALL", "SOME", "RETURN", "IS", "LIKE", "BETWEEN", "INTO", "OVER", "FILTER",
                        "WITHIN", "LATERAL", "TABLE",
                    ]);
                let join_ctx = i > 0
                    && (t[i - 1].is_any_kw(&["FROM", "JOIN", "APPLY"])
                        || t[i - 1].is(SyntaxKind::COMMA)
                        || t[i - 1].is(SyntaxKind::L_PAREN));
                if next_query {
                    parent.push(Some(cur));
                    let g = parent.len() - 1;
                    stack.push((Paren::Query, cur));
                    cur = g;
                } else if join_ctx
                    && stack.last().map_or(true, |s| s.0 == Paren::Query)
                    && is_join_paren(t, i)
                {
                    // A parenthesized join stays at query level, in the same group.
                    stack.push((Paren::Query, cur));
                } else if prev_name {
                    stack.push((Paren::Call, cur));
                } else {
                    stack.push((Paren::Plain, cur));
                }
                continue;
            }
            SyntaxKind::R_PAREN => {
                if let Some((_, outer)) = stack.pop() {
                    cur = outer;
                }
                group.push(cur);
                clause.push(stack.last().map_or(true, |s| s.0 == Paren::Query));
                inner.push(stack.last().map(|s| s.0));
                continue;
            }
            _ => {}
        }
        let at_query = stack.last().map_or(true, |s| s.0 == Paren::Query);
        if at_query && x.is_any_kw(SET_OPS) {
            // A new branch: same parent, separate scope.
            let p = parent[cur];
            parent.push(p);
            cur = parent.len() - 1;
        }
        group.push(cur);
        clause.push(at_query);
        inner.push(stack.last().map(|s| s.0));
    }
    Shape {
        group,
        parent,
        clause,
        inner,
    }
}

impl Shape {
    /// Groups from `g` outwards.
    fn chain(&self, g: usize) -> Vec<usize> {
        let mut out = vec![g];
        let mut c = g;
        while let Some(Some(p)) = self.parent.get(c) {
            out.push(*p);
            c = *p;
        }
        out
    }
}

/// Pending relation before resolution.
struct Pending {
    parts: Vec<Ident>,
    span: (usize, usize),
    alias: Option<Ident>,
    group: usize,
    kind: PendingKind,
    head_target: bool,
    /// Token range of an INSERT / OUTPUT INTO column list.
    col_list: Option<(usize, usize)>,
    /// Column aliases: `AS s (a, b)`.
    alias_cols: Option<Vec<Ident>>,
    /// Query group of a derived table's own query.
    body: Option<usize>,
}

enum PendingKind {
    Named {
        insert_target: bool,
    },
    Derived(Option<Vec<OutputCol>>),
    Function,
    /// Db2 `FINAL TABLE (INSERT ...)`: the rows of the data change in this group.
    DataChange(usize),
}

struct Analysis {
    consumed: Vec<bool>,
    var_only: Vec<bool>,
    sources: Vec<Source>,
    outputs: HashSet<String>,
    ctes: Vec<(String, Option<Vec<OutputCol>>)>,
    /// Target source of each INSERT/UPDATE/DELETE/MERGE, by query group.
    targets: Vec<(usize, usize)>,
    /// ISNULL calls: (name token, args).
    isnull: Vec<(usize, Vec<(usize, usize)>)>,
    /// T-SQL `SELECT ... INTO #t`: the new table's name parts.
    select_into: Option<Vec<Ident>>,
    /// Where the relation scan resumes after a FROM list that holds a derived
    /// table or a parenthesized join: inside it, so the relations of the
    /// inner query or join are read too.
    resume: Option<usize>,
    /// PostgreSQL RETURNING: its query group, where it starts (byte offset)
    /// and the target source its `old` and `new` rows belong to.
    returning: Option<(usize, usize, usize)>,
}

impl Analysis {
    fn resume_at(&mut self, k: usize) {
        self.resume = Some(self.resume.map_or(k, |r| r.min(k)));
    }
}

impl<'a> Binder<'a> {
    /// Binds every reference in `t`.
    pub(crate) fn analyze(&mut self, t: &[Tok], stmt: (usize, usize)) {
        if t.is_empty() {
            return;
        }
        let sh = shape(t);
        let mut an = Analysis {
            consumed: vec![false; t.len()],
            var_only: vec![false; t.len()],
            sources: Vec::new(),
            outputs: HashSet::new(),
            ctes: Vec::new(),
            targets: Vec::new(),
            isnull: Vec::new(),
            select_into: None,
            resume: None,
            returning: None,
        };
        self.find_ctes(t, &sh, &mut an);
        let pending = self.find_relations(t, &sh, &mut an);
        self.resolve_relations(pending, t, stmt, &mut an);
        self.pseudo_sources(t, &mut an);
        self.target_columns(t, &sh, stmt, &mut an);
        self.mark_outputs(t, &sh, &mut an);
        self.mark_into(t, &sh, &mut an);
        self.generic(t, &sh, stmt, &mut an);
        self.isnull_hazards(t, stmt, &an);
        if let Some(parts) = an.select_into.clone() {
            self.create_from_select(&parts, t);
        }
    }

    // ---- CTEs ---------------------------------------------------------------

    fn find_ctes(&mut self, t: &[Tok], sh: &Shape, an: &mut Analysis) {
        let mut i = 0;
        while i < t.len() {
            if !(t[i].is_kw("WITH") && sh.clause[i]) {
                i += 1;
                continue;
            }
            let mut k = i + 1;
            if t.get(k).is_some_and(|x| x.is_kw("RECURSIVE")) {
                k += 1;
            }
            while let Some(n) = t.get(k).filter(|x| x.is_name()) {
                let mut j = k + 1;
                let mut explicit = None;
                if t.get(j).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
                    let c = close_paren(t, j);
                    explicit = Some(
                        split_commas(&t[j + 1..c])
                            .iter()
                            .filter_map(|e| e.first().filter(|x| x.is_name()))
                            .map(|x| OutputCol {
                                name: Some(x.ident()),
                                source: None,
                            })
                            .collect::<Vec<_>>(),
                    );
                    for m in j..=c.min(t.len() - 1) {
                        an.consumed[m] = true;
                    }
                    j = c + 1;
                }
                if !t.get(j).is_some_and(|x| x.is_kw("AS")) {
                    break;
                }
                j += 1;
                while t
                    .get(j)
                    .is_some_and(|x| x.is_any_kw(&["NOT", "MATERIALIZED"]))
                {
                    j += 1;
                }
                if !t.get(j).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
                    break;
                }
                let c = close_paren(t, j);
                let cols = explicit.or_else(|| ddl::select_outputs(&t[j + 1..c]));
                an.consumed[k] = true;
                an.consumed[j - 1] = true; // AS
                an.ctes.push((n.ident().key(self.d), cols));
                k = c + 1;
                if t.get(k).is_some_and(|x| x.is(SyntaxKind::COMMA)) {
                    k += 1;
                } else {
                    break;
                }
            }
            i = k.max(i + 1);
        }
    }

    // ---- relations ----------------------------------------------------------

    fn find_relations(&mut self, t: &[Tok], sh: &Shape, an: &mut Analysis) -> Vec<Pending> {
        let mut out = Vec::new();
        let head = first_dml(t);
        let head_word = head.map(|h| t[h].upper()).unwrap_or_default();
        let mut i = 0;
        while i < t.len() {
            let x = &t[i];
            if !sh.clause[i] || x.kind != SyntaxKind::IDENT {
                i += 1;
                continue;
            }
            let w = x.upper();
            let prev = i.checked_sub(1).map(|p| &t[p]);
            let g = sh.group[i];
            // DML heads: the statement's first, or one nested in parentheses (CTE, FINAL TABLE).
            let head_here = Some(i) == head || prev.is_some_and(|p| p.is(SyntaxKind::L_PAREN));
            match w.as_str() {
                "FROM" if !prev.is_some_and(|p| p.is_kw("DISTINCT")) => {
                    // T-SQL `DELETE FROM t`, `DELETE alias FROM t alias`: both are relations.
                    let after_delete = prev.is_some_and(|p| p.is_kw("DELETE"));
                    i = self.read_items(t, sh, i + 1, g, true, after_delete, false, an, &mut out);
                }
                "JOIN" | "APPLY" => {
                    i = self.read_items(t, sh, i + 1, g, false, false, false, an, &mut out);
                }
                "UPDATE" if head_here && !t.get(i + 1).is_some_and(|n| n.is_kw("SET")) => {
                    let mut k = i + 1;
                    if t.get(k).is_some_and(|n| n.is_kw("ONLY")) {
                        k += 1;
                    }
                    i = self.read_items(t, sh, k, g, false, true, false, an, &mut out);
                }
                "DELETE" if head_here => {
                    let mut k = i + 1;
                    if t.get(k).is_some_and(|n| n.is_kw("TOP")) {
                        k = skip_top(t, k);
                    }
                    if t.get(k).is_some_and(|n| n.is_kw("FROM")) {
                        k += 1;
                    }
                    if t.get(k).is_some_and(|n| n.is_kw("ONLY")) {
                        k += 1;
                    }
                    i = self.read_items(t, sh, k, g, false, true, false, an, &mut out);
                }
                "INSERT" if head_here => {
                    let mut k = i + 1;
                    if t.get(k).is_some_and(|n| n.is_kw("TOP")) {
                        k = skip_top(t, k);
                    }
                    if t.get(k).is_some_and(|n| n.is_kw("INTO")) {
                        k += 1;
                    }
                    i = self.read_items(t, sh, k, g, false, true, true, an, &mut out);
                }
                "MERGE" if head_here => {
                    let mut k = i + 1;
                    if t.get(k).is_some_and(|n| n.is_kw("TOP")) {
                        k = skip_top(t, k);
                    }
                    if t.get(k).is_some_and(|n| n.is_kw("INTO")) {
                        k += 1;
                    }
                    i = self.read_items(t, sh, k, g, false, true, false, an, &mut out);
                }
                "USING" if head_word == "MERGE" || head_word == "DELETE" => {
                    let list = head_word == "DELETE";
                    let is_cols = t.get(i + 1).is_some_and(|n| n.is(SyntaxKind::L_PAREN))
                        && !t
                            .get(i + 2)
                            .is_some_and(|n| n.is_any_kw(&["SELECT", "WITH", "VALUES"]));
                    if is_cols && head_word != "MERGE" {
                        i += 1;
                    } else {
                        i = self.read_items(t, sh, i + 1, g, list, false, false, an, &mut out);
                    }
                }
                "TRUNCATE" | "LOCK" if Some(i) == Some(0) => {
                    let mut k = i + 1;
                    if t.get(k).is_some_and(|n| n.is_kw("TABLE")) {
                        k += 1;
                    }
                    i = self.read_items(t, sh, k, g, true, false, false, an, &mut out);
                }
                "INTO"
                    if self.d == Dialect::Tsql
                        && t[..i].iter().enumerate().any(|(k, p)| {
                            p.is_kw("OUTPUT") && sh.group[k] == g && sh.clause[k]
                        }) =>
                {
                    // OUTPUT ... INTO target [(cols)]
                    i = self.read_items(t, sh, i + 1, g, false, false, true, an, &mut out);
                }
                "INTO"
                    if self.d == Dialect::Tsql
                        && head_word == "SELECT"
                        && t[..i].iter().all(|p| !p.is_kw("FROM")) =>
                {
                    // SELECT ... INTO #t FROM: creates a table.
                    let (parts, after) = tokens::qualified(t, i + 1);
                    if !parts.is_empty() {
                        for m in i + 1..after {
                            an.consumed[m] = true;
                        }
                        an.select_into = Some(parts);
                        i = after;
                    } else {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
            if let Some(r) = an.resume.take() {
                i = i.min(r);
            }
        }
        out
    }

    /// Reads relation items at `i`; returns the index after them.
    #[allow(clippy::too_many_arguments)]
    fn read_items(
        &mut self,
        t: &[Tok],
        sh: &Shape,
        mut i: usize,
        group: usize,
        list: bool,
        head_target: bool,
        insert_target: bool,
        an: &mut Analysis,
        out: &mut Vec<Pending>,
    ) -> usize {
        loop {
            while t.get(i).is_some_and(|x| x.is_any_kw(&["LATERAL", "ONLY"])) {
                i += 1;
            }
            let Some(x) = t.get(i) else { return i };
            let start = i;
            let kind;
            let mut parts = Vec::new();
            let mut span = (x.start, x.end);
            let mut body = None;
            if x.is(SyntaxKind::L_PAREN) {
                let c = close_paren(t, i);
                let inner = &t[i + 1..c];
                if inner
                    .first()
                    .is_some_and(|y| y.is_any_kw(&["SELECT", "WITH"]))
                {
                    kind = PendingKind::Derived(ddl::select_outputs(inner));
                    body = sh.group.get(i + 1).copied();
                    an.resume_at(i + 1);
                } else if inner.first().is_some_and(|y| y.is_kw("VALUES")) {
                    kind = PendingKind::Derived(None);
                    body = sh.group.get(i + 1).copied();
                    an.resume_at(i + 1);
                } else if is_join_paren(t, i) {
                    // Parenthesized join (pg_dump writes `FROM ((a JOIN b ON ...) JOIN c ON ...)`):
                    // its first item is read here, the JOINs inside by the scan that resumes after it.
                    let after_first = self.read_items(
                        t,
                        sh,
                        i + 1,
                        group,
                        false,
                        head_target,
                        insert_target,
                        an,
                        out,
                    );
                    an.resume_at(after_first);
                    let mut k = c + 1;
                    // `(a JOIN b) AS j` hides the inner names behind `j`, whose columns are not listed.
                    let alias_at = if t.get(k).is_some_and(|y| y.is_kw("AS")) {
                        an.consumed[k] = true;
                        k + 1
                    } else {
                        k
                    };
                    if let Some(y) = t.get(alias_at).filter(|y| {
                        y.kind == SyntaxKind::QUOTED_IDENT
                            || (y.kind == SyntaxKind::IDENT && !y.is_any_kw(CLAUSE_WORDS))
                    }) {
                        an.consumed[alias_at] = true;
                        out.push(Pending {
                            parts: Vec::new(),
                            span: (x.start, x.end),
                            alias: Some(y.ident()),
                            group,
                            kind: PendingKind::Derived(None),
                            head_target: false,
                            col_list: None,
                            alias_cols: None,
                            body: None,
                        });
                        k = alias_at + 1;
                    }
                    if list
                        && t.get(k).is_some_and(|y| y.is(SyntaxKind::COMMA))
                        && sh.group.get(k) == Some(&group)
                    {
                        i = k + 1;
                        continue;
                    }
                    return k;
                } else {
                    return c + 1;
                }
                i = c + 1;
            } else if x.is_any_kw(&["FINAL", "NEW", "OLD"])
                && t.get(i + 1).is_some_and(|y| y.is_kw("TABLE"))
            {
                let open = i + 2;
                if !t.get(open).is_some_and(|y| y.is(SyntaxKind::L_PAREN)) {
                    return open;
                }
                let c = close_paren(t, open);
                // Alias after the parenthesis; the inner statement is read by the caller's loop.
                let mut alias = None;
                let mut k = c + 1;
                if t.get(k).is_some_and(|y| y.is_kw("AS")) {
                    an.consumed[k] = true;
                    k += 1;
                }
                if let Some(y) = t
                    .get(k)
                    .filter(|y| y.is_name() && !y.is_any_kw(CLAUSE_WORDS))
                {
                    alias = Some(y.ident());
                    an.consumed[k] = true;
                }
                out.push(Pending {
                    parts: Vec::new(),
                    span: (x.start, x.end),
                    alias,
                    group,
                    kind: PendingKind::DataChange(sh.group.get(open + 1).copied().unwrap_or(group)),
                    head_target: false,
                    col_list: None,
                    alias_cols: None,
                    body: None,
                });
                return open + 1;
            } else if x.is_kw("TABLE") && t.get(i + 1).is_some_and(|y| y.is(SyntaxKind::L_PAREN)) {
                i = close_paren(t, i + 1) + 1;
                kind = PendingKind::Function;
            } else if x.is(SyntaxKind::VARIABLE) {
                // T-SQL table variable.
                parts.push(Ident::plain(x.text.clone()));
                an.consumed[i] = true;
                i += 1;
                kind = PendingKind::Named { insert_target };
            } else if x.is_name()
                && !(x.kind == SyntaxKind::IDENT
                    && x.is_any_kw(&["SET", "SELECT", "VALUES", "DEFAULT", "WHERE"]))
            {
                let (p, after) = tokens::qualified(t, i);
                parts = p;
                span = (x.start, t[after - 1].end);
                for m in i..after {
                    an.consumed[m] = true;
                }
                if t.get(after).is_some_and(|y| y.is(SyntaxKind::L_PAREN)) && !insert_target {
                    // Table function: the call is bound by the generic pass.
                    for m in i..after {
                        an.consumed[m] = false;
                    }
                    if self.d == Dialect::Db2i
                        && !(after == i + 1 && x.is_any_kw(&["UNNEST", "XMLTABLE", "JSON_TABLE"]))
                    {
                        let call = t[i..after]
                            .iter()
                            .map(|y| y.text.as_str())
                            .collect::<String>();
                        self.issue(Issue {
                            rule: "bind.table-function-form",
                            class: ChangeClass::Binding,
                            severity: Severity::Medium,
                            proven: false,
                            message: format!(
                                "`{call}(…)` is called in FROM without TABLE: IBM i documents a table function reference as `TABLE({call}(…)) AS name`."
                            ),
                            start: x.start,
                            end: t[close_paren(t, after).min(t.len() - 1)].end,
                            key: format!("table function {}", call.to_lowercase()),
                            assumptions: Vec::new(),
                        });
                    }
                    i = close_paren(t, after) + 1;
                    kind = PendingKind::Function;
                } else {
                    i = after;
                    kind = PendingKind::Named { insert_target };
                }
            } else {
                return i;
            }
            // T-SQL table hints may come before the alias: `t WITH (HOLDLOCK) AS x`.
            let mut k = i;
            if t.get(k).is_some_and(|y| y.is_kw("WITH"))
                && t.get(k + 1).is_some_and(|y| y.is(SyntaxKind::L_PAREN))
            {
                let c = close_paren(t, k + 1);
                for m in k..=c.min(t.len() - 1) {
                    an.consumed[m] = true;
                }
                k = c + 1;
            }
            // Alias.
            let mut alias = None;
            if t.get(k).is_some_and(|y| y.is_kw("AS")) && t.get(k + 1).is_some_and(Tok::is_name) {
                an.consumed[k] = true;
                k += 1;
                alias = Some(t[k].ident());
                an.consumed[k] = true;
                k += 1;
            } else if let Some(y) = t.get(k) {
                let is_alias = (y.kind == SyntaxKind::QUOTED_IDENT
                    || (y.kind == SyntaxKind::IDENT && !y.is_any_kw(CLAUSE_WORDS)))
                    && !t.get(k + 1).is_some_and(|z| z.is(SyntaxKind::DOT));
                if is_alias {
                    alias = Some(y.ident());
                    an.consumed[k] = true;
                    k += 1;
                }
            }
            // Column alias list `alias (a, b)`, unless it is an INSERT column list.
            let mut alias_cols = None;
            if alias.is_some()
                && !insert_target
                && t.get(k).is_some_and(|y| y.is(SyntaxKind::L_PAREN))
            {
                let c = close_paren(t, k);
                alias_cols = Some(
                    split_commas(&t[k + 1..c])
                        .iter()
                        .filter_map(|e| e.first().filter(|y| y.is_name()).map(Tok::ident))
                        .collect(),
                );
                for m in k..=c.min(t.len() - 1) {
                    an.consumed[m] = true;
                }
                k = c + 1;
            }
            // T-SQL table hints: WITH (NOLOCK)
            if t.get(k).is_some_and(|y| y.is_kw("WITH"))
                && t.get(k + 1).is_some_and(|y| y.is(SyntaxKind::L_PAREN))
            {
                let c = close_paren(t, k + 1);
                for m in k..=c.min(t.len() - 1) {
                    an.consumed[m] = true;
                }
                k = c + 1;
            }
            let _ = start;
            // INSERT INTO t (cols), OUTPUT ... INTO t (cols)
            let mut col_list = None;
            if insert_target
                && t.get(k).is_some_and(|y| y.is(SyntaxKind::L_PAREN))
                && !t
                    .get(k + 1)
                    .is_some_and(|y| y.is_any_kw(&["SELECT", "WITH", "VALUES"]))
            {
                let c = close_paren(t, k);
                for m in k..=c.min(t.len() - 1) {
                    an.consumed[m] = true;
                }
                col_list = Some((k + 1, c));
                k = c + 1;
            }
            out.push(Pending {
                parts,
                span,
                alias,
                group,
                kind,
                head_target,
                col_list,
                alias_cols,
                body,
            });
            i = k;
            if list
                && t.get(i).is_some_and(|y| y.is(SyntaxKind::COMMA))
                && sh.group.get(i) == Some(&group)
            {
                i += 1;
                continue;
            }
            return i;
        }
    }

    fn resolve_relations(
        &mut self,
        pending: Vec<Pending>,
        t: &[Tok],
        stmt: (usize, usize),
        an: &mut Analysis,
    ) {
        let mut alias_targets: Vec<(usize, String)> = Vec::new();
        let mut col_lists: Vec<(usize, (usize, usize))> = Vec::new();
        let mut renamed: Vec<(usize, Vec<Ident>)> = Vec::new();
        let mut data_changes: Vec<(usize, usize)> = Vec::new();
        let d = self.d;
        let aliases: Vec<String> = pending
            .iter()
            .filter_map(|p| p.alias.as_ref().map(|a| a.key(d)))
            .collect();
        for p in pending {
            if let Some(cols) = &p.alias_cols {
                renamed.push((an.sources.len(), cols.clone()));
            }
            let alias_key = p.alias.as_ref().map(|a| a.key(d));
            let name_key = p.parts.last().map(|n| n.key(d));
            let schema_key = (p.parts.len() >= 2).then(|| p.parts[p.parts.len() - 2].key(d));
            let display = p
                .parts
                .iter()
                .map(|x| x.text.clone())
                .collect::<Vec<_>>()
                .join(".");
            match p.kind {
                PendingKind::Named { insert_target } => {
                    // T-SQL `UPDATE x ... FROM t x`: the head names an alias.
                    if p.head_target
                        && p.parts.len() == 1
                        && alias_key.is_none()
                        && aliases.contains(name_key.as_ref().unwrap())
                    {
                        self.add_ref(
                            RefKind::Table,
                            display,
                            p.span,
                            stmt,
                            Resolution::Resolved(Target::Implicit),
                        );
                        // The target is the aliased source, resolved below.
                        alias_targets.push((p.group, name_key.clone().unwrap_or_default()));
                        continue;
                    }
                    let cte = (p.parts.len() == 1)
                        .then(|| {
                            an.ctes
                                .iter()
                                .find(|c| Some(&c.0) == name_key.as_ref())
                                .cloned()
                        })
                        .flatten();
                    let (columns, authoritative, failed) = if let Some((_, cols)) = cte {
                        self.add_ref(
                            RefKind::Table,
                            display.clone(),
                            p.span,
                            stmt,
                            Resolution::Resolved(Target::Implicit),
                        );
                        let known = cols
                            .as_ref()
                            .is_some_and(|c| c.iter().all(|o| o.name.is_some()));
                        (cols.map(|c| self.cols_from_outputs(&c)), known, None)
                    } else {
                        match self.table_ref(&p.parts, p.span, stmt) {
                            Some(tb) => {
                                let auth = tb.columns_known;
                                (auth.then(|| cols_of(d, &tb)), auth, None)
                            }
                            None => {
                                let failed = match self.out.refs.last().map(|r| &r.resolution) {
                                    Some(Resolution::Failed(i)) => Some(*i),
                                    _ => None,
                                };
                                (None, false, failed)
                            }
                        }
                    };
                    let idx = an.sources.len();
                    an.sources.push(Source {
                        alias: alias_key,
                        name: name_key,
                        schema: schema_key,
                        display,
                        columns,
                        authoritative,
                        group: p.group,
                        in_scope: !insert_target,
                        from: 0,
                        failed,
                        body: None,
                    });
                    if p.head_target && !an.targets.iter().any(|(g, _)| *g == p.group) {
                        an.targets.push((p.group, idx));
                    }
                    if let Some(cl) = p.col_list {
                        col_lists.push((idx, cl));
                    }
                }
                PendingKind::Derived(outs) => {
                    let known = outs
                        .as_ref()
                        .is_some_and(|c| c.iter().all(|o| o.name.is_some()));
                    let columns = outs.map(|c| self.cols_from_outputs(&c));
                    an.sources.push(Source {
                        alias: alias_key,
                        name: None,
                        schema: None,
                        display: "derived table".into(),
                        columns,
                        authoritative: known,
                        group: p.group,
                        in_scope: true,
                        from: 0,
                        failed: None,
                        body: p.body,
                    });
                }
                PendingKind::DataChange(inner) => {
                    data_changes.push((an.sources.len(), inner));
                    an.sources.push(Source {
                        alias: alias_key,
                        name: None,
                        schema: None,
                        display: "data-change table".into(),
                        columns: None,
                        authoritative: false,
                        group: p.group,
                        in_scope: true,
                        from: 0,
                        failed: None,
                        body: None,
                    });
                }
                PendingKind::Function => {
                    an.sources.push(Source {
                        alias: alias_key,
                        name: name_key,
                        schema: None,
                        display,
                        columns: None,
                        authoritative: false,
                        group: p.group,
                        in_scope: true,
                        from: 0,
                        failed: None,
                        body: None,
                    });
                }
            }
        }
        for (idx, cols) in renamed {
            let src = &mut an.sources[idx];
            src.columns = Some(
                cols.iter()
                    .map(|n| Col {
                        name: n.text.clone(),
                        key: n.key(d),
                        data_type: DataType::unknown(),
                        nullable: true,
                    })
                    .collect(),
            );
            src.authoritative = true;
            src.failed = None;
        }
        for (idx, inner) in data_changes {
            if let Some(&(_, ti)) = an.targets.iter().find(|(g, _)| *g == inner) {
                let (cols, auth, failed, disp) = {
                    let s = &an.sources[ti];
                    (
                        s.columns.clone(),
                        s.authoritative,
                        s.failed,
                        s.display.clone(),
                    )
                };
                let src = &mut an.sources[idx];
                src.columns = cols;
                src.authoritative = auth;
                src.failed = failed;
                src.display = disp;
            }
        }
        // Resolve deferred alias targets.
        for (g, key) in alias_targets {
            if let Some(idx) = an
                .sources
                .iter()
                .position(|s| s.alias.as_deref() == Some(key.as_str()))
            {
                if !an.targets.iter().any(|(x, _)| *x == g) {
                    an.targets.push((g, idx));
                }
            }
        }
        for (idx, (s, e)) in col_lists {
            for item in split_commas(&t[s..e]) {
                if let Some(n) = item.iter().rev().find(|y| y.is_name()) {
                    let n = n.clone();
                    self.target_column(&n, idx, stmt, an);
                }
            }
        }
    }

    fn target_of(&self, an: &Analysis, g: usize) -> Option<usize> {
        an.targets
            .iter()
            .find(|(x, _)| *x == g)
            .or(an.targets.first())
            .map(|(_, i)| *i)
    }

    fn cols_from_outputs(&self, outs: &[OutputCol]) -> Vec<Col> {
        outs.iter()
            .filter_map(|o| o.name.as_ref())
            .map(|n| Col {
                name: n.text.clone(),
                key: n.key(self.d),
                data_type: DataType::unknown(),
                nullable: true,
            })
            .collect()
    }

    /// Looks a table up in the artifact's own objects, then the catalog, and
    /// records the reference. Returns the table when found.
    pub(crate) fn table_ref(
        &mut self,
        parts: &[Ident],
        span: (usize, usize),
        stmt: (usize, usize),
    ) -> Option<Table> {
        let d = self.d;
        let display = parts
            .iter()
            .map(|p| p.text.clone())
            .collect::<Vec<_>>()
            .join(".");
        // A PostgreSQL trigger function's transition table, or `inserted` /
        // `deleted` in a T-SQL trigger: the rows of the trigger's table.
        if let (Some(tr), [p]) = (&self.trigger, parts) {
            let key = p.key(d);
            let image = if tr.pg_function {
                tr.referencing.iter().any(|(a, t)| *t && *a == key)
            } else {
                d == Dialect::Tsql
                    && !tr.table.is_empty()
                    && (key == "inserted" || key == "deleted")
            };
            if image {
                let tb = self.lookup_table_silent(&tr.table.clone());
                let res = match &tb {
                    Some(tb) => Resolution::Resolved(Target::Table(tb.display())),
                    None => Resolution::Resolved(Target::Implicit),
                };
                self.add_ref(RefKind::Table, display, span, stmt, res);
                return tb;
            }
        }
        let local = CatalogView::new(d, &[&self.local]);
        if let Lookup::Found(tb) = local.table(parts) {
            let tb = tb.clone();
            self.add_ref(
                RefKind::Table,
                display,
                span,
                stmt,
                Resolution::Resolved(Target::Table(tb.display())),
            );
            return Some(tb);
        }
        let first = parts.first().map(|p| p.text.as_str()).unwrap_or("");
        let temp = (d == Dialect::Tsql && (first.starts_with('#') || first.starts_with('@')))
            || (d == Dialect::Db2i && parts.len() == 2 && parts[0].key(d) == "SESSION");
        if temp {
            let id = self.assume(
                format!("ASM-BIND-TEMP:{}", display.to_lowercase()),
                format!("Temporary table `{display}` exists when this runs (created by the caller or earlier in the session)"),
            );
            self.add_ref(RefKind::Table, display, span, stmt, Resolution::Assumed(id));
            return None;
        }
        if d == Dialect::Db2i
            && parts.len() == 2
            && parts[0].key(d) == "SYSIBM"
            && parts[1].key(d) == "SYSDUMMY1"
        {
            // Every Db2 for i system has it: one row, column IBMREQD CHAR(1).
            let mut tb = Table::new(Some("SYSIBM".into()), "SYSDUMMY1", TableKind::View);
            tb.columns.push(crate::catalog::Column {
                name: "IBMREQD".into(),
                data_type: DataType::parse_str("CHAR(1)"),
                nullable: false,
                identity: false,
                has_default: false,
                generated: false,
                collation: None,
            });
            self.add_ref(
                RefKind::Table,
                display,
                span,
                stmt,
                Resolution::Resolved(Target::Table(tb.display())),
            );
            return Some(tb);
        }
        let view = self.input.view;
        match view.table(parts) {
            Lookup::Found(tb) => {
                let tb = tb.clone();
                self.add_ref(
                    RefKind::Table,
                    display,
                    span,
                    stmt,
                    Resolution::Resolved(Target::Table(tb.display())),
                );
                Some(tb)
            }
            Lookup::NoSchema(s) if view.tables_authoritative() => {
                let msg = format!(
                    "`{display}` is qualified with schema `{s}`, which does not exist in the {} catalog ({}); the statement fails with an unknown-object error.",
                    d.display_name(),
                    view.origin()
                );
                self.fail(
                    RefKind::Table,
                    display.clone(),
                    span,
                    stmt,
                    "bind.unknown-schema",
                    msg,
                    format!("schema {}", s.to_lowercase()),
                    Vec::new(),
                );
                None
            }
            Lookup::NotFound if view.tables_authoritative() => {
                let msg = format!(
                    "Table `{display}` does not exist in the {} catalog ({}).",
                    d.display_name(),
                    view.origin()
                );
                self.fail(
                    RefKind::Table,
                    display.clone(),
                    span,
                    stmt,
                    "bind.unknown-table",
                    msg,
                    format!("table {}", display.to_lowercase()),
                    Vec::new(),
                );
                None
            }
            Lookup::OutOfScope => {
                let id = self.assume(
                    format!("ASM-BIND-TABLE:{}", display.to_lowercase()),
                    format!("Cross-database or remote object `{display}` exists with the shape this code expects"),
                );
                self.add_ref(RefKind::Table, display, span, stmt, Resolution::Assumed(id));
                None
            }
            _ => {
                let id = self.assume(
                    format!("ASM-BIND-TABLE:{}", display.to_lowercase()),
                    format!("Table `{display}` exists with the columns this code uses (no catalog describes it)"),
                );
                self.add_ref(RefKind::Table, display, span, stmt, Resolution::Assumed(id));
                None
            }
        }
    }

    /// Pseudo tables: T-SQL `inserted`/`deleted` (triggers, OUTPUT),
    /// PostgreSQL `excluded` (ON CONFLICT).
    fn pseudo_sources(&mut self, t: &[Tok], an: &mut Analysis) {
        let d = self.d;
        let first_target = an.targets.first().map(|(_, i)| *i);
        let target_cols = first_target
            .and_then(|i| an.sources.get(i))
            .map(|s| (s.columns.clone(), s.authoritative));
        let add = |an: &mut Analysis, name: &str, cols: Option<Vec<Col>>, auth: bool| {
            an.sources.push(Source {
                alias: None,
                name: Some(Ident::plain(name).key(d)),
                schema: None,
                display: name.to_string(),
                columns: cols,
                authoritative: auth,
                group: 0,
                in_scope: false,
                from: 0,
                failed: None,
                body: None,
            });
        };
        if d == Dialect::Tsql {
            if let Some(tr) = self.trigger.clone() {
                let view = self.input.view;
                let local = CatalogView::new(d, &[&self.local]);
                let tb = match local.table(&tr.table) {
                    Lookup::Found(x) => Some(x.clone()),
                    _ => match view.table(&tr.table) {
                        Lookup::Found(x) => Some(x.clone()),
                        _ => None,
                    },
                };
                let cols = tb.as_ref().map(|x| cols_of(d, x));
                let auth = cols.is_some();
                add(an, "inserted", cols.clone(), auth);
                add(an, "deleted", cols, auth);
            } else if t.iter().any(|x| x.is_kw("OUTPUT")) {
                let (cols, auth) = target_cols.clone().unwrap_or((None, false));
                add(an, "inserted", cols.clone(), auth);
                add(an, "deleted", cols, auth);
            }
        }
        // An INSERT target becomes visible after its VALUES / query: in
        // RETURNING and (PostgreSQL) from ON CONFLICT on. PostgreSQL rejects a
        // target column in VALUES ("cannot be referenced from this part of the
        // query"), so a PL/pgSQL variable of that name is not ambiguous there.
        let reveal = |an: &mut Analysis, i: usize, at: usize| {
            let s = &mut an.sources[i];
            if !s.in_scope {
                s.in_scope = true;
                s.from = at;
            } else if s.from > 0 {
                s.from = s.from.min(at);
            }
        };
        if let Some(r) = t.iter().position(|x| x.is_kw("RETURNING")) {
            for (_, i) in an.targets.clone() {
                reveal(an, i, r);
            }
            // PostgreSQL 18: `old` and `new` name the row before and after the
            // change in RETURNING (a trigger function's NEW and OLD are its own).
            let pg_trigger = self.trigger.as_ref().is_some_and(|tr| tr.pg_function);
            if d == Dialect::Postgres && !pg_trigger {
                let g = shape(t).group[r];
                let ti = an
                    .targets
                    .iter()
                    .find(|(tg, _)| *tg == g)
                    .or(an.targets.first())
                    .map(|(_, i)| *i);
                if let Some(i) = ti {
                    an.returning = Some((g, t[r].start, i));
                }
            }
        }
        if d == Dialect::Postgres {
            if let Some(c) = t.iter().position(|x| x.is_kw("CONFLICT")) {
                let (cols, auth) = target_cols.unwrap_or((None, false));
                add(an, "excluded", cols, auth);
                if let Some(i) = first_target {
                    reveal(an, i, c);
                }
            }
        }
    }

    /// INSERT column lists, UPDATE/MERGE SET targets and ON CONFLICT columns
    /// name columns of the target.
    fn target_columns(&mut self, t: &[Tok], sh: &Shape, stmt: (usize, usize), an: &mut Analysis) {
        if an.targets.is_empty() {
            return;
        }
        let mut i = 0;
        while i < t.len() {
            let x = &t[i];
            if !sh.clause[i] || an.consumed[i] && !x.is(SyntaxKind::L_PAREN) {
                i += 1;
                continue;
            }
            // INSERT target (cols) / MERGE ... INSERT (cols) / ON CONFLICT (cols)
            let Some(ti) = self.target_of(an, sh.group[i]) else {
                i += 1;
                continue;
            };
            let list_open = if x.is(SyntaxKind::L_PAREN) {
                let prev_insert = i > 0 && t[i - 1].is_kw("INSERT");
                let prev_conflict = i > 0 && t[i - 1].is_kw("CONFLICT");
                let not_query = !t
                    .get(i + 1)
                    .is_some_and(|n| n.is_any_kw(&["SELECT", "WITH", "VALUES"]));
                not_query && (prev_insert || prev_conflict) && !an.consumed[i]
            } else {
                false
            };
            if list_open {
                let c = close_paren(t, i);
                for item in split_commas(&t[i + 1..c]) {
                    if let Some(n) = item.iter().rev().find(|y| y.is_name()) {
                        let pos = t.iter().position(|y| y.start == n.start).unwrap_or(i);
                        an.consumed[pos] = true;
                        self.target_column(n, ti, stmt, an);
                    }
                }
                for m in i..=c.min(t.len() - 1) {
                    an.consumed[m] = true;
                }
                i = c + 1;
                continue;
            }
            if x.is_kw("SET")
                && i > 0
                && (t[i - 1].is_kw("UPDATE")
                    || an.consumed[i - 1]
                    || t[i - 1].is(SyntaxKind::R_PAREN)
                    || first_dml(t).is_some_and(|h| t[h].is_kw("UPDATE")))
            {
                let g = sh.group[i];
                let end = (i + 1..t.len())
                    .find(|&k| {
                        sh.group[k] == g
                            && sh.clause[k]
                            && t[k].is_any_kw(&[
                                "WHERE",
                                "FROM",
                                "RETURNING",
                                "OUTPUT",
                                "WHEN",
                                "OPTION",
                            ])
                    })
                    .unwrap_or(t.len());
                let mut k = i + 1;
                let mut item_start = true;
                let mut depth = 0usize;
                while k < end {
                    match t[k].kind {
                        SyntaxKind::L_PAREN => depth += 1,
                        SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
                        SyntaxKind::COMMA if depth == 0 => {
                            item_start = true;
                            k += 1;
                            continue;
                        }
                        _ => {}
                    }
                    if item_start && depth == 0 {
                        item_start = false;
                        // target [, (a, b)] = ...
                        if t[k].is(SyntaxKind::L_PAREN) {
                            let c = close_paren(t, k);
                            for (m, n) in t.iter().enumerate().take(c).skip(k + 1) {
                                if n.is_name() {
                                    an.consumed[m] = true;
                                    self.target_column(n, ti, stmt, an);
                                }
                            }
                            k = c + 1;
                            continue;
                        }
                        if t[k].is_name() {
                            let (parts, after) = tokens::qualified(t, k);
                            if t.get(after)
                                .is_some_and(|y| y.is_op("=") || y.is_op("+=") || y.is_op("-="))
                            {
                                for m in k..after {
                                    an.consumed[m] = true;
                                }
                                let last = t[after - 1].clone();
                                let _ = parts;
                                self.target_column(&last, ti, stmt, an);
                                k = after;
                                continue;
                            }
                        }
                    }
                    k += 1;
                }
                i = end;
                continue;
            }
            i += 1;
        }
    }

    fn target_column(&mut self, n: &Tok, ti: usize, stmt: (usize, usize), an: &Analysis) {
        let src = &an.sources[ti];
        if let Some(f) = src.failed {
            self.add_ref(
                RefKind::Column,
                n.text.clone(),
                (n.start, n.end),
                stmt,
                Resolution::Failed(f),
            );
            return;
        }
        let key = n.ident().key(self.d);
        match &src.columns {
            Some(cols) => match cols.iter().find(|c| c.key == key) {
                Some(c) => {
                    let target = Target::Column {
                        table: src.display.clone(),
                        column: c.name.clone(),
                        data_type: c.data_type.clone(),
                        nullable: c.nullable,
                    };
                    self.add_ref(
                        RefKind::Column,
                        n.text.clone(),
                        (n.start, n.end),
                        stmt,
                        Resolution::Resolved(target),
                    );
                }
                None if src.authoritative => {
                    let disp = src.display.clone();
                    self.unknown_column_in(&n.text, &disp, (n.start, n.end), stmt);
                }
                None => self.assume_column(n, stmt),
            },
            None => self.assume_column(n, stmt),
        }
    }

    fn assume_column(&mut self, n: &Tok, stmt: (usize, usize)) {
        let id = self.assume(
            format!("ASM-BIND-COLUMN:{}", n.ident().key(self.d).to_lowercase()),
            format!(
                "Column `{}` exists in the table this code reads or writes",
                n.text
            ),
        );
        self.add_ref(
            RefKind::Column,
            n.text.clone(),
            (n.start, n.end),
            stmt,
            Resolution::Assumed(id),
        );
    }

    pub(crate) fn unknown_column_in(
        &mut self,
        col: &str,
        table: &str,
        span: (usize, usize),
        stmt: (usize, usize),
    ) {
        let view = self.input.view;
        let msg = format!(
            "`{table}` has no column `{col}` in the catalog ({}); the statement fails with an unknown-column error every time it runs.",
            view.origin()
        );
        let key = format!("column {}.{}", table.to_lowercase(), col.to_lowercase());
        self.fail(
            RefKind::Column,
            col.to_string(),
            span,
            stmt,
            "bind.unknown-column",
            msg,
            key,
            Vec::new(),
        );
    }

    /// Select-list aliases: `expr AS x`, `expr x`, T-SQL `x = expr`.
    fn mark_outputs(&mut self, t: &[Tok], sh: &Shape, an: &mut Analysis) {
        for s in 0..t.len() {
            if !(t[s].is_kw("SELECT") && sh.clause[s]) {
                continue;
            }
            let g = sh.group[s];
            let mut i = s + 1;
            while t.get(i).is_some_and(|x| x.is_any_kw(&["DISTINCT", "ALL"])) {
                i += 1;
            }
            if t.get(i).is_some_and(|x| x.is_kw("TOP")) {
                i = skip_top(t, i);
            }
            let end = (i..t.len())
                .find(|&k| {
                    (sh.group[k] == g
                        && sh.clause[k]
                        && t[k].is_any_kw(&[
                            "FROM",
                            "INTO",
                            "WHERE",
                            "GROUP",
                            "HAVING",
                            "ORDER",
                            "UNION",
                            "EXCEPT",
                            "INTERSECT",
                            "LIMIT",
                            "OFFSET",
                            "FETCH",
                            "FOR",
                            "WINDOW",
                        ]))
                        || (sh.group[k] != g && sh.chain(sh.group[k]).len() < sh.chain(g).len())
                        || t[k].is(SyntaxKind::SEMICOLON)
                })
                .unwrap_or(t.len());
            let mut item = i;
            let mut depth = 0usize;
            for k in i..=end {
                let at_end = k == end;
                if !at_end {
                    match t[k].kind {
                        SyntaxKind::L_PAREN => depth += 1,
                        SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
                if at_end || (t[k].is(SyntaxKind::COMMA) && depth == 0) {
                    self.mark_output_item(t, item, k, an);
                    item = k + 1;
                }
            }
        }
    }

    fn mark_output_item(&self, t: &[Tok], s: usize, e: usize, an: &mut Analysis) {
        if e <= s {
            return;
        }
        let n = e - s;
        let it = &t[s..e];
        // T-SQL `alias = expr` (not `@v = expr`).
        if self.d == Dialect::Tsql && n >= 3 && it[0].is_name() && it[1].is_op("=") {
            an.consumed[s] = true;
            an.outputs.insert(it[0].ident().key(self.d));
            return;
        }
        if n >= 2 && it[n - 2].is_kw("AS") && it[n - 1].is_name() {
            an.consumed[e - 2] = true;
            an.consumed[e - 1] = true;
            an.outputs.insert(it[n - 1].ident().key(self.d));
            return;
        }
        let trailing = n >= 2
            && (it[n - 1].kind == SyntaxKind::QUOTED_IDENT
                || (it[n - 1].kind == SyntaxKind::IDENT && !self.is_keyword(&it[n - 1].text)))
            && matches!(
                it[n - 2].kind,
                SyntaxKind::IDENT
                    | SyntaxKind::QUOTED_IDENT
                    | SyntaxKind::R_PAREN
                    | SyntaxKind::NUMBER
                    | SyntaxKind::STRING
                    | SyntaxKind::VARIABLE
            )
            && !it[n - 2].is_any_kw(&[
                "CASE", "WHEN", "THEN", "ELSE", "AND", "OR", "NOT", "IS", "LIKE", "IN", "BETWEEN",
                "DISTINCT", "COLLATE", "INTERVAL", "AT", "ZONE",
            ]);
        if trailing {
            an.consumed[e - 1] = true;
            an.outputs.insert(it[n - 1].ident().key(self.d));
        }
    }

    /// `INTO` targets that must be variables (PL/pgSQL, Db2 SELECT/VALUES/FETCH INTO, RETURNING INTO).
    fn mark_into(&mut self, t: &[Tok], sh: &Shape, an: &mut Analysis) {
        if self.d == Dialect::Tsql {
            return;
        }
        for i in 0..t.len() {
            if !(t[i].is_kw("INTO") && sh.clause[i]) {
                continue;
            }
            let prev = i.checked_sub(1).map(|p| &t[p]);
            if prev.is_some_and(|p| p.is_any_kw(&["INSERT", "MERGE"]))
                || an.consumed.get(i + 1).copied().unwrap_or(false)
            {
                continue;
            }
            let mut k = i + 1;
            if t.get(k).is_some_and(|x| x.is_kw("STRICT")) {
                k += 1;
            }
            let g = sh.group[i];
            while k < t.len() {
                let x = &t[k];
                if sh.group[k] == g
                    && x.is_any_kw(&[
                        "FROM", "WHERE", "GROUP", "ORDER", "USING", "UNION", "LIMIT", "FETCH",
                        "FOR", "WITH", "HAVING", "OFFSET",
                    ])
                    || x.is(SyntaxKind::SEMICOLON)
                {
                    break;
                }
                an.var_only[k] = true;
                k += 1;
            }
        }
    }

    // ---- the generic pass ---------------------------------------------------

    fn generic(&mut self, t: &[Tok], sh: &Shape, stmt: (usize, usize), an: &mut Analysis) {
        let mut i = 0;
        while i < t.len() {
            if an.consumed[i] {
                i += 1;
                continue;
            }
            let x = &t[i];
            match x.kind {
                SyntaxKind::VARIABLE => {
                    let x = x.clone();
                    self.variable_token(&x, stmt);
                    i += 1;
                }
                // T-SQL `geography::Point(...)`, `hierarchyid::GetRoot()`: a static
                // method of a CLR type, not a PostgreSQL cast.
                SyntaxKind::OP if x.text == "::" && self.d == Dialect::Tsql => {
                    i += 2;
                }
                SyntaxKind::OP if x.text == "::" => {
                    i = self.type_after(t, i + 1, stmt, an);
                }
                SyntaxKind::IDENT if x.is_kw("AS") => {
                    // CAST(x AS type) and other `AS name` forms.
                    let in_call = sh.inner.get(i).copied().flatten() == Some(Paren::Call);
                    if in_call {
                        i = self.type_after(t, i + 1, stmt, an);
                    } else {
                        if t.get(i + 1).is_some_and(Tok::is_name) {
                            an.consumed[i + 1] = true;
                        }
                        i += 1;
                    }
                }
                SyntaxKind::IDENT if x.is_kw("COLLATE") => {
                    i += 2;
                }
                // PostgreSQL `OPERATOR(schema.op)`: an operator name, not a call.
                SyntaxKind::IDENT
                    if self.d == Dialect::Postgres
                        && x.is_kw("OPERATOR")
                        && t.get(i + 1).is_some_and(|y| y.is(SyntaxKind::L_PAREN)) =>
                {
                    i = close_paren(t, i + 1) + 1;
                }
                // T-SQL query hints `OPTION (RECOMPILE, LABEL = '...', OPTIMIZE FOR (@p = 1))`:
                // hint words, not references; only the variables in them are.
                SyntaxKind::IDENT
                    if self.d == Dialect::Tsql
                        && x.is_kw("OPTION")
                        && sh.clause[i]
                        && t.get(i + 1).is_some_and(|y| y.is(SyntaxKind::L_PAREN)) =>
                {
                    let c = close_paren(t, i + 1);
                    for v in t[i + 1..c.min(t.len())]
                        .iter()
                        .filter(|y| y.is(SyntaxKind::VARIABLE))
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        self.variable_token(&v, stmt);
                    }
                    i = c + 1;
                }
                // Db2 `a CONCAT b`: the infix form of `||`.
                SyntaxKind::IDENT
                    if self.d == Dialect::Db2i
                        && x.is_kw("CONCAT")
                        && !t.get(i + 1).is_some_and(|y| y.is(SyntaxKind::L_PAREN)) =>
                {
                    i += 1;
                }
                // `ON CONFLICT ON CONSTRAINT name` names a constraint.
                SyntaxKind::IDENT
                    if x.is_kw("CONSTRAINT")
                        && i >= 2
                        && t[i - 1].is_kw("ON")
                        && t[i - 2].is_kw("CONFLICT") =>
                {
                    i += 2;
                }
                SyntaxKind::IDENT
                    if x.is_kw("FOR")
                        && i >= 2
                        && t[i - 1].is_kw("VALUE")
                        && t[i - 2].is_any_kw(&["NEXT", "PREVIOUS"]) =>
                {
                    // NEXT VALUE FOR seq
                    let (_, after) = tokens::qualified(t, i + 1);
                    i = after.max(i + 1);
                }
                SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT
                    if self.d == Dialect::Tsql && t.get(i + 1).is_some_and(|y| y.is_op("::")) =>
                {
                    // The type of a static method call; the method follows `::`.
                    i += 1;
                }
                SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT => {
                    i = self.name_at(t, sh, i, stmt, an);
                }
                _ => i += 1,
            }
        }
    }

    /// Consumes a type after `::` or `AS` and checks it.
    fn type_after(
        &mut self,
        t: &[Tok],
        mut k: usize,
        stmt: (usize, usize),
        an: &mut Analysis,
    ) -> usize {
        let start = k;
        while let Some(x) = t.get(k) {
            if x.is_name()
                && (k == start
                    || x.is_any_kw(&[
                        "VARYING",
                        "PRECISION",
                        "WITH",
                        "WITHOUT",
                        "TIME",
                        "ZONE",
                        "LOCAL",
                    ])
                    || t[k - 1].is_any_kw(&[
                        "DOUBLE",
                        "CHARACTER",
                        "NATIONAL",
                        "WITH",
                        "WITHOUT",
                        "TIME",
                        "LOCAL",
                    ]))
            {
                an.consumed[k] = true;
                k += 1;
            } else if x.is(SyntaxKind::DOT) && k > start {
                k += 1;
            } else if x.is(SyntaxKind::L_PAREN)
                && k > start
                && t.get(k + 1)
                    .is_some_and(|y| y.is(SyntaxKind::NUMBER) || y.is_kw("MAX"))
            {
                let c = close_paren(t, k);
                for m in k..=c.min(t.len() - 1) {
                    an.consumed[m] = true;
                }
                k = c + 1;
            } else if x.is(SyntaxKind::L_BRACKET) || x.is(SyntaxKind::R_BRACKET) {
                k += 1;
            } else {
                break;
            }
        }
        let k = k.min(t.len());
        if k > start {
            let ty = DataType::parse(&tokens::texts(&t[start..k]));
            self.check_type(&ty, (t[start].start, t[k - 1].end), stmt);
        }
        k.max(start)
    }

    /// A name or qualified name at `i`: call or reference.
    fn name_at(
        &mut self,
        t: &[Tok],
        sh: &Shape,
        i: usize,
        stmt: (usize, usize),
        an: &mut Analysis,
    ) -> usize {
        let (parts, after) = if self.d == Dialect::Db2i {
            tokens::qualified(t, i)
        } else {
            tokens::qualified_dot(t, i)
        };
        let span = (t[i].start, t[after - 1].end);
        let next = t.get(after);
        // `t.*`
        if next.is_some_and(|n| n.is(SyntaxKind::DOT))
            && t.get(after + 1).is_some_and(|n| n.is_op("*"))
        {
            self.qualifier_ref(&parts, span, sh.group[i], sh, stmt, an);
            return after + 2;
        }
        if next.is_some_and(|n| n.is(SyntaxKind::L_PAREN)) {
            let word = parts
                .last()
                .map(|p| p.text.to_ascii_uppercase())
                .unwrap_or_default();
            let known_fn = self.builtins().function(&word, "9999") != Availability::Unknown;
            let is_call = parts.len() > 1 || !self.is_keyword(&word) || known_fn;
            if is_call && !t[i].is(SyntaxKind::QUOTED_IDENT) {
                return self.call_at(t, sh, i, after, parts, span, stmt, an);
            }
            return after;
        }
        // Named argument notation (`name => v`, PostgreSQL's older `name := v`).
        if next
            .is_some_and(|n| n.is_op("=>") || (n.is_op(":=") && sh.inner[i] == Some(Paren::Call)))
        {
            return after + 1;
        }
        let var_only = an.var_only[i];
        self.resolve_chain(t, &parts, span, sh.group[i], sh, stmt, an, var_only, i);
        after
    }

    #[allow(clippy::too_many_arguments)]
    fn call_at(
        &mut self,
        t: &[Tok],
        _sh: &Shape,
        i: usize,
        open: usize,
        parts: Vec<Ident>,
        span: (usize, usize),
        stmt: (usize, usize),
        an: &mut Analysis,
    ) -> usize {
        let close = close_paren(t, open);
        let inner = &t[open + 1..close];
        let word = parts
            .last()
            .map(|p| p.text.to_ascii_uppercase())
            .unwrap_or_default();
        let single = parts.len() == 1;
        // Arguments that are not references.
        if single && self.d == Dialect::Tsql {
            match word.as_str() {
                "CONVERT" | "TRY_CONVERT" => {
                    let first = split_commas(inner).first().map(|f| f.len()).unwrap_or(0);
                    if first > 0 {
                        let ty = DataType::parse(&tokens::texts(&inner[..first]));
                        self.check_type(&ty, (inner[0].start, inner[first - 1].end), stmt);
                        for m in open + 1..open + 1 + first {
                            an.consumed[m] = true;
                        }
                    }
                }
                "DATEADD" | "DATEDIFF" | "DATEDIFF_BIG" | "DATEPART" | "DATENAME" | "DATETRUNC"
                | "DATE_BUCKET" => {
                    if inner.first().is_some_and(Tok::is_name) {
                        an.consumed[open + 1] = true;
                    }
                }
                "ISNULL" => {
                    let mut args = Vec::new();
                    let mut s = open + 1;
                    for a in split_commas(inner) {
                        args.push((s, s + a.len()));
                        s += a.len() + 1;
                    }
                    an.isnull.push((i, args));
                }
                _ => {}
            }
        }
        if single && word == "EXTRACT" && inner.first().is_some_and(Tok::is_name) {
            an.consumed[open + 1] = true;
        }
        // Named-argument names are not references.
        let mut args = Vec::new();
        let mut s = open + 1;
        for a in split_commas(inner) {
            if a.len() >= 2
                && a[0].is_name()
                && (a[1].is_op("=>")
                    || a[1].is_op(":=")
                    || (a[1].is(SyntaxKind::COLON) && a.get(2).is_some_and(|y| y.is_op("="))))
            {
                an.consumed[s] = true;
                args.push(CallArg::Named(a[0].ident().text));
            } else if !a.is_empty() {
                args.push(CallArg::Positional);
            }
            s += a.len() + 1;
        }
        // COUNT(*), aggregates with DISTINCT etc. are fine as positional.
        self.resolve_call(&parts, span, stmt, &args, CallStyle::Function);
        // Bind the arguments with the generic pass (the caller continues inside).
        open + 1
    }

    /// Resolves a routine or function call and checks its signature.
    pub(crate) fn resolve_call(
        &mut self,
        parts: &[Ident],
        span: (usize, usize),
        stmt: (usize, usize),
        args: &[CallArg],
        style: CallStyle,
    ) {
        let d = self.d;
        let display = parts
            .iter()
            .map(|p| p.text.clone())
            .collect::<Vec<_>>()
            .join(".");
        let kind = if style == CallStyle::Function {
            RefKind::Function
        } else {
            RefKind::Routine
        };
        let local = CatalogView::new(d, &[&self.local]);
        let view = self.input.view;
        let found = match local.routines(parts) {
            Lookup::Found(r) => Some(r.into_iter().cloned().collect::<Vec<_>>()),
            _ => match view.routines(parts) {
                Lookup::Found(r) => Some(r.into_iter().cloned().collect::<Vec<_>>()),
                _ => None,
            },
        };
        if let Some(overloads) = found {
            let ok = overloads
                .iter()
                .any(|r| signature_accepts(d, r, args, style));
            let r = &overloads[0];
            if ok {
                self.add_ref(
                    kind,
                    display,
                    span,
                    stmt,
                    Resolution::Resolved(Target::Routine(r.display())),
                );
            } else if let Some(id) = self.defined_later(parts, &display, Some((args, style))) {
                self.add_ref(kind, display, span, stmt, Resolution::Assumed(id));
            } else {
                let given = describe_args(args);
                let sigs: Vec<String> = overloads.iter().map(|r| r.signature()).collect();
                let missing: Vec<String> = overloads
                    .iter()
                    .flat_map(|r| missing_params(d, r, args, style))
                    .collect();
                let tail = if missing.is_empty() {
                    String::new()
                } else {
                    format!(" Missing: {}.", missing.join(", "))
                };
                let msg = format!(
                    "`{display}` is called with {given}, but its signature is {}; the call fails at run time.{tail}",
                    sigs.join(" or ")
                );
                self.fail(
                    kind,
                    display.clone(),
                    span,
                    stmt,
                    "bind.signature",
                    msg,
                    format!("signature {} {}", display.to_lowercase(), args.len()),
                    Vec::new(),
                );
            }
            return;
        }
        let word = parts.last().map(|p| p.text.clone()).unwrap_or_default();
        let system_schema = parts.len() == 2
            && crate::catalog::system_schemas(d)
                .iter()
                .any(|s| Ident::plain(*s).key(d) == parts[0].key(d));
        if parts.len() == 1 || system_schema {
            let b = self.builtins();
            let avail = if style == CallStyle::Function {
                b.function(&word, &self.version)
            } else if b.procedure(&word) {
                Availability::Available
            } else {
                Availability::Unknown
            };
            match avail {
                Availability::Available => {
                    self.add_ref(
                        kind,
                        display,
                        span,
                        stmt,
                        Resolution::Resolved(Target::Builtin),
                    );
                    return;
                }
                Availability::Since(v) => {
                    let msg = format!(
                        "`{word}` needs {} {v} or later; the target is {} {}.",
                        d.display_name(),
                        d.display_name(),
                        self.version
                    );
                    self.fail(
                        kind,
                        display,
                        span,
                        stmt,
                        "bind.builtin-version",
                        msg,
                        format!("builtin {}", word.to_lowercase()),
                        Vec::new(),
                    );
                    return;
                }
                Availability::Unknown => {}
            }
        }
        if self.input.helpers.iter().any(|h| {
            h.dialect == d
                && Ident::plain(h.name.clone()).key(d) == Ident::new(word.clone(), false).key(d)
        }) {
            self.add_ref(
                kind,
                display,
                span,
                stmt,
                Resolution::Resolved(Target::Helper),
            );
            return;
        }
        let elsewhere = if style == CallStyle::Function && parts.len() == 1 {
            builtins::function_elsewhere(&word, d)
        } else {
            Vec::new()
        };
        if !elsewhere.is_empty() {
            let names: Vec<&str> = elsewhere.iter().map(|x| x.display_name()).collect();
            if !builtins::of(d).known_absent(&word) {
                // Not on the verified-absent list: the built-in list may just be incomplete.
                let id = self.assume(
                    format!("ASM-BUILTIN-ABSENT:{}", word.to_lowercase()),
                    format!(
                        "{} has no built-in function `{word}` (it is not on SQLDiff's built-in list)",
                        d.display_name()
                    ),
                );
                let msg = format!(
                    "`{word}()` is a {} built-in and is not on SQLDiff's {} built-in list, and no helper or catalog routine provides it; if {} has no such function, the statement fails when it runs.",
                    names.join("/"),
                    d.display_name(),
                    d.display_name()
                );
                self.issue(Issue {
                    rule: "bind.unknown-builtin",
                    class: ChangeClass::Binding,
                    severity: Severity::Medium,
                    proven: false,
                    message: msg,
                    start: span.0,
                    end: span.1,
                    key: format!("builtin {}", word.to_lowercase()),
                    assumptions: vec![id.clone()],
                });
                self.add_ref(kind, display, span, stmt, Resolution::Assumed(id));
                return;
            }
            let msg = format!(
                "`{word}()` is a {} built-in; {} has no function of that name and no helper or catalog routine provides one, so the statement fails when it runs.",
                names.join("/"),
                d.display_name()
            );
            self.fail(
                kind,
                display,
                span,
                stmt,
                "bind.unknown-builtin",
                msg,
                format!("builtin {}", word.to_lowercase()),
                Vec::new(),
            );
            return;
        }
        let rkind = if style == CallStyle::Function {
            RoutineKind::Function
        } else {
            RoutineKind::Procedure
        };
        if let Some(id) = self.defined_later(parts, &display, None) {
            self.add_ref(kind, display, span, stmt, Resolution::Assumed(id));
            return;
        }
        if view.routines_authoritative(rkind) {
            let what = if style == CallStyle::Function {
                "function"
            } else {
                "procedure"
            };
            let msg = format!(
                "No {what} `{display}` exists in the {} catalog ({}), and it is not a built-in or pack helper.",
                d.display_name(),
                view.origin()
            );
            self.fail(
                kind,
                display.clone(),
                span,
                stmt,
                "bind.unknown-routine",
                msg,
                format!("routine {}", display.to_lowercase()),
                Vec::new(),
            );
            return;
        }
        let id = self.assume(
            format!("ASM-BIND-ROUTINE:{}", display.to_lowercase()),
            format!("Routine `{display}` exists in the target database with a signature that accepts this call"),
        );
        self.add_ref(kind, display, span, stmt, Resolution::Assumed(id));
    }

    /// Inside a routine body, a routine the artifact creates further down
    /// (an overload that accepts the call, when `call` is given) may exist
    /// by the time the body runs: an assumption, not a binding error.
    fn defined_later(
        &mut self,
        parts: &[Ident],
        display: &str,
        call: Option<(&[CallArg], CallStyle)>,
    ) -> Option<String> {
        if !self.in_routine() {
            return None;
        }
        let d = self.d;
        let later = CatalogView::new(d, &[&self.later]);
        let Lookup::Found(rs) = later.routines(parts) else {
            return None;
        };
        let fits = match call {
            Some((args, style)) => rs.iter().any(|r| signature_accepts(d, r, args, style)),
            None => true,
        };
        if !fits {
            return None;
        }
        Some(self.assume(
            format!("ASM-BIND-ROUTINE-LATER:{}", display.to_lowercase()),
            format!("Routine `{display}`, created later in this file, exists when this body runs"),
        ))
    }

    // ---- references -------------------------------------------------------

    /// Sources visible from group `g`, innermost level first.
    fn visible<'s>(&self, an: &'s Analysis, sh: &Shape, g: usize) -> Vec<Vec<&'s Source>> {
        let chain = sh.chain(g);
        chain
            .iter()
            .map(|&lvl| {
                an.sources
                    .iter()
                    // A derived table is not visible inside its own query.
                    .filter(|s| s.group == lvl && s.body.map_or(true, |b| !chain.contains(&b)))
                    .collect()
            })
            .collect()
    }

    fn find_source<'s>(
        &self,
        an: &'s Analysis,
        sh: &Shape,
        g: usize,
        q: &str,
        schema: Option<&str>,
    ) -> Option<&'s Source> {
        for lvl in self.visible(an, sh, g) {
            let hit = lvl.iter().find(|s| {
                let by_alias = s.alias.as_deref() == Some(q);
                let by_name = s.alias.is_none()
                    && s.name.as_deref() == Some(q)
                    && (schema.is_none() || s.schema.as_deref() == schema);
                // Without an alias, `schema.table.col` and `table.col` both work; with one, only the alias.
                by_alias || by_name || (s.name.as_deref() == Some(q) && s.alias.is_none())
            });
            if let Some(s) = hit {
                return Some(s);
            }
        }
        // Pseudo tables and sources of other groups (lenient).
        an.sources.iter().find(|s| {
            s.alias.as_deref() == Some(q) || (s.alias.is_none() && s.name.as_deref() == Some(q))
        })
    }

    /// PostgreSQL 18 `old` / `new` in RETURNING (only there): the changed
    /// table's row before and after the change.
    fn row_image<'s>(&self, an: &'s Analysis, g: usize, q: &str, at: usize) -> Option<&'s Source> {
        let (rg, start, ti) = an.returning?;
        (matches!(q, "new" | "old") && rg == g && at > start)
            .then(|| an.sources.get(ti))
            .flatten()
    }

    fn qualifier_ref(
        &mut self,
        parts: &[Ident],
        span: (usize, usize),
        g: usize,
        sh: &Shape,
        stmt: (usize, usize),
        an: &Analysis,
    ) {
        let q = parts.last().map(|p| p.key(self.d)).unwrap_or_default();
        if self.find_source(an, sh, g, &q, None).is_some()
            || self.row_image(an, g, &q, span.0).is_some()
        {
            self.add_ref(
                RefKind::Table,
                parts.last().unwrap().text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Implicit),
            );
        } else if self
            .lookup(
                &q,
                &[DeclKind::Variable, DeclKind::Parameter, DeclKind::LoopVar],
            )
            .is_some()
        {
            self.add_ref(
                RefKind::Variable,
                parts.last().unwrap().text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Implicit),
            );
        } else {
            let id = self.assume(
                format!("ASM-BIND-ALIAS:{}", q.to_lowercase()),
                format!("`{}` names a relation in scope", parts.last().unwrap().text),
            );
            self.add_ref(
                RefKind::Table,
                parts.last().unwrap().text.clone(),
                span,
                stmt,
                Resolution::Assumed(id),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_chain(
        &mut self,
        t: &[Tok],
        parts: &[Ident],
        span: (usize, usize),
        g: usize,
        sh: &Shape,
        stmt: (usize, usize),
        an: &Analysis,
        var_only: bool,
        at: usize,
    ) {
        let d = self.d;
        match parts.len() {
            1 => self.resolve_bare(t, &parts[0], span, g, sh, stmt, an, var_only, at),
            2 | 3 => {
                let (schema, q, n) = if parts.len() == 3 {
                    (Some(parts[0].key(d)), parts[1].key(d), &parts[2])
                } else {
                    (None, parts[0].key(d), &parts[1])
                };
                let name = parts
                    .iter()
                    .map(|p| p.text.clone())
                    .collect::<Vec<_>>()
                    .join(".");
                let image = || {
                    schema
                        .is_none()
                        .then(|| self.row_image(an, g, &q, span.0))
                        .flatten()
                };
                if let Some(src) = self
                    .find_source(an, sh, g, &q, schema.as_deref())
                    .or_else(image)
                    .cloned()
                {
                    self.column_of(&src, n, &name, span, stmt);
                    return;
                }
                // Row variable, record, trigger row.
                if let Some(di) = self.lookup(
                    &q,
                    &[DeclKind::Variable, DeclKind::Parameter, DeclKind::LoopVar],
                ) {
                    self.field_of(di, n, &name, span, stmt);
                    return;
                }
                if let Some(tr) = self.trigger.clone() {
                    let is_pg_row = tr.pg_function && (q == "new" || q == "old");
                    let is_db2_ref = tr.referencing.iter().any(|(a, _)| *a == q);
                    if is_pg_row || is_db2_ref {
                        let tb = if is_db2_ref {
                            self.lookup_table_silent(&tr.table)
                        } else {
                            None
                        };
                        match tb {
                            Some(tb) => {
                                let src = Source {
                                    alias: Some(q.clone()),
                                    name: None,
                                    schema: None,
                                    display: tb.display(),
                                    columns: Some(cols_of(d, &tb)),
                                    authoritative: tb.columns_known,
                                    group: 0,
                                    in_scope: true,
                                    from: 0,
                                    failed: None,
                                    body: None,
                                };
                                self.column_of(&src, n, &name, span, stmt);
                            }
                            None => {
                                let id = self.assume("ASM-BIND-TRIGGER-ROW".into(), "The trigger's table has the columns the trigger body reads from NEW and OLD".into());
                                self.add_ref(
                                    RefKind::Column,
                                    name,
                                    span,
                                    stmt,
                                    Resolution::Assumed(id),
                                );
                            }
                        }
                        return;
                    }
                }
                // Label-qualified variable.
                if self.has_label(&q) && parts.len() == 2 {
                    let key = n.key(d);
                    match self.lookup_qualified(&q, &key) {
                        Some(di) => self.add_ref(
                            RefKind::Variable,
                            name,
                            span,
                            stmt,
                            Resolution::Resolved(Target::Decl(di)),
                        ),
                        None => {
                            let tok = Tok {
                                kind: SyntaxKind::IDENT,
                                text: name.clone(),
                                start: span.0,
                                end: span.1,
                            };
                            self.unknown_variable(&tok, stmt);
                        }
                    }
                    return;
                }
                let all_known = self
                    .visible(an, sh, g)
                    .iter()
                    .flatten()
                    .all(|s| s.authoritative)
                    && !an.sources.is_empty()
                    && self.trigger.is_none()
                    && !self.has_unknown_rows();
                if all_known && parts.len() == 2 {
                    let msg = format!(
                        "`{name}`: no table, alias or row variable named `{}` is in scope, so the reference cannot be bound.",
                        parts[0].text
                    );
                    self.fail(
                        RefKind::Column,
                        name.clone(),
                        span,
                        stmt,
                        "bind.unknown-alias",
                        msg,
                        format!("alias {}", name.to_lowercase()),
                        Vec::new(),
                    );
                } else {
                    let id = self.assume(
                        format!("ASM-BIND-COLUMN:{}", name.to_lowercase()),
                        format!("`{name}` resolves to a column of a relation in scope"),
                    );
                    self.add_ref(RefKind::Column, name, span, stmt, Resolution::Assumed(id));
                }
            }
            _ => {
                let name = parts
                    .iter()
                    .map(|p| p.text.clone())
                    .collect::<Vec<_>>()
                    .join(".");
                let id = self.assume(
                    format!("ASM-BIND-COLUMN:{}", name.to_lowercase()),
                    format!("`{name}` resolves to a column of a cross-database object"),
                );
                self.add_ref(RefKind::Column, name, span, stmt, Resolution::Assumed(id));
            }
        }
    }

    fn lookup_table_silent(&self, parts: &[Ident]) -> Option<Table> {
        let local = CatalogView::new(self.d, &[&self.local]);
        match local.table(parts) {
            Lookup::Found(x) => Some(x.clone()),
            _ => match self.input.view.table(parts) {
                Lookup::Found(x) => Some(x.clone()),
                _ => None,
            },
        }
    }

    fn has_unknown_rows(&self) -> bool {
        self.scopes.iter().flat_map(|s| s.decls.iter()).any(|&i| {
            let dcl = &self.out.decls[i];
            dcl.row.is_none() && dcl.data_type.as_ref().is_some_and(|t| t.base == "RECORD")
        })
    }

    fn column_of(
        &mut self,
        src: &Source,
        n: &Ident,
        name: &str,
        span: (usize, usize),
        stmt: (usize, usize),
    ) {
        if let Some(f) = src.failed {
            self.add_ref(
                RefKind::Column,
                name.to_string(),
                span,
                stmt,
                Resolution::Failed(f),
            );
            return;
        }
        let key = n.key(self.d);
        match &src.columns {
            Some(cols) => match cols.iter().find(|c| c.key == key) {
                Some(c) => {
                    let target = Target::Column {
                        table: src.display.clone(),
                        column: c.name.clone(),
                        data_type: c.data_type.clone(),
                        nullable: c.nullable,
                    };
                    self.add_ref(
                        RefKind::Column,
                        name.to_string(),
                        span,
                        stmt,
                        Resolution::Resolved(target),
                    );
                }
                None if src.authoritative => {
                    let disp = src.display.clone();
                    self.unknown_column_in(&n.text, &disp, span, stmt);
                }
                None => {
                    let id = self.assume(
                        format!("ASM-BIND-COLUMN:{}", name.to_lowercase()),
                        format!("`{name}` is a column of {}", src.display),
                    );
                    self.add_ref(
                        RefKind::Column,
                        name.to_string(),
                        span,
                        stmt,
                        Resolution::Assumed(id),
                    );
                }
            },
            None => {
                let id = self.assume(
                    format!("ASM-BIND-COLUMN:{}", name.to_lowercase()),
                    format!("`{name}` is a column of {}", src.display),
                );
                self.add_ref(
                    RefKind::Column,
                    name.to_string(),
                    span,
                    stmt,
                    Resolution::Assumed(id),
                );
            }
        }
    }

    fn field_of(
        &mut self,
        di: usize,
        n: &Ident,
        name: &str,
        span: (usize, usize),
        stmt: (usize, usize),
    ) {
        let key = n.key(self.d);
        let row = self.out.decls[di].row.clone();
        match row {
            Some(fields) => {
                if fields
                    .iter()
                    .any(|f| f.name.as_ref().is_some_and(|x| x.key(self.d) == key))
                {
                    self.add_ref(
                        RefKind::Column,
                        name.to_string(),
                        span,
                        stmt,
                        Resolution::Resolved(Target::Decl(di)),
                    );
                } else if fields.iter().all(|f| f.name.is_some()) {
                    let dn = self.out.decls[di].name.clone();
                    let msg = format!(
                        "Row `{dn}` has no field `{}`; the reference fails when it runs.",
                        n.text
                    );
                    self.fail(
                        RefKind::Column,
                        name.to_string(),
                        span,
                        stmt,
                        "bind.unknown-field",
                        msg,
                        format!("field {}", name.to_lowercase()),
                        Vec::new(),
                    );
                } else {
                    let id = self.assume(
                        format!("ASM-BIND-FIELD:{}", name.to_lowercase()),
                        format!("Row `{}` has a field `{}`", self.out.decls[di].name, n.text),
                    );
                    self.add_ref(
                        RefKind::Column,
                        name.to_string(),
                        span,
                        stmt,
                        Resolution::Assumed(id),
                    );
                }
            }
            None => {
                let id = self.assume(
                    format!("ASM-BIND-FIELD:{}", name.to_lowercase()),
                    format!(
                        "Record `{}` has a field `{}` when this runs",
                        self.out.decls[di].name, n.text
                    ),
                );
                self.add_ref(
                    RefKind::Column,
                    name.to_string(),
                    span,
                    stmt,
                    Resolution::Assumed(id),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_bare(
        &mut self,
        _t: &[Tok],
        x: &Ident,
        span: (usize, usize),
        g: usize,
        sh: &Shape,
        stmt: (usize, usize),
        an: &Analysis,
        var_only: bool,
        at: usize,
    ) {
        let d = self.d;
        let key = x.key(d);
        let var = if d == Dialect::Tsql {
            None
        } else {
            self.lookup(
                &key,
                &[DeclKind::Variable, DeclKind::Parameter, DeclKind::LoopVar],
            )
        };
        if var_only {
            match var {
                Some(i) => self.add_ref(
                    RefKind::Variable,
                    x.text.clone(),
                    span,
                    stmt,
                    Resolution::Resolved(Target::Decl(i)),
                ),
                None => {
                    let tok = Tok {
                        kind: SyntaxKind::IDENT,
                        text: x.text.clone(),
                        start: span.0,
                        end: span.1,
                    };
                    self.unknown_variable(&tok, stmt);
                }
            }
            return;
        }
        // Columns: innermost query level with a match wins.
        let mut matches: Vec<(Source, Col)> = Vec::new();
        for lvl in self.visible(an, sh, g) {
            for s in lvl.iter().filter(|s| s.visible_at(at)) {
                if let Some(c) = s
                    .columns
                    .as_ref()
                    .and_then(|cs| cs.iter().find(|c| c.key == key))
                {
                    matches.push(((*s).clone(), c.clone()));
                }
            }
            if !matches.is_empty() {
                break;
            }
        }
        let col_target = |s: &Source, c: &Col| Target::Column {
            table: s.display.clone(),
            column: c.name.clone(),
            data_type: c.data_type.clone(),
            nullable: c.nullable,
        };
        match (var, matches.first()) {
            (Some(i), Some((s, c))) => {
                if d == Dialect::Postgres && !self.pg_conflict_directive {
                    let dn = self.out.decls[i].name.clone();
                    let msg = format!(
                        "`{}` is both PL/pgSQL variable `{dn}` (line {}) and column {}.{}; with the default plpgsql.variable_conflict = error the statement fails with \"column reference is ambiguous\".",
                        x.text,
                        self.line(self.out.decls[i].start),
                        s.display,
                        c.name
                    );
                    let asm = self.assume(
                        "ASM-PG-VARIABLE-CONFLICT-ERROR".into(),
                        "plpgsql.variable_conflict is `error` (the default) for this function"
                            .into(),
                    );
                    self.fail(
                        RefKind::Column,
                        x.text.clone(),
                        span,
                        stmt,
                        "bind.ambiguous-name",
                        msg,
                        format!("ambiguous {key}"),
                        vec![asm],
                    );
                } else {
                    // Db2 for i and T-SQL: the column wins.
                    let target = col_target(s, c);
                    if d == Dialect::Db2i {
                        let msg = format!(
                            "`{}` names both a column of {} and an SQL variable; Db2 resolves it to the column, so the variable is not used here.",
                            x.text, s.display
                        );
                        self.issue(Issue {
                            rule: "bind.name-conflict",
                            class: ChangeClass::Binding,
                            severity: Severity::Low,
                            proven: false,
                            message: msg,
                            start: span.0,
                            end: span.1,
                            key: format!("conflict {key}"),
                            assumptions: Vec::new(),
                        });
                    }
                    self.add_ref(
                        RefKind::Column,
                        x.text.clone(),
                        span,
                        stmt,
                        Resolution::Resolved(target),
                    );
                }
            }
            (Some(i), None) => self.add_ref(
                RefKind::Variable,
                x.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Decl(i)),
            ),
            (None, Some((s, c))) => {
                if matches.len() > 1 && matches.iter().any(|(s2, _)| s2.display != s.display) {
                    let names: Vec<String> =
                        matches.iter().map(|(s, _)| s.display.clone()).collect();
                    let msg = format!(
                        "`{}` matches a column of several relations at the same level ({}); qualify it.",
                        x.text,
                        names.join(", ")
                    );
                    self.issue(Issue {
                        rule: "bind.ambiguous-column",
                        class: ChangeClass::Binding,
                        severity: Severity::Medium,
                        proven: false,
                        message: msg,
                        start: span.0,
                        end: span.1,
                        key: format!("ambiguous-column {key}"),
                        assumptions: Vec::new(),
                    });
                }
                let target = col_target(s, c);
                self.add_ref(
                    RefKind::Column,
                    x.text.clone(),
                    span,
                    stmt,
                    Resolution::Resolved(target),
                );
            }
            (None, None) => self.unresolved_bare(x, span, g, sh, stmt, an, at),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn unresolved_bare(
        &mut self,
        x: &Ident,
        span: (usize, usize),
        g: usize,
        sh: &Shape,
        stmt: (usize, usize),
        an: &Analysis,
        at: usize,
    ) {
        let d = self.d;
        let key = x.key(d);
        // Columns of Db2 FOR rows are visible unqualified.
        let row_hit = self
            .scopes
            .iter()
            .rev()
            .flat_map(|s| s.decls.iter().rev().copied())
            .find(|&i| {
                let dcl = &self.out.decls[i];
                dcl.kind == DeclKind::LoopVar
                    && d == Dialect::Db2i
                    && dcl.row.as_ref().is_some_and(|r| {
                        r.iter()
                            .any(|o| o.name.as_ref().is_some_and(|n| n.key(d) == key))
                    })
            });
        if let Some(i) = row_hit {
            self.add_ref(
                RefKind::Column,
                x.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Decl(i)),
            );
            return;
        }
        if an.outputs.contains(&key) {
            self.add_ref(
                RefKind::Column,
                x.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::OutputAlias),
            );
            return;
        }
        if an
            .sources
            .iter()
            .any(|s| s.alias.as_deref() == Some(&key) || s.name.as_deref() == Some(&key))
            || an.ctes.iter().any(|c| c.0 == key)
        {
            self.add_ref(
                RefKind::Table,
                x.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Implicit),
            );
            return;
        }
        if self
            .lookup(&key, &[DeclKind::Cursor, DeclKind::Condition])
            .is_some()
            || self.has_label(&key)
        {
            self.add_ref(
                RefKind::Variable,
                x.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Implicit),
            );
            return;
        }
        if is_implicit(d, &key, self.trigger.is_some()) {
            self.add_ref(
                RefKind::Variable,
                x.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Implicit),
            );
            return;
        }
        if !x.quoted && self.is_keyword(&x.text) {
            return;
        }
        let visible: Vec<&Source> = self
            .visible(an, sh, g)
            .into_iter()
            .flatten()
            .filter(|s| s.visible_at(at))
            .collect();
        let tok = Tok {
            kind: SyntaxKind::IDENT,
            text: x.text.clone(),
            start: span.0,
            end: span.1,
        };
        if visible.is_empty() && an.sources.iter().all(|s| !s.visible_at(at) || s.group != g) {
            if d == Dialect::Tsql {
                let msg = format!("`{}` is not a column of any table in this statement; SQL Server fails with \"Invalid column name\".", x.text);
                self.fail(
                    RefKind::Column,
                    x.text.clone(),
                    span,
                    stmt,
                    "bind.unknown-column",
                    msg,
                    format!("column {key}"),
                    Vec::new(),
                );
            } else {
                self.unknown_variable(&tok, stmt);
            }
            return;
        }
        if visible.iter().all(|s| s.authoritative) && !visible.is_empty() {
            let tables: Vec<String> = visible.iter().map(|s| s.display.clone()).collect();
            let view = self.input.view;
            let also = if d == Dialect::Tsql {
                ""
            } else {
                ", and no variable or parameter of that name is in scope"
            };
            let msg = format!(
                "`{}` is not a column of {} in the catalog ({}){also}; the statement fails with an unknown-column error every time it runs.",
                x.text,
                tables.join(" or "),
                view.origin()
            );
            let k = format!(
                "column {}.{}",
                tables.join("+").to_lowercase(),
                key.to_lowercase()
            );
            self.fail(
                RefKind::Column,
                x.text.clone(),
                span,
                stmt,
                "bind.unknown-column",
                msg,
                k,
                Vec::new(),
            );
            return;
        }
        // A relation that failed to bind takes its unqualified columns down with it.
        if let Some(f) = visible.iter().find_map(|s| s.failed) {
            if visible
                .iter()
                .all(|s| s.failed.is_some() || s.authoritative)
            {
                self.add_ref(
                    RefKind::Column,
                    x.text.clone(),
                    span,
                    stmt,
                    Resolution::Failed(f),
                );
                return;
            }
        }
        self.assume_column(&tok, stmt);
    }

    /// Output columns of a query, for rows of FOR loops.
    pub(crate) fn typed_outputs(
        &self,
        outs: Option<Vec<OutputCol>>,
        _query: &[Tok],
    ) -> Option<Vec<OutputCol>> {
        outs
    }

    // ---- ISNULL and SELECT INTO ---------------------------------------------

    /// T-SQL `ISNULL(a, b)` returns the type of `a`: a longer string or a
    /// decimal `b` is truncated when `a` is NULL (COALESCE would not).
    fn isnull_hazards(&mut self, t: &[Tok], stmt: (usize, usize), an: &Analysis) {
        for (name_at, args) in &an.isnull {
            if args.len() != 2 {
                continue;
            }
            let ty = |s: usize, e: usize| -> Option<(DataType, bool, String)> {
                let text = tokens::texts(&t[s..e]).join("");
                if e == s + 1 && t[s].is(SyntaxKind::STRING) {
                    // Characters of the value: no N prefix or quotes, '' is one quote.
                    let body = t[s].text.trim_start_matches(['N', 'n']);
                    let inner = body
                        .strip_prefix('\'')
                        .and_then(|b| b.strip_suffix('\''))
                        .unwrap_or(body);
                    let len = inner.replace("''", "'").chars().count() as u32;
                    return Some((DataType::parse_str(&format!("varchar({len})")), false, text));
                }
                // The last reference recorded inside the argument span.
                let r = self.out.refs.iter().rev().find(|r| {
                    r.start >= t[s].start && r.end <= t[e - 1].end && r.start == t[s].start
                })?;
                match &r.resolution {
                    Resolution::Resolved(Target::Column {
                        data_type,
                        nullable,
                        ..
                    }) if e == s + 1 || (e == s + 3 && t[s + 1].is(SyntaxKind::DOT)) => {
                        Some((data_type.clone(), *nullable, text))
                    }
                    Resolution::Resolved(Target::Decl(i)) if e == s + 1 => self.out.decls[*i]
                        .data_type
                        .clone()
                        .map(|dt| (dt, true, text)),
                    _ => None,
                }
            };
            let (Some((t1, null1, a1)), Some((t2, _, a2))) =
                (ty(args[0].0, args[0].1), ty(args[1].0, args[1].1))
            else {
                continue;
            };
            if !null1 {
                continue;
            }
            // T-SQL declarations: `varchar` / `nvarchar` without a length hold one character.
            let tsql_len = |dt: &DataType| -> Option<u32> {
                if dt.args.is_empty()
                    && !dt.max
                    && matches!(dt.base.as_str(), "VARCHAR" | "NVARCHAR")
                {
                    Some(1)
                } else {
                    dt.char_length()
                }
            };
            let hazard = if t1.is_string() && t2.is_string() {
                match (tsql_len(&t1), tsql_len(&t2)) {
                    (Some(l1), Some(l2)) if l2 > l1 => Some(format!(
                        "when {a1} is NULL, values of {a2} longer than {l1} characters are truncated to {l1}; COALESCE would return {}",
                        t2.text
                    )),
                    (Some(l1), None) => Some(format!("when {a1} is NULL, {a2} is truncated to {l1} characters")),
                    _ => None,
                }
            } else if matches!(
                t1.base.as_str(),
                "INTEGER" | "SMALLINT" | "TINYINT" | "BIGINT"
            ) && matches!(
                t2.base.as_str(),
                "DECIMAL" | "NUMERIC" | "MONEY" | "FLOAT" | "REAL"
            ) {
                Some(format!(
                    "when {a1} is NULL, {a2} is converted to {} and loses its fraction",
                    t1.text
                ))
            } else {
                None
            };
            if let Some(h) = hazard {
                let s = t[*name_at].start;
                let e = t[args[1].1].end;
                let msg = format!(
                    "ISNULL({a1}, {a2}) takes the type of {a1} ({}): {h}.",
                    t1.text
                );
                self.issue(Issue {
                    rule: "types.isnull-truncation",
                    class: ChangeClass::Semantic,
                    severity: Severity::Medium,
                    proven: true,
                    message: msg,
                    start: s,
                    end: e,
                    key: format!("isnull {} {}", a1.to_lowercase(), a2.to_lowercase()),
                    assumptions: Vec::new(),
                });
            }
        }
        let _ = stmt;
    }

    fn create_from_select(&mut self, parts: &[Ident], t: &[Tok]) {
        let name = parts.last().map(|p| p.stored(self.d)).unwrap_or_default();
        let mut tb = Table::new(
            None,
            name.clone(),
            if name.starts_with('#') {
                TableKind::Temporary
            } else {
                TableKind::Table
            },
        );
        match ddl::select_outputs(t) {
            Some(outs) if outs.iter().all(|o| o.name.is_some()) => {
                for o in outs {
                    let n = o.name.unwrap();
                    tb.columns.push(crate::catalog::Column {
                        name: n.stored(self.d),
                        data_type: DataType::unknown(),
                        nullable: true,
                        identity: false,
                        has_default: false,
                        generated: false,
                        collation: None,
                    });
                }
            }
            _ => tb.columns_known = false,
        }
        self.local.put_table(self.d, tb);
    }
}

/// A parenthesis that holds a join (`(a JOIN b ON ...)`, possibly nested)
/// rather than a query, a call or an expression.
fn is_join_paren(t: &[Tok], open: usize) -> bool {
    if !t.get(open).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
        return false;
    }
    let c = close_paren(t, open);
    let inner = &t[open + 1..c.min(t.len())];
    if inner.first().map_or(true, |y| {
        y.is_any_kw(&[
            "SELECT", "WITH", "VALUES", "INSERT", "UPDATE", "DELETE", "MERGE",
        ])
    }) {
        return false;
    }
    let mut depth = 0usize;
    for y in inner {
        match y.kind {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
            _ if depth == 0 && y.is_any_kw(&["JOIN", "APPLY"]) => return true,
            _ => {}
        }
    }
    // `((a JOIN b ON ...))`: one join wrapped in another pair.
    inner.first().is_some_and(|y| y.is(SyntaxKind::L_PAREN))
        && close_paren(t, open + 1) + 1 == c
        && is_join_paren(t, open + 1)
}

fn cols_of(d: Dialect, tb: &Table) -> Vec<Col> {
    tb.columns
        .iter()
        .map(|c| Col {
            name: c.name.clone(),
            key: crate::catalog::key(d, &c.name),
            data_type: c.data_type.clone(),
            nullable: c.nullable,
        })
        .collect()
}

/// Index of the first DML keyword at the top level (after a `WITH` list).
fn first_dml(t: &[Tok]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, x) in t.iter().enumerate() {
        match x.kind {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
            _ if depth == 0
                && x.is_any_kw(&[
                    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "VALUES", "TRUNCATE",
                ]) =>
            {
                return Some(i)
            }
            _ => {}
        }
    }
    None
}

fn skip_top(t: &[Tok], mut k: usize) -> usize {
    k += 1;
    if t.get(k).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
        k = close_paren(t, k) + 1;
    } else {
        k += 1;
    }
    if t.get(k).is_some_and(|x| x.is_kw("PERCENT")) {
        k += 1;
    }
    if t.get(k).is_some_and(|x| x.is_kw("WITH")) && t.get(k + 1).is_some_and(|x| x.is_kw("TIES")) {
        k += 2;
    }
    k
}

fn is_implicit(d: Dialect, key: &str, in_trigger: bool) -> bool {
    match d {
        Dialect::Postgres => {
            matches!(key, "found" | "sqlstate" | "sqlerrm")
                || (in_trigger && (key.starts_with("tg_") || key == "new" || key == "old"))
        }
        Dialect::Db2i => {
            matches!(key, "SQLSTATE" | "SQLCODE")
                // Trigger event predicates of a multiple-event trigger.
                || (in_trigger && matches!(key, "INSERTING" | "UPDATING" | "DELETING"))
        }
        Dialect::Tsql => false,
    }
}

fn describe_args(args: &[CallArg]) -> String {
    let named: Vec<&str> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Named(n) => Some(n.as_str()),
            CallArg::Positional => None,
        })
        .collect();
    let n = args.len();
    let base = format!("{n} argument{}", if n == 1 { "" } else { "s" });
    if named.is_empty() {
        base
    } else {
        format!("{base} ({})", named.join(", "))
    }
}

fn param_key(d: Dialect, name: &str) -> String {
    let n = name.trim_start_matches('@');
    Ident::plain(n).key(d).to_lowercase()
}

/// Whether a routine accepts the call's argument shape.
fn signature_accepts(
    d: Dialect,
    r: &crate::catalog::Routine,
    args: &[CallArg],
    style: CallStyle,
) -> bool {
    missing_or_extra(d, r, args, style).is_none()
}

fn missing_params(
    d: Dialect,
    r: &crate::catalog::Routine,
    args: &[CallArg],
    style: CallStyle,
) -> Vec<String> {
    match missing_or_extra(d, r, args, style) {
        Some(Err(missing)) => missing,
        _ => Vec::new(),
    }
}

/// `None` when the call fits; `Some(Err(missing))` lists unsupplied
/// parameters; `Some(Ok(()))` for other mismatches (too many, unknown names).
fn missing_or_extra(
    d: Dialect,
    r: &crate::catalog::Routine,
    args: &[CallArg],
    style: CallStyle,
) -> Option<Result<(), Vec<String>>> {
    let params = r.call_params(d);
    let variadic = params
        .iter()
        .any(|p| p.mode == crate::catalog::ParamMode::Variadic);
    let positional = args
        .iter()
        .take_while(|a| **a == CallArg::Positional)
        .count();
    // Positional after named is an error in every dialect.
    if args[positional..].contains(&CallArg::Positional) {
        return Some(Ok(()));
    }
    if positional > params.len() && !variadic {
        return Some(Ok(()));
    }
    let mut supplied: Vec<bool> = (0..params.len()).map(|i| i < positional).collect();
    for a in &args[positional..] {
        if let CallArg::Named(n) = a {
            let k = param_key(d, n);
            match params.iter().position(|p| param_key(d, &p.name) == k) {
                Some(i) => supplied[i] = true,
                None => return Some(Ok(())),
            }
        }
    }
    // T-SQL scalar functions take every argument (DEFAULT must be written).
    let defaults_optional =
        !(d == Dialect::Tsql && style == CallStyle::Function && r.kind == RoutineKind::Function);
    let missing: Vec<String> = params
        .iter()
        .zip(&supplied)
        .filter(|(p, s)| {
            !**s && !(p.has_default && defaults_optional)
                && p.mode != crate::catalog::ParamMode::Variadic
        })
        .map(|(p, _)| {
            if p.name.is_empty() {
                p.data_type.text.clone()
            } else {
                p.name.clone()
            }
        })
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(Err(missing))
    }
}
