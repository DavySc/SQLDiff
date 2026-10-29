//! TOP and ORDER BY: row limits and where NULLs sort.
//!
//! Checked on SQL Server 2022 and PostgreSQL 18.6 (2026-10-29):
//! - `TOP (n)` with n NULL raises Msg 1014, negative Msg 127, and a
//!   fractional or string count Msg 1060; PostgreSQL's `LIMIT NULL` returns
//!   every row, `LIMIT -1` is an error and `LIMIT 2.7` keeps 3 rows. So a
//!   count is converted only when it is an integer, and a count that is not
//!   a literal carries `ASM-PAGE-COUNT-NOT-NULL`.
//! - TOP and OFFSET in one query is Msg 10741.
//! - `TOP n PERCENT` keeps ceil(n% of the rows) (30 PERCENT of 5 rows is 2);
//!   `TOP 100 PERCENT` keeps them all and is dropped; in a view the ORDER BY
//!   next to it does not order the view's rows, and stays (it orders nothing
//!   in the target either).
//! - `WITH TIES` ties rows that compare equal under the column's collation
//!   (`'B'` and `'b'` under a case-insensitive one), NULLs with each other.
//! - T-SQL sorts NULL as the lowest value, PostgreSQL as the highest; Db2 for
//!   i as the highest too (fact V19, Db2 for i part not checked on a real
//!   system), so the output states NULLS FIRST / NULLS LAST unless the key
//!   cannot be NULL.

use std::borrow::Cow;

use super::*;
use crate::tok::depths;

/// The rule of a `NULLS FIRST` / `NULLS LAST` the rules add.
const NULLS: &str = "tsql.order.nulls";
/// The rule of a row limit that replaces TOP.
pub(super) const TOP: &str = "tsql.select.top";

/// Words that end an ORDER BY list at its own depth.
const ORDER_END: &[&str] = &[
    "OFFSET",
    "FETCH",
    "FOR",
    "OPTION",
    "ROWS",
    "RANGE",
    "UNION",
    "EXCEPT",
    "INTERSECT",
];

/// A TOP clause, read.
pub(super) enum Top {
    /// `TOP n`: the last token of the clause and the target's row count.
    Rows { end: usize, count: String },
    /// `TOP 100 PERCENT`: every row.
    All { end: usize },
}

impl Cx<'_> {
    /// The TOP clause whose TOP word is `ts[top]`, or the reason it is not
    /// converted.
    pub(super) fn top_clause(&mut self, ts: &[T], top: usize) -> R<Top> {
        let arg = next_sig(ts, top).ok_or(No("TOP without a count".into()))?;
        let (end, inner) = if ts[arg].k == L_PAREN {
            let r = matching(ts, arg).ok_or(No("unbalanced TOP".into()))?;
            (r, trimmed(&ts[arg + 1..r]))
        } else {
            (arg, &ts[arg..=arg])
        };
        let after = next_sig(ts, end);
        if after.is_some_and(|j| ts[j].is("PERCENT")) {
            let end = after.unwrap();
            if let [n] = inner {
                if n.k == NUMBER && n.s.parse::<f64>() == Ok(100.0) {
                    return Ok(Top::All { end });
                }
            }
            return no(self.pick(
                "TOP … PERCENT keeps ceil(n% of the rows): PostgreSQL has no percentage limit; count the rows first (or use a window with count(*) OVER ()) and limit to that",
                "TOP … PERCENT keeps ceil(n% of the rows): Db2 for i has no percentage limit; count the rows first and limit to that",
            ));
        }
        if after.is_some_and(|j| ts[j].is("WITH")) {
            return no(self.pick(
                "TOP … WITH TIES: PostgreSQL's FETCH FIRST … WITH TIES ties rows by its own collation, T-SQL by the column's (case-insensitive by default); no rule yet",
                "TOP … WITH TIES: Db2 for i has no WITH TIES; rewrite with RANK() OVER (ORDER BY …) <= n",
            ));
        }
        let count = match inner {
            [n] if n.k == NUMBER => {
                if n.s.is_empty() || !n.s.bytes().all(|b| b.is_ascii_digit()) {
                    return no("TOP with a count that is not a whole number: SQL Server rejects it (Msg 1060), PostgreSQL and Db2 for i round it");
                }
                n.s.clone()
            }
            [v] if v.k == VARIABLE && self.integer(inner) => self.expr(inner).trim().to_string(),
            _ if self.db2() => {
                // FETCH FIRST takes a constant or a variable, not an expression.
                return no("TOP (expression): Db2 for i FETCH FIRST takes a number or a variable; compute the count into an integer variable first");
            }
            _ if self.integer(inner) => format!("({})", self.expr(inner).trim()),
            _ => {
                return no(self.pick(
                    "TOP (…) whose count is not known to be an integer: SQL Server rejects a fractional or string count (Msg 1060), PostgreSQL rounds it; compute the count into an integer variable first",
                    "TOP (…) whose count is not known to be an integer: SQL Server rejects a fractional or string count (Msg 1060); compute the count into an integer variable first",
                ))
            }
        };
        if inner.iter().any(|t| t.k == VARIABLE) {
            let text = format!(
                "The row count of TOP is never NULL: T-SQL raises an error (Msg 1014) for a NULL count, {}.",
                self.pick(
                    "PostgreSQL's LIMIT NULL returns every row",
                    "Db2 for i's FETCH FIRST with a NULL count was not checked on a real system"
                )
            );
            self.note(Status::Assumptions, "ASM-PAGE-COUNT-NOT-NULL", &text);
        }
        Ok(Top::Rows { end, count })
    }

    /// Db2 for i: a query below the statement's own (a subquery, derived
    /// table, common table expression, INSERT … SELECT or a view's query)
    /// keeps its ORDER BY and FETCH FIRST.
    pub(super) fn note_v32(&mut self) {
        self.note(Status::Assumptions, "ASM-FACT-V32", "ORDER BY and FETCH FIRST n ROWS ONLY in a subquery, derived table, common table expression, INSERT … SELECT or view: Db2 for i accepts them there and FETCH FIRST limits that query's rows after its own ORDER BY (fact V32, not checked on a real system).");
    }

    /// Whether an expression is an integer: integer literals and integer
    /// variables joined by arithmetic.
    fn integer(&self, ts: &[T]) -> bool {
        let s = sig(ts);
        !s.is_empty()
            && s.iter().all(|&i| {
                let t = &ts[i];
                match t.k {
                    NUMBER => t.s.bytes().all(|b| b.is_ascii_digit()),
                    VARIABLE => self
                        .vars
                        .get(&t.s.trim_start_matches('@').to_ascii_lowercase())
                        .is_some_and(|v| matches!(v.dt, Dt::Int | Dt::BigInt)),
                    OP => matches!(t.s.as_str(), "+" | "-" | "*" | "/" | "%"),
                    L_PAREN | R_PAREN => true,
                    _ => false,
                }
            })
    }

    /// The tokens the expression walk converts: TOP of each query that
    /// `select` does not handle itself (subqueries, derived tables, CTEs,
    /// INSERT … SELECT) becomes the target's row limit, and ORDER BY keys
    /// that may be NULL get T-SQL's NULL placement. A TOP that cannot be
    /// converted is left in place with a note.
    pub(super) fn paging<'t>(&mut self, ts: &'t [T]) -> Cow<'t, [T]> {
        let mut out = Cow::Borrowed(ts);
        if ts.iter().any(|t| t.is("TOP")) {
            if let Some(v) = self.nested_tops(&out) {
                out = Cow::Owned(v);
            }
        }
        if ts.iter().any(|t| t.is("ORDER")) {
            if let Some(v) = self.order_nulls(&out) {
                out = Cow::Owned(v);
            }
        }
        out
    }

    fn nested_tops(&mut self, ts: &[T]) -> Option<Vec<T>> {
        let ds = depths(ts);
        let mut cut = vec![false; ts.len()];
        let mut add: Vec<(usize, String)> = Vec::new();
        for i in 0..ts.len() {
            if !ts[i].is("SELECT") {
                continue;
            }
            let Some(mut k) = next_sig(ts, i) else {
                continue;
            };
            if ts[k].is("DISTINCT") || ts[k].is("ALL") {
                let Some(n) = next_sig(ts, k) else { continue };
                k = n;
            }
            if !ts[k].is("TOP") {
                continue;
            }
            let d = ds[i];
            // The query block: up to the parenthesis that closes it, a set
            // operator or the end of the statement.
            let mut end = ts.len();
            let mut refusal = None;
            for j in k + 1..ts.len() {
                if ds[j] < d || ts[j].k == SEMICOLON {
                    end = j;
                    break;
                }
                if ds[j] != d || ts[j].k != IDENT {
                    continue;
                }
                if ["UNION", "EXCEPT", "INTERSECT"].iter().any(|w| ts[j].is(w)) {
                    refusal = Some("TOP in one query of a UNION/EXCEPT/INTERSECT: no rule yet");
                    break;
                }
                if ts[j].is("OFFSET") {
                    refusal =
                        Some("TOP and OFFSET in one query: SQL Server rejects it (Msg 10741)");
                    break;
                }
                if ts[j].is("FOR") || ts[j].is("OPTION") {
                    end = j;
                    break;
                }
            }
            if let Some(r) = refusal {
                self.note(Status::NotConverted, TOP, r);
                return None;
            }
            let clause = match self.top_clause(ts, k) {
                Ok(c) => c,
                Err(No(r)) => {
                    self.note(Status::NotConverted, TOP, &r);
                    return None;
                }
            };
            // `EXISTS (SELECT TOP 1 …)`: EXISTS asks for one row, so a limit
            // of one or more changes nothing there.
            let in_exists = d > 0
                && (0..i)
                    .rev()
                    .find(|&o| ts[o].k == L_PAREN && ds[o] == d - 1)
                    .and_then(|o| prev_sig(ts, o))
                    .is_some_and(|p| ts[p].is("EXISTS"));
            let (last, limit) = match clause {
                Top::Rows { end, count }
                    if in_exists && count.parse::<u64>().is_ok_and(|n| n >= 1) =>
                {
                    (end, None)
                }
                Top::Rows { end, count } => (end, Some(count)),
                Top::All { end } => (end, None),
            };
            self.step_ts(TOP, &ts[k..=last]);
            // The clause and the blanks after it.
            let stop = next_sig(ts, last).unwrap_or(ts.len());
            cut[k..stop].iter_mut().for_each(|c| *c = true);
            // An ORDER BY left without its limit still stands in the subquery.
            if limit.is_none() && self.db2() && (stop..end).any(|j| ds[j] == d && ts[j].is("ORDER"))
            {
                self.note_v32();
            }
            if let Some(n) = limit {
                if self.db2() {
                    self.note_v32();
                }
                let Some(at) = (stop..end).rev().find(|&j| ts[j].sig()) else {
                    continue;
                };
                add.push((at, self.limit(&n, " ")));
            }
        }
        let mut v = Vec::with_capacity(ts.len() + add.len());
        for (i, t) in ts.iter().enumerate() {
            if !cut[i] {
                v.push(t.clone());
            }
            for (_, text) in add.iter().filter(|(at, _)| *at == i) {
                // Copied as is by the expression walk.
                v.push(T::made(NUMBER, text.clone(), t.pos.1, TOP));
            }
        }
        Some(v)
    }

    /// Adds NULLS FIRST (ascending) or NULLS LAST (descending) to every
    /// ORDER BY key that may be NULL, so the order is T-SQL's.
    fn order_nulls(&mut self, ts: &[T]) -> Option<Vec<T>> {
        let ds = depths(ts);
        let mut add: Vec<(usize, &'static str)> = Vec::new();
        for i in 0..ts.len() {
            if !ts[i].is("ORDER") {
                continue;
            }
            let Some(by) = next_sig(ts, i).filter(|&j| ts[j].is("BY")) else {
                continue;
            };
            let d = ds[i];
            let mut items = Vec::new();
            let mut start = by + 1;
            let mut j = by + 1;
            while j < ts.len() {
                let t = &ts[j];
                if ds[j] < d || t.k == SEMICOLON {
                    break;
                }
                if ds[j] == d {
                    if t.k == IDENT && ORDER_END.iter().any(|w| t.is(w)) {
                        break;
                    }
                    // A row limit a rule added after the list.
                    if t.made.is_some_and(|r| r != NULLS) {
                        break;
                    }
                    if t.k == COMMA {
                        items.push((start, j));
                        start = j + 1;
                    }
                }
                j += 1;
            }
            items.push((start, j));
            for (a, b) in items {
                let s: Vec<usize> = (a..b).filter(|&k| ts[k].sig()).collect();
                let Some(&last) = s.last() else { continue };
                if s.iter()
                    .any(|&k| ts[k].made == Some(NULLS) || ts[k].is("NULLS"))
                {
                    continue;
                }
                let desc = ts[last].is("DESC");
                let key_end = if desc || ts[last].is("ASC") {
                    match s.len() {
                        1 => continue,
                        n => s[n - 2],
                    }
                } else {
                    last
                };
                if self.not_null_key(ts, &ds, i, &ts[s[0]..=key_end]) {
                    continue;
                }
                add.push((last, if desc { " NULLS LAST" } else { " NULLS FIRST" }));
            }
        }
        if add.is_empty() {
            return None;
        }
        if self.db2() {
            self.note(Status::Assumptions, "ASM-FACT-V19", "ORDER BY states NULLS FIRST / NULLS LAST, T-SQL's NULL placement (NULL sorts lowest); Db2 for i accepts and honours it (fact V19, not checked on a real system).");
        }
        let mut v = Vec::with_capacity(ts.len() + add.len());
        for (i, t) in ts.iter().enumerate() {
            v.push(t.clone());
            if let Some((_, text)) = add.iter().find(|(at, _)| *at == i) {
                v.push(T::made(NUMBER, *text, t.pos.1, NULLS));
            }
        }
        Some(v)
    }

    /// Whether the ORDER BY key `key` (of the ORDER at `ts[order]`) is a
    /// column the source catalog declares NOT NULL, of a table that no
    /// outer join can leave out. `false` whenever that is not certain.
    fn not_null_key(&self, ts: &[T], ds: &[i32], order: usize, key: &[T]) -> bool {
        use sqldiff_core::catalog::Ident;
        // Only with the tree's catalog, which the engine checks against too
        // (without it, it cannot tell either). And not in a statement with
        // CTEs: a FROM name may be a CTE, not a table.
        if self.catalog.is_none() || self.scope.is_none() {
            return false;
        }
        let Some(parts) = name_parts(key) else {
            return false;
        };
        let (q, col) = match parts.as_slice() {
            [c] => (None, c),
            [q, c] => (Some(q), c),
            _ => return false,
        };
        // The query the key belongs to: the window's or the group's for
        // `OVER (… ORDER BY …)` and `WITHIN GROUP (ORDER BY …)`.
        let mut d = ds[order];
        let mut from = order;
        if d > 0 {
            if let Some(open) = (0..order)
                .rev()
                .find(|&k| ts[k].k == L_PAREN && ds[k] == d - 1)
            {
                if prev_sig(ts, open).is_some_and(|p| ts[p].is("OVER") || ts[p].is("GROUP")) {
                    d -= 1;
                    from = open;
                }
            }
        }
        let Some(select) = (0..from)
            .rev()
            .take_while(|&k| ds[k] >= d)
            .find(|&k| ds[k] == d && ts[k].is("SELECT"))
        else {
            return false;
        };
        let words = |w: &[&str]| {
            (select..from)
                .find(|&k| ds[k] == d && ts[k].k == IDENT && w.iter().any(|x| ts[k].is(x)))
        };
        if words(&["UNION", "EXCEPT", "INTERSECT"]).is_some() {
            return false;
        }
        let Some(f) = (select..from).find(|&k| ds[k] == d && ts[k].is("FROM")) else {
            return false;
        };
        let f_end = (f + 1..from)
            .find(|&k| {
                ds[k] == d
                    && ts[k].k == IDENT
                    && ["WHERE", "GROUP", "HAVING", "ORDER", "WINDOW", "OPTION"]
                        .iter()
                        .any(|w| ts[k].is(w))
            })
            .unwrap_or(from);
        // A select-list alias of that name is what a bare key means.
        if q.is_none()
            && split_commas(&ts[select + 1..f])
                .iter()
                .any(|it| item_alias(it).as_deref() == Some(col.as_str()))
        {
            return false;
        }
        let mut leaves = Vec::new();
        for item in split_commas(&ts[f + 1..f_end]) {
            let Ok(steps) = join_steps(item) else {
                return false;
            };
            leaves.extend(steps.into_iter().map(|s| (s.kind == Join::Left, s.leaf)));
        }
        let owner = match q {
            Some(q) => {
                let mut m = leaves.iter().filter(|(_, l)| match &l.alias {
                    Some(a) => a == q,
                    None => l.name.last() == Some(q),
                });
                match (m.next(), m.next()) {
                    (Some(l), None) => l,
                    _ => return false,
                }
            }
            None if leaves.len() == 1 => &leaves[0],
            None => {
                let mut owners = Vec::new();
                for l in &leaves {
                    if !l.1.table {
                        return false;
                    }
                    match self.table_columns(&l.1.name) {
                        Some(cs) if cs.contains(col) => owners.push(l),
                        Some(_) => {}
                        None => return false,
                    }
                }
                match owners.as_slice() {
                    [l] => *l,
                    _ => return false,
                }
            }
        };
        let (outer, leaf) = owner;
        if *outer || !leaf.table {
            return false;
        }
        self.catalog_table(&leaf.name)
            .and_then(|t| t.column(Dialect::Tsql, &Ident::plain(col.clone())))
            .is_some_and(|c| !c.nullable)
    }
}

/// The alias a select-list item gives its column: `x AS a`, `x a` or
/// `a = x` (lower case, unquoted).
fn item_alias(item: &[T]) -> Option<String> {
    let s: Vec<usize> = sig(item)
        .into_iter()
        .filter(|&i| !item[i].is("DISTINCT") && !item[i].is("ALL"))
        .collect();
    let name = |t: &T| match t.k {
        IDENT => Some(t.s.to_ascii_lowercase()),
        QUOTED_IDENT => Some(tsql_unquote(&t.s).to_ascii_lowercase()),
        STRING => Some(string_value(&t.s).to_ascii_lowercase()),
        _ => None,
    };
    if let [a, eq, ..] = s.as_slice() {
        if item[*eq].k == OP && item[*eq].s == "=" {
            return name(&item[*a]);
        }
    }
    match s.as_slice() {
        [.., p, l] if !matches!(item[*p].k, DOT) && !matches!(item[*p].k, OP) => name(&item[*l]),
        _ => None,
    }
}
