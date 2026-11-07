//! CROSS APPLY and OUTER APPLY: PostgreSQL's lateral joins.
//!
//! `a CROSS APPLY (SELECT …) x` runs the subquery once for each row of `a`
//! (it may name that row's columns) and keeps the row only when the subquery
//! returns one: `a CROSS JOIN LATERAL (SELECT …) x`. `a OUTER APPLY (SELECT …)
//! x` keeps the row with NULLs when the subquery returns none:
//! `a LEFT JOIN LATERAL (SELECT …) x ON TRUE`. A table-valued function on
//! the right (`CROSS APPLY dbo.f(a.id) x`) is the same join with the call as
//! the lateral item; the call itself is converted by its own rules. A table or
//! view on the right names no outer row, so it is a plain join:
//! `CROSS APPLY t x` is `CROSS JOIN t x`, `OUTER APPLY t x` is
//! `LEFT JOIN t x ON TRUE`.
//!
//! Only the keywords change here: the subquery is converted where the walk
//! reaches it (`TOP 1` becomes `LIMIT 1`, and so on). An APPLY the rules do
//! not read as `CROSS|OUTER APPLY (…)`, `CROSS|OUTER APPLY name(…)` or
//! `CROSS|OUTER APPLY name [alias]` (a table with `WITH (…)` hints is not) is
//! left as written, and Db2 for i has no rule yet.

use std::borrow::Cow;

use super::*;

/// The rule of an APPLY written as a lateral join.
pub(super) const APPLY: &str = "tsql.join.apply";

/// Words that start the next clause or join: none of them is the alias of the
/// item before it.
const NOT_ALIAS: &[&str] = &[
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "OUTER",
    "ON",
    "OPTION",
    "FOR",
    "WINDOW",
    "SET",
    "WITH",
    "USING",
    "OFFSET",
    "FETCH",
    "LIMIT",
    "PIVOT",
    "UNPIVOT",
    "TABLESAMPLE",
];

impl Cx<'_> {
    /// The tokens the expression walk converts: every `CROSS APPLY` and
    /// `OUTER APPLY` becomes the lateral join of the same meaning.
    pub(super) fn laterals<'t>(&mut self, ts: &'t [T]) -> Cow<'t, [T]> {
        if self.db2() || !ts.iter().any(|t| t.is("APPLY")) {
            return Cow::Borrowed(ts);
        }
        // (first word, APPLY, last token of the item, OUTER, lateral item)
        let mut found: Vec<(usize, usize, usize, bool, bool)> = Vec::new();
        for i in 0..ts.len() {
            if !ts[i].is("APPLY") {
                continue;
            }
            let Some(p) = prev_sig(ts, i) else { continue };
            let outer = ts[p].is("OUTER");
            if !(outer || ts[p].is("CROSS")) {
                continue;
            }
            // Only blanks between the two words, so that no comment is lost.
            if ts[p + 1..i].iter().any(|t| t.k != WHITESPACE) {
                continue;
            }
            let Some((end, lateral)) = item_end(ts, i) else {
                continue;
            };
            found.push((p, i, end, outer, lateral));
        }
        if found.is_empty() {
            return Cow::Borrowed(ts);
        }
        let mut v = Vec::with_capacity(ts.len() + found.len());
        for (k, t) in ts.iter().enumerate() {
            if let Some(&(p, i, _, outer, lateral)) = found.iter().find(|f| f.0 == k) {
                self.step_ts(APPLY, &ts[p..=i]);
                // A table or view on the right names no outer row: a plain join.
                let text = match (outer, lateral) {
                    (true, true) => "LEFT JOIN LATERAL",
                    (false, true) => "CROSS JOIN LATERAL",
                    (true, false) => "LEFT JOIN",
                    (false, false) => "CROSS JOIN",
                };
                v.push(T::made(IDENT, text, ts[p].pos.0, APPLY));
                continue;
            }
            // The blanks after the first word, and APPLY.
            if found.iter().any(|f| f.0 < k && k <= f.1) {
                continue;
            }
            v.push(t.clone());
            if found.iter().any(|f| f.2 == k && f.3) {
                v.push(T::made(IDENT, " ON TRUE", t.pos.1, APPLY));
            }
        }
        Cow::Owned(v)
    }
}

/// The last token of the item an APPLY takes, and whether it is a lateral
/// item (a subquery or a function call: `(…)`, `[schema.]name(…)`) or a table
/// or view that names no outer row (`[schema.]name`), then an optional
/// `[AS] alias` and an optional column list. A table with hints (`WITH (…)`)
/// is left as written.
fn item_end(ts: &[T], apply: usize) -> Option<(usize, bool)> {
    let name = |t: &T| matches!(t.k, IDENT | QUOTED_IDENT);
    let mut j = next_sig(ts, apply)?;
    let (mut end, lateral) = if ts[j].k == L_PAREN {
        (matching(ts, j)?, true)
    } else {
        // [schema.]name, then `(` for a function call.
        while name(&ts[j]) {
            match next_sig(ts, j) {
                Some(n) if ts[n].k == DOT => j = next_sig(ts, n)?,
                _ => break,
            }
        }
        if !name(&ts[j]) {
            return None;
        }
        match next_sig(ts, j).filter(|&n| ts[n].k == L_PAREN) {
            Some(open) => (matching(ts, open)?, true),
            None => (j, false),
        }
    };
    if let Some(k) = next_sig(ts, end) {
        let alias = if ts[k].is("AS") {
            next_sig(ts, k).filter(|&a| name(&ts[a]))
        } else if name(&ts[k]) && !NOT_ALIAS.iter().any(|w| ts[k].is(w)) {
            Some(k)
        } else {
            None
        };
        if let Some(a) = alias {
            end = a;
            if let Some(c) = next_sig(ts, a).filter(|&c| ts[c].k == L_PAREN) {
                end = matching(ts, c)?;
            }
        }
    }
    // Table hints stand after the alias, before any ON TRUE.
    if !lateral && next_sig(ts, end).is_some_and(|k| ts[k].is("WITH")) {
        return None;
    }
    Some((end, lateral))
}
