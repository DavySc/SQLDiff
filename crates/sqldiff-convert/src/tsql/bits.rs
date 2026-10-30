//! T-SQL bit next to numbers, for PostgreSQL's boolean.
//!
//! BIT becomes boolean (the default mapping, under `ASM-BIT-BOOLEAN`). T-SQL reads a bit as
//! the number 0 or 1; PostgreSQL has no comparison, arithmetic, assignment
//! or cast between boolean and the numbers except `integer::boolean` and
//! `boolean::integer` (checked on SQL Server 2022 and PostgreSQL 18,
//! 2026-10-26; eval/bit-boolean). So where a bit meets a number the output
//! is rewritten when an exact form exists:
//! - a comparison with 0 or 1, or a list of them: FALSE / TRUE (PostgreSQL
//!   orders false before true, as 0 before 1);
//! - storing a number in a bit (SET, VALUES, DEFAULT, a variable, RETURN, a
//!   procedure argument): 0 is FALSE and any other number TRUE, as T-SQL
//!   stores it; a numeric value `v` becomes `(v <> 0)`; a CASE stores each
//!   result so;
//! - a bit in arithmetic, CONCAT, or a cast to a type other than bit and int:
//!   `::integer`, the 0 or 1 T-SQL computes with;
//! - a CASE or IIF result of 0 or 1 next to a bit result: FALSE / TRUE;
//! - `~b` is `(NOT b)`, `a ^ b` of two bits `(a <> b)`, `a & b` and `a | b`
//!   of two bits `(a::integer & b::integer)::boolean` (NULL when an operand is
//!   NULL, as T-SQL; AND and OR are not, since `NULL AND FALSE` is FALSE where
//!   T-SQL's `NULL & 0` is NULL); with a number, the bit is `::integer`.
//!
//! Anything else (a bit against 2 or a numeric expression, a CASE mixing a
//! bit with other numbers) has no exact form and the unit is refused: the
//! engine's check would find it only where it knows the column types.

use super::*;

/// A CTE of the statement being converted: its name and column list
/// (lower case, unquoted) and its query's tokens.
pub(super) struct CteDef {
    pub name: String,
    pub cols: Option<Vec<String>>,
    pub body: Vec<T>,
}

/// The CTEs of a statement that starts with WITH.
pub(super) fn cte_defs(ts: &[T]) -> Vec<CteDef> {
    let Some(bodies) = super::cte_bodies(ts) else {
        return Vec::new();
    };
    bodies
        .into_iter()
        .map(|(name, body)| {
            // `name (cols) AS (`: the list before AS.
            let cols = body
                .start
                .checked_sub(1)
                .and_then(|open| prev_sig(ts, open))
                .and_then(|as_| prev_sig(ts, as_))
                .filter(|&c| ts[c].k == R_PAREN)
                .and_then(|c| opening(ts, c).map(|o| (o, c)))
                .map(|(o, c)| {
                    split_commas(&ts[o + 1..c])
                        .iter()
                        .map(|c| {
                            trimmed(c)
                                .iter()
                                .map(|t| tsql_unquote(&t.s).to_ascii_lowercase())
                                .collect()
                        })
                        .collect()
                });
            CteDef {
                name,
                cols,
                body: ts[body].to_vec(),
            }
        })
        .collect()
}

/// The index of the CTE `name` (one part) among the first `visible`.
fn cte_named(ctes: &[CteDef], name: &[String], visible: usize) -> Option<usize> {
    match name {
        [n] => ctes[..visible.min(ctes.len())]
            .iter()
            .rposition(|c| &c.name == n),
        _ => None,
    }
}

/// What a select item's values hold, as far as storing them in a bit goes.
#[derive(Clone, Copy, PartialEq)]
enum Held {
    Bit,
    Number,
}

const QUERY_END: [&str; 11] = [
    "FROM",
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "OPTION",
    "INTO",
    "FOR",
];

/// First and last significant token of each select list item.
type Items = Vec<(usize, usize)>;

/// The items of the select list after the SELECT at `sel`, and the word
/// that ends it, if one does. None for `SELECT TOP` and a `*` item.
fn select_items(ts: &[T], sel: usize) -> Option<(Items, Option<usize>)> {
    let ds = crate::tok::depths(ts);
    let d = ds[sel];
    let mut start = next_sig(ts, sel)?;
    if ts[start].is("DISTINCT") || ts[start].is("ALL") {
        start = next_sig(ts, start)?;
    }
    if ts[start].is("TOP") {
        return None;
    }
    let mut items = Vec::new();
    let mut first: Option<usize> = None;
    let mut last = start;
    let mut stop = None;
    for j in start..ts.len() {
        let t = &ts[j];
        if !t.sig() {
            continue;
        }
        if ds[j] < d || t.k == SEMICOLON {
            break;
        }
        if ds[j] == d && QUERY_END.iter().any(|w| t.is(w)) {
            stop = Some(j);
            break;
        }
        if ds[j] == d && t.k == COMMA {
            items.push((first.take()?, last));
            continue;
        }
        if t.k == OP && t.s == "*" && (first.is_none() || ts[last].k == DOT) {
            return None;
        }
        first.get_or_insert(j);
        last = j;
    }
    items.push((first?, last));
    Some((items, stop))
}

/// The expression of a select item: without `AS alias`, a trailing alias
/// or a leading `alias =`.
fn item_expr(ts: &[T], f: usize, l: usize) -> (usize, usize) {
    let named = |t: &T| {
        t.k == QUOTED_IDENT || t.k == IDENT && !sqldiff_core::normalize::is_keyword(&t.up())
    };
    if named(&ts[f]) {
        if let Some(eq) = next_sig(ts, f).filter(|&q| q < l && ts[q].k == OP && ts[q].s == "=") {
            if let Some(n) = next_sig(ts, eq) {
                return (n, l);
            }
        }
    }
    let Some(p) = prev_sig(ts, l).filter(|&p| p >= f) else {
        return (f, l);
    };
    if ts[p].is("AS") {
        return (f, prev_sig(ts, p).filter(|&b| b >= f).unwrap_or(f));
    }
    let before = &ts[p];
    if named(&ts[l])
        && (matches!(
            before.k,
            QUOTED_IDENT | R_PAREN | STRING | NUMBER | VARIABLE
        ) || before.k == IDENT && (named(before) || before.is("END")))
    {
        return (f, p);
    }
    (f, l)
}

/// The name a select item gives its column: its alias, or the column it
/// reads (lower case, unquoted).
fn item_name(ts: &[T], f: usize, l: usize) -> Option<String> {
    let (a, b) = item_expr(ts, f, l);
    if (a, b) != (f, l) {
        let n = if b < l { l } else { f };
        return Some(tsql_unquote(&ts[n].s).to_ascii_lowercase());
    }
    super::name_parts(&ts[f..=l]).and_then(|p| p.last().cloned())
}

/// The only FROM item after the FROM at `from` (its name parts and alias,
/// lower case): None for a join, several items, a derived table or hints.
fn single_from(ts: &[T], from: Option<usize>) -> Option<(Vec<String>, Option<String>)> {
    let from = from.filter(|&f| ts[f].is("FROM"))?;
    let ds = crate::tok::depths(ts);
    let d = ds[from];
    let mut j = next_sig(ts, from)?;
    let st = j;
    while let Some(dot) = next_sig(ts, j).filter(|&n| ts[n].k == DOT) {
        j = next_sig(ts, dot)?;
    }
    let name = super::name_parts(&ts[st..=j])?;
    let mut alias = None;
    let mut next = next_sig(ts, j);
    if next.is_some_and(|n| ts[n].is("AS")) {
        next = next.and_then(|n| next_sig(ts, n));
    }
    if let Some(n) = next.filter(|&n| {
        ts[n].k == QUOTED_IDENT
            || ts[n].k == IDENT && !sqldiff_core::normalize::is_keyword(&ts[n].up())
    }) {
        alias = Some(tsql_unquote(&ts[n].s).to_ascii_lowercase());
        next = next_sig(ts, n);
    }
    let ends = |n: usize| {
        ds[n] < d
            || ts[n].k == SEMICOLON
            || ds[n] == d
                && [
                    "WHERE",
                    "GROUP",
                    "HAVING",
                    "ORDER",
                    "UNION",
                    "EXCEPT",
                    "INTERSECT",
                    "OPTION",
                ]
                .iter()
                .any(|w| ts[n].is(w))
    };
    next.map_or(true, ends).then_some((name, alias))
}

/// Edits to one token slice, by token index.
#[derive(Default)]
pub(super) struct BitEdits {
    /// Output in place of the token.
    pub rep: HashMap<usize, String>,
    /// Output before the token.
    pub pre: HashMap<usize, String>,
    /// Output after the token.
    pub post: HashMap<usize, String>,
    /// The rule of each rewrite and the tokens it rewrote (first and last
    /// index): the expression walk that applies the edits records them as
    /// steps.
    pub steps: Vec<(&'static str, usize, usize)>,
}

impl BitEdits {
    /// Adds the edits of an enclosing rewrite: its text goes outside ours.
    pub fn wrap(&mut self, outer: BitEdits) {
        self.steps.extend(outer.steps);
        self.rep.extend(outer.rep);
        for (k, p) in outer.pre {
            let inner = self.pre.remove(&k).unwrap_or_default();
            self.pre.insert(k, p + &inner);
        }
        for (k, p) in outer.post {
            self.post.entry(k).or_default().push_str(&p);
        }
    }
}

/// `(` matching the `)` at `close`.
pub(super) fn opening(ts: &[T], close: usize) -> Option<usize> {
    let mut depth = 0i32;
    for j in (0..=close).rev() {
        match ts[j].k {
            R_PAREN => depth += 1,
            L_PAREN => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// First token of the simple operand that ends at `j`: a variable, a
/// literal, a dotted name, a call or a parenthesized group.
pub(super) fn operand_start(ts: &[T], j: usize) -> Option<usize> {
    match ts[j].k {
        VARIABLE | NUMBER | STRING => Some(j),
        IDENT | QUOTED_IDENT => {
            let mut st = j;
            while let Some(d) = prev_sig(ts, st).filter(|&d| ts[d].k == DOT) {
                match prev_sig(ts, d) {
                    Some(q) if matches!(ts[q].k, IDENT | QUOTED_IDENT) => st = q,
                    _ => break,
                }
            }
            Some(st)
        }
        R_PAREN => {
            let open = opening(ts, j)?;
            match prev_sig(ts, open) {
                // A call: the function name (keywords like IN or AND are not).
                Some(f)
                    if ts[f].k == IDENT && !sqldiff_core::normalize::is_keyword(&ts[f].up())
                        || ts[f].is("CAST")
                        || ts[f].is("CONVERT")
                        || ts[f].is("COALESCE")
                        || ts[f].is("ISNULL") =>
                {
                    Some(f)
                }
                _ => Some(open),
            }
        }
        _ => None,
    }
}

/// Last token of the simple operand that starts at `j`.
fn operand_end(ts: &[T], j: usize) -> Option<usize> {
    match ts[j].k {
        VARIABLE | NUMBER | STRING => Some(j),
        IDENT | QUOTED_IDENT => {
            let mut en = j;
            while let Some(d) = next_sig(ts, en).filter(|&d| ts[d].k == DOT) {
                match next_sig(ts, d) {
                    Some(q) if matches!(ts[q].k, IDENT | QUOTED_IDENT) => en = q,
                    _ => break,
                }
            }
            match next_sig(ts, en).filter(|&p| ts[p].k == L_PAREN) {
                Some(p) if en == j => matching(ts, p),
                _ => Some(en),
            }
        }
        L_PAREN => matching(ts, j),
        _ => None,
    }
}

/// The value of an integer literal, if the tokens are one.
fn int_literal(ts: &[T]) -> Option<i128> {
    match sig(ts).as_slice() {
        [i] if ts[*i].k == NUMBER => ts[*i].s.parse().ok(),
        _ => None,
    }
}

/// The 0 or 1 a string literal is as a T-SQL bit, for the ones modelled.
fn bit_string(ts: &[T]) -> Option<&'static str> {
    let [i] = sig(ts)[..] else { return None };
    if ts[i].k != STRING {
        return None;
    }
    let s = &ts[i].s;
    let v = string_value(s.strip_prefix(['N', 'n']).unwrap_or(s));
    match v.trim_matches(' ').to_ascii_lowercase().as_str() {
        "true" | "1" => Some("1"),
        "false" | "0" => Some("0"),
        _ => None,
    }
}

/// A T-SQL bit literal string PostgreSQL's boolean reads the same way.
fn bool_string(s: &str) -> bool {
    let v = string_value(s.strip_prefix(['N', 'n']).unwrap_or(s));
    matches!(
        v.trim_matches(' ').to_ascii_lowercase().as_str(),
        "true" | "false" | "1" | "0"
    )
}

const ARITH: [&str; 5] = ["+", "-", "*", "/", "%"];
const CMP: [&str; 7] = ["=", "<>", "!=", "<", ">", "<=", ">="];

/// The END of the CASE at `i`.
fn case_end(ts: &[T], i: usize) -> Option<usize> {
    let ds = crate::tok::depths(ts);
    let mut nested = 0;
    for j in sig(ts).into_iter().filter(|&j| j > i && ds[j] == ds[i]) {
        if ts[j].is("CASE") {
            nested += 1;
        } else if ts[j].is("END") {
            if nested == 0 {
                return Some(j);
            }
            nested -= 1;
        }
    }
    None
}

/// The result expressions (first and last significant token) of the CASE at
/// `i`: what follows each THEN and the ELSE.
fn case_results(ts: &[T], i: usize) -> Vec<(usize, usize)> {
    let ds = crate::tok::depths(ts);
    let mut out = Vec::new();
    let mut nested = 0;
    let mut start: Option<usize> = None;
    let mut last = i;
    for j in sig(ts).into_iter().filter(|&j| j > i && ds[j] == ds[i]) {
        let t = &ts[j];
        let ends = nested == 0 && (t.is("WHEN") || t.is("ELSE") || t.is("END"));
        if ends {
            if let Some(st) = start.take() {
                out.push((st, last));
            }
            if t.is("END") {
                break;
            }
        }
        if t.is("CASE") {
            nested += 1;
        } else if t.is("END") {
            nested -= 1;
        }
        if nested == 0 && (t.is("THEN") || t.is("ELSE")) {
            start = next_sig(ts, j);
        }
        last = match t.k {
            L_PAREN => matching(ts, j).unwrap_or(j),
            _ => j,
        };
    }
    out
}

impl Cx<'_> {
    /// Whether the tokens are a T-SQL bit value: a bit variable, column or
    /// CAST.
    pub(super) fn is_bit(&self, ts: &[T]) -> bool {
        if self.expr_dt(ts) == Some(Dt::Bit) {
            return true;
        }
        let name = |t: &T| tsql_unquote(&t.s).to_ascii_lowercase();
        match sig(ts).as_slice() {
            // A column of the table being created or altered, or a name the
            // statement gave a bit.
            [i] if matches!(ts[*i].k, IDENT | QUOTED_IDENT) => {
                self.bit_cols.contains(&name(&ts[*i]))
                    || self.expr_dt(ts).is_none() && self.bit_aliases.contains(&name(&ts[*i]))
            }
            // `d.a`, a derived table's column.
            [q, d, i]
                if matches!(ts[*q].k, IDENT | QUOTED_IDENT)
                    && ts[*d].k == DOT
                    && matches!(ts[*i].k, IDENT | QUOTED_IDENT) =>
            {
                self.expr_dt(ts).is_none() && self.bit_aliases.contains(&name(&ts[*i]))
            }
            _ => false,
        }
    }

    /// The bit columns of every table `ts` names, by column name.
    fn named_bit_cols(&self, ts: &[T]) -> Vec<String> {
        let s = sig(ts);
        let mut cols: Vec<String> = Vec::new();
        let mut k = 0;
        while k < s.len() {
            let mut e = k;
            while e + 2 < s.len()
                && ts[s[e + 1]].k == DOT
                && matches!(ts[s[e + 2]].k, IDENT | QUOTED_IDENT)
            {
                e += 2;
            }
            if matches!(ts[s[k]].k, IDENT | QUOTED_IDENT | VARIABLE) {
                cols.extend(self.table_bit_cols(&ts[s[k]..=s[e]]));
            }
            k = e + 1;
        }
        cols
    }

    /// The names a select list in `ts` gives a bit: `x AS a`, `x a`.
    pub(super) fn bit_aliases_of(&self, ts: &[T]) -> Vec<String> {
        let mut out = Vec::new();
        if !self.bits_possible() || self.db2() {
            return out;
        }
        let cols = self.named_bit_cols(ts);
        let s = sig(ts);
        let lower = |t: &T| tsql_unquote(&t.s).to_ascii_lowercase();
        for (k, &i) in s.iter().enumerate() {
            // A bit column selected under its own name (`SELECT Id, IsActive FROM`).
            if matches!(ts[i].k, IDENT | QUOTED_IDENT)
                && cols.contains(&lower(&ts[i]))
                && k > 0
                && (ts[s[k - 1]].k == COMMA || ts[s[k - 1]].is("SELECT"))
                && s.get(k + 1)
                    .is_some_and(|&n| ts[n].k == COMMA || ts[n].is("FROM"))
            {
                out.push(lower(&ts[i]));
                continue;
            }
            let (alias, before) = if ts[i].is("AS") {
                match (s.get(k + 1), k.checked_sub(1).map(|p| s[p])) {
                    (Some(&a), Some(b)) => (a, b),
                    _ => continue,
                }
            } else if matches!(ts[i].k, IDENT | QUOTED_IDENT)
                && !sqldiff_core::normalize::is_keyword(&ts[i].up())
                && s.get(k + 1)
                    .is_some_and(|&n| ts[n].k == COMMA || ts[n].is("FROM"))
                && k > 0
                && matches!(ts[s[k - 1]].k, IDENT | QUOTED_IDENT | R_PAREN)
                && !ts[s[k - 1]].is("SELECT")
            {
                (i, s[k - 1])
            } else {
                continue;
            };
            if !matches!(ts[alias].k, IDENT | QUOTED_IDENT) {
                continue;
            }
            if let Some(st) = operand_start(ts, before) {
                let x = &ts[st..=before];
                // A column named like a bit column of a table here, or a typed bit.
                let named = x
                    .last()
                    .filter(|t| matches!(t.k, IDENT | QUOTED_IDENT))
                    .is_some_and(|t| cols.contains(&tsql_unquote(&t.s).to_ascii_lowercase()));
                if named || self.expr_dt(x) == Some(Dt::Bit) {
                    out.push(tsql_unquote(&ts[alias].s).to_ascii_lowercase());
                }
            }
        }
        out
    }

    fn is_numeric(&self, ts: &[T]) -> bool {
        matches!(
            self.expr_dt(ts),
            Some(Dt::Int | Dt::BigInt | Dt::Fraction | Dt::Money)
        )
    }

    fn unhandled(&mut self, ts: &[T]) {
        let text: String = ts.iter().map(|t| t.s.as_str()).collect();
        let text = collapse_spaces(text.trim());
        if !self.bit_unhandled.contains(&text) {
            self.bit_unhandled.push(text);
        }
    }

    /// A value stored in a bit: the replacement for the tokens at
    /// `at..=end`, or false when there is no exact one.
    fn store(&mut self, ts: &[T], at: usize, end: usize, e: &mut BitEdits) -> bool {
        let v = &ts[at..=end];
        // A CASE stores the value of the branch taken: each result is stored
        // on its own. A string result would be converted to the CASE's type
        // first when another result is a number, so none may be a string.
        if ts[at].is("CASE") && ts[end].is("END") && case_end(ts, at) == Some(end) {
            let rs = case_results(ts, at);
            if rs.iter().any(|&(f, l)| f == l && ts[f].k == STRING) {
                return false;
            }
            return rs.into_iter().all(|(f, l)| self.store(ts, f, l, e));
        }
        if self.db2() {
            // SMALLINT: the 0 or 1 T-SQL stores.
            if let Some(n) = int_literal(v) {
                e.rep.insert(at, if n == 0 { "0" } else { "1" }.into());
                e.steps.push(("tsql.bit.store", at, end));
            } else if let Some(b) = bit_string(v) {
                e.rep.insert(at, b.into());
                e.steps.push(("tsql.bit.store", at, end));
            }
            return true;
        }
        if let Some(n) = int_literal(v) {
            e.rep
                .insert(at, if n == 0 { "FALSE" } else { "TRUE" }.into());
            e.steps.push(("tsql.bit.store", at, end));
            return true;
        }
        if self.is_bit(v) || v.iter().all(|t| !t.sig() || t.is("NULL")) {
            return true;
        }
        if let [i] = sig(v).as_slice() {
            if v[*i].k == STRING {
                return bool_string(&v[*i].s);
            }
        }
        if self.is_numeric(v) {
            e.pre.insert(at, "(".into());
            e.post.insert(end, " <> 0)".into());
            e.steps.push(("tsql.bit.store", at, end));
            return true;
        }
        false
    }

    /// The results of a CASE or IIF where one is a bit: T-SQL's result is a
    /// number when another is, so only the literals 0 and 1 have a boolean of
    /// the same value (FALSE / TRUE, the replacement given here). A number
    /// of any other kind cannot become a boolean and is reported.
    pub(super) fn bit_results(&mut self, rs: &[&[T]]) -> Vec<Option<&'static str>> {
        let none = vec![None; rs.len()];
        if !rs.iter().any(|r| self.is_bit(r)) {
            return none;
        }
        rs.iter()
            .map(|r| match int_literal(r) {
                Some(0) => Some("FALSE"),
                Some(1) => Some("TRUE"),
                Some(_) => {
                    self.unhandled(r);
                    None
                }
                None => {
                    if !self.is_bit(r) && self.is_numeric(r) {
                        self.unhandled(r);
                    }
                    None
                }
            })
            .collect()
    }

    /// Whether the `=` at `i`, whose left operand starts at `ls`, assigns
    /// (an UPDATE or MERGE SET list, `SELECT @v = …`) rather than compares.
    fn assigns(&self, ts: &[T], ls: usize) -> bool {
        let ds = crate::tok::depths(ts);
        let var = ts[ls].k == VARIABLE;
        let mut j = ls;
        while let Some(p) = prev_sig(ts, j) {
            if ds[p] < ds[ls] {
                return false;
            }
            j = p;
            if ds[p] > ds[ls] {
                continue;
            }
            let t = &ts[p];
            if t.is("SET") {
                return true;
            }
            if t.is("SELECT") {
                return var;
            }
            const STOP: [&str; 16] = [
                "WHERE", "FROM", "ON", "WHEN", "THEN", "ELSE", "AND", "OR", "NOT", "IF", "WHILE",
                "RETURN", "HAVING", "CASE", "JOIN", "BY",
            ];
            if STOP.iter().any(|w| t.is(w)) {
                return false;
            }
        }
        false
    }

    /// The bit rewrites of one token slice (PostgreSQL only).
    pub(super) fn bit_edits(&mut self, ts: &[T]) -> BitEdits {
        let mut e = BitEdits::default();
        if !self.bits_possible() {
            return e;
        }
        let s = sig(ts);
        if self.db2() {
            self.bit_edits_db2(ts, &s, &mut e);
            return e;
        }
        for &i in &s {
            let t = &ts[i];
            match t.k {
                OP if CMP.contains(&t.s.as_str()) => self.bit_cmp(ts, i, &mut e),
                OP if ARITH.contains(&t.s.as_str()) => {
                    // Binary only: an operand on both sides.
                    let (Some(lp), Some(rn)) = (prev_sig(ts, i), next_sig(ts, i)) else {
                        continue;
                    };
                    if matches!(ts[lp].k, OP | L_PAREN | COMMA)
                        || ts[lp].k == IDENT && is_clause_word(&ts[lp].s)
                    {
                        continue;
                    }
                    let spans = [
                        (operand_start(ts, lp), Some(lp)),
                        (Some(rn), operand_end(ts, rn)),
                    ];
                    // SQL Server rejects arithmetic between two bits: no output claims it.
                    if let [(Some(a), Some(b)), (Some(c), Some(d))] = spans {
                        if self.is_bit(&ts[a..=b]) && self.is_bit(&ts[c..=d]) {
                            self.unhandled(&ts[a..=d]);
                            continue;
                        }
                    }
                    for (st, en) in spans {
                        if let (Some(st), Some(en)) = (st, en) {
                            if self.is_bit(&ts[st..=en]) {
                                e.post.insert(en, "::integer".into());
                                e.steps.push(("tsql.bit.arithmetic", st, en));
                            }
                        }
                    }
                }
                OP if t.s == "~" => {
                    if let Some(en) =
                        next_sig(ts, i).and_then(|n| operand_end(ts, n).map(|en| (n, en)))
                    {
                        if self.is_bit(&ts[en.0..=en.1]) {
                            // NOT binds more loosely than ~.
                            e.rep.insert(i, "(NOT ".into());
                            e.post.insert(en.1, ")".into());
                            e.steps.push(("tsql.bit.not", i, en.1));
                        } else {
                            self.unhandled(&ts[i..=en.1]);
                        }
                    }
                }
                OP if matches!(t.s.as_str(), "^" | "&" | "|") => {
                    let (Some(lp), Some(rn)) = (prev_sig(ts, i), next_sig(ts, i)) else {
                        continue;
                    };
                    let (Some(ls), Some(re)) = (operand_start(ts, lp), operand_end(ts, rn)) else {
                        continue;
                    };
                    let (l, r) = (self.is_bit(&ts[ls..=lp]), self.is_bit(&ts[rn..=re]));
                    if t.s == "^" && l && r {
                        // ^ binds more tightly than a comparison; <> does not.
                        e.rep.insert(i, "<>".into());
                        e.pre.insert(ls, "(".into());
                        e.post.insert(re, ")".into());
                        e.steps.push(("tsql.bit.bitwise", ls, re));
                    } else if l || r {
                        // On integers, & and | treat NULL as T-SQL does on
                        // bits; PostgreSQL's ^ is a power, # its exclusive or.
                        e.steps.push(("tsql.bit.bitwise", ls, re));
                        if t.s == "^" {
                            e.rep.insert(i, "#".into());
                        }
                        for (bit, end) in [(l, lp), (r, re)] {
                            if bit {
                                e.post.insert(end, "::integer".into());
                            }
                        }
                        if l && r {
                            e.pre.insert(ls, "(".into());
                            e.post.insert(re, "::integer)::boolean".into());
                        }
                    }
                }
                IDENT if t.is("CASE") => {
                    let spans = case_results(ts, i);
                    let rs: Vec<&[T]> = spans.iter().map(|&(f, l)| &ts[f..=l]).collect();
                    for (&(f, l), r) in spans.iter().zip(self.bit_results(&rs)) {
                        if let Some(r) = r {
                            e.rep.insert(f, r.into());
                            e.steps.push(("tsql.bit.case-result", f, l));
                        }
                    }
                }
                IDENT if t.is("BETWEEN") => self.bit_between(ts, i, &mut e),
                IDENT if t.is("IN") => self.bit_in(ts, i, &mut e),
                IDENT if t.is("VALUES") => self.bit_values(ts, i, &mut e),
                IDENT if t.is("SELECT") => self.bit_insert_select(ts, i, &mut e),
                IDENT if t.is("COALESCE") || t.is("CONCAT") => {
                    let Some(p) = next_sig(ts, i).filter(|&p| ts[p].k == L_PAREN) else {
                        continue;
                    };
                    let Some(close) = matching(ts, p) else {
                        continue;
                    };
                    let args = split_commas(&ts[p + 1..close]);
                    let mut at = p + 1;
                    let mut spans = Vec::new();
                    for a in &args {
                        let sg = sig(a);
                        if let (Some(&f), Some(&l)) = (sg.first(), sg.last()) {
                            spans.push((at + f, at + l));
                        }
                        at += a.len() + 1;
                    }
                    if t.is("CONCAT") {
                        // CONCAT writes a bit as 1 / 0 and a boolean as t / f.
                        for &(f, l) in &spans {
                            if self.is_bit(&ts[f..=l]) {
                                e.post.insert(l, "::integer".into());
                                e.steps.push(("tsql.bit.concat", f, l));
                            }
                        }
                        continue;
                    }
                    // COALESCE(bit, 0): T-SQL's result is a number, so only
                    // 0 and 1 have a boolean of the same value.
                    if spans.first().is_some_and(|&(f, l)| self.is_bit(&ts[f..=l])) {
                        for &(f, l) in &spans[1..] {
                            match int_literal(&ts[f..=l]) {
                                Some(0) => {
                                    e.rep.insert(f, "FALSE".into());
                                    e.steps.push(("tsql.bit.coalesce", f, l));
                                }
                                Some(1) => {
                                    e.rep.insert(f, "TRUE".into());
                                    e.steps.push(("tsql.bit.coalesce", f, l));
                                }
                                _ if self.is_bit(&ts[f..=l]) || ts[f].is("NULL") => {}
                                _ => self.unhandled(&ts[i..=close]),
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        e
    }

    /// Whether any bit can be in play: a bit variable, a column of the table
    /// being defined, a bit column of a table variable or `#` table, or a
    /// bit column in the catalog.
    fn bits_possible(&self) -> bool {
        !self.bit_cols.is_empty()
            || self.vars.values().any(|v| v.dt == Dt::Bit)
            || self
                .own_tables
                .iter()
                .any(|t| t.columns.iter().any(|c| c.data_type.base == "BIT"))
            || self.type_catalog().is_some_and(|c| {
                c.tables
                    .iter()
                    .any(|t| t.columns.iter().any(|c| c.data_type.base == "BIT"))
            })
    }

    fn bit_cmp(&mut self, ts: &[T], i: usize, e: &mut BitEdits) {
        let (Some(lp), Some(rn)) = (prev_sig(ts, i), next_sig(ts, i)) else {
            return;
        };
        let (Some(ls), Some(re)) = (operand_start(ts, lp), operand_end(ts, rn)) else {
            return;
        };
        let (l, r) = (self.is_bit(&ts[ls..=lp]), self.is_bit(&ts[rn..=re]));
        if l == r {
            return;
        }
        if ts[i].s == "=" && l && self.assigns(ts, ls) {
            // The value runs to the next comma or clause at this depth.
            let ds = crate::tok::depths(ts);
            let mut end = match ts[rn].is("CASE") {
                true => case_end(ts, rn).unwrap_or(re),
                false => re,
            };
            let mut j = end;
            while let Some(n) = next_sig(ts, j) {
                if ds[n] < ds[rn]
                    || ds[n] == ds[rn]
                        && (ts[n].k == COMMA
                            || ts[n].k == IDENT && is_clause_word(&ts[n].s)
                            || ts[n].is("OUTPUT")
                            || ts[n].is("WHEN"))
                {
                    break;
                }
                // A CASE's own WHEN is not a MERGE clause.
                end = match ts[n].is("CASE") {
                    true => case_end(ts, n).unwrap_or(n),
                    false => n,
                };
                j = end;
            }
            if !self.store(ts, rn, end, e) {
                self.unhandled(&ts[ls..=end]);
            }
            return;
        }
        let (os, oe) = if l { (rn, re) } else { (ls, lp) };
        let other = &ts[os..=oe];
        match int_literal(other) {
            Some(0) => {
                e.rep.insert(os, "FALSE".into());
                e.steps.push(("tsql.bit.compare", os, oe));
                return;
            }
            Some(1) => {
                e.rep.insert(os, "TRUE".into());
                e.steps.push(("tsql.bit.compare", os, oe));
                return;
            }
            _ => {}
        }
        if let [k] = sig(other).as_slice() {
            if other[*k].k == STRING && bool_string(&other[*k].s) || other[*k].is("NULL") {
                return;
            }
        }
        self.unhandled(&ts[ls..=re]);
    }

    /// `b BETWEEN lo AND hi` on a bit: bounds of 0 and 1 are FALSE and TRUE.
    fn bit_between(&mut self, ts: &[T], i: usize, e: &mut BitEdits) {
        let Some(lp) = prev_sig(ts, i)
            .filter(|&p| !ts[p].is("NOT"))
            .or_else(|| prev_sig(ts, i).and_then(|p| prev_sig(ts, p)))
        else {
            return;
        };
        let Some(ls) = operand_start(ts, lp) else {
            return;
        };
        let lo = next_sig(ts, i).and_then(|f| operand_end(ts, f).map(|l| (f, l)));
        let and = lo
            .and_then(|(_, l)| next_sig(ts, l))
            .filter(|&a| ts[a].is("AND"));
        let hi = and
            .and_then(|a| next_sig(ts, a))
            .and_then(|f| operand_end(ts, f).map(|l| (f, l)));
        let x = &ts[ls..=lp];
        let (Some(lo), Some(hi)) = (lo, hi) else {
            if self.is_bit(x) {
                self.unhandled(&ts[ls..=i]);
            }
            return;
        };
        let bounds = [lo, hi];
        if !self.is_bit(x) {
            if bounds.iter().any(|&(f, l)| self.is_bit(&ts[f..=l])) {
                self.unhandled(&ts[ls..=hi.1]);
            }
            return;
        }
        for (f, l) in bounds {
            let b = &ts[f..=l];
            match int_literal(b) {
                Some(0) => {
                    e.rep.insert(f, "FALSE".into());
                    e.steps.push(("tsql.bit.between", f, l));
                }
                Some(1) => {
                    e.rep.insert(f, "TRUE".into());
                    e.steps.push(("tsql.bit.between", f, l));
                }
                _ if self.is_bit(b)
                    || f == l
                        && (ts[f].is("NULL") || ts[f].k == STRING && bool_string(&ts[f].s)) => {}
                _ => self.unhandled(&ts[ls..=hi.1]),
            }
        }
    }

    fn bit_in(&mut self, ts: &[T], i: usize, e: &mut BitEdits) {
        let Some(mut lp) = prev_sig(ts, i) else {
            return;
        };
        if ts[lp].is("NOT") {
            match prev_sig(ts, lp) {
                Some(p) => lp = p,
                None => return,
            }
        }
        let Some(ls) = operand_start(ts, lp) else {
            return;
        };
        if !self.is_bit(&ts[ls..=lp]) {
            return;
        }
        let Some(open) = next_sig(ts, i).filter(|&p| ts[p].k == L_PAREN) else {
            return;
        };
        let Some(close) = matching(ts, open) else {
            return;
        };
        if next_sig(ts, open).is_some_and(|f| ts[f].is("SELECT")) {
            return;
        }
        let mut at = open + 1;
        let mut reps = Vec::new();
        for a in split_commas(&ts[open + 1..close]) {
            let sg = sig(a);
            match (int_literal(a), sg.first()) {
                (Some(0), Some(&f)) => reps.push((at + f, at + sg[sg.len() - 1], "FALSE")),
                (Some(1), Some(&f)) => reps.push((at + f, at + sg[sg.len() - 1], "TRUE")),
                _ if self.is_bit(a) => {}
                _ => {
                    self.unhandled(&ts[ls..=close]);
                    return;
                }
            }
            at += a.len() + 1;
        }
        for (k, l, v) in reps {
            e.rep.insert(k, v.into());
            e.steps.push(("tsql.bit.in", k, l));
        }
    }

    /// Which columns of the INSERT column list that ends at `close` are bit
    /// columns, by position; None when the table or a column is not known
    /// or none is a bit.
    fn insert_bit_cols(&self, ts: &[T], close: usize) -> Option<Vec<bool>> {
        use sqldiff_core::catalog::Ident;
        let open = opening(ts, close)?;
        let name_end = prev_sig(ts, open)?;
        let name_start = operand_start(ts, name_end)?;
        let parts: Vec<String> = ts[name_start..=name_end]
            .iter()
            .filter(|t| matches!(t.k, IDENT | QUOTED_IDENT | VARIABLE))
            .map(|t| tsql_unquote(&t.s).to_ascii_lowercase())
            .collect();
        let entry = ScopeEntry {
            name: parts,
            alias: None,
            named: true,
        };
        let table = self.entry_table(&entry)?;
        let bit: Vec<bool> = split_commas(&ts[open + 1..close])
            .iter()
            .map(|c| {
                let c: String = trimmed(c).iter().map(|t| tsql_unquote(&t.s)).collect();
                table
                    .column(Dialect::Tsql, &Ident::plain(c))
                    .is_some_and(|c| c.data_type.base == "BIT")
            })
            .collect();
        bit.contains(&true).then_some(bit)
    }

    /// `INSERT [INTO] t (cols) VALUES (…), (…)`: values stored in bit columns.
    fn bit_values(&mut self, ts: &[T], i: usize, e: &mut BitEdits) {
        let Some(close) = prev_sig(ts, i).filter(|&c| ts[c].k == R_PAREN) else {
            return;
        };
        let Some(bit) = self.insert_bit_cols(ts, close) else {
            return;
        };
        let mut j = i;
        while let Some(row) = next_sig(ts, j).filter(|&p| ts[p].k == L_PAREN) {
            let Some(rend) = matching(ts, row) else {
                return;
            };
            let mut at = row + 1;
            for (k, a) in split_commas(&ts[row + 1..rend]).into_iter().enumerate() {
                let sg = sig(a);
                if let (true, Some(&f), Some(&l)) =
                    (bit.get(k) == Some(&true), sg.first(), sg.last())
                {
                    if !self.store(ts, at + f, at + l, e) {
                        self.unhandled(&ts[at + f..=at + l]);
                    }
                }
                at += a.len() + 1;
            }
            match next_sig(ts, rend).filter(|&c| ts[c].k == COMMA) {
                Some(c) => j = c,
                None => break,
            }
        }
    }

    /// `INSERT [INTO] t (cols) SELECT …`: the select items stored in bit
    /// columns, in each branch of a UNION. An item that reads a column of
    /// one of the statement's CTEs is stored by what that column holds (a
    /// number becomes `(c <> 0)`, as T-SQL stores it in a bit); any other
    /// item as a value in VALUES. An item no rule can store stays as it
    /// was: the engine compares the statement.
    fn bit_insert_select(&mut self, ts: &[T], i: usize, e: &mut BitEdits) {
        let Some(close) = prev_sig(ts, i).filter(|&c| ts[c].k == R_PAREN) else {
            return;
        };
        let Some(name) = opening(ts, close)
            .and_then(|o| prev_sig(ts, o))
            .and_then(|n| operand_start(ts, n))
        else {
            return;
        };
        // `INSERT t (…)` reaches here from the table name: its INSERT is
        // written by the statement (as INSERT INTO).
        match prev_sig(ts, name) {
            Some(k) if ts[k].is("INTO") || ts[k].is("INSERT") => {}
            None if self.stmt_insert => {}
            _ => return,
        }
        let Some(bit) = self.insert_bit_cols(ts, close) else {
            return;
        };
        let ctes = std::mem::take(&mut self.stmt_ctes);
        let mut sel = i;
        while let Some((items, stop)) = select_items(ts, sel) {
            let from = single_from(ts, stop);
            for (k, &(f, l)) in items.iter().enumerate() {
                if bit.get(k) != Some(&true) {
                    continue;
                }
                let (f, l) = item_expr(ts, f, l);
                // A column read from a CTE; a literal or CASE is stored as
                // in VALUES.
                let cte = super::name_parts(&ts[f..=l]).is_some()
                    && from
                        .as_ref()
                        .is_some_and(|(n, _)| cte_named(&ctes, n, ctes.len()).is_some());
                if cte {
                    match self.held(&ctes, ctes.len(), ts, f, l, from.as_ref(), 0) {
                        Some(Held::Bit) => continue,
                        Some(Held::Number) => {
                            e.pre.insert(f, "(".into());
                            e.post.insert(l, " <> 0)".into());
                            e.steps.push(("tsql.bit.store", f, l));
                            continue;
                        }
                        None => {}
                    }
                }
                // Applied only when every part of the value has a rewrite.
                let mut one = BitEdits::default();
                if self.store(ts, f, l, &mut one) {
                    e.wrap(one);
                }
            }
            // The next branch of a UNION, EXCEPT or INTERSECT.
            let Some(op) =
                stop.filter(|&o| ["UNION", "EXCEPT", "INTERSECT"].iter().any(|w| ts[o].is(w)))
            else {
                break;
            };
            let next = next_sig(ts, op)
                .filter(|&n| !ts[n].is("ALL"))
                .or_else(|| next_sig(ts, op).and_then(|a| next_sig(ts, a)));
            match next.filter(|&n| ts[n].is("SELECT")) {
                Some(n) => sel = n,
                None => break,
            }
        }
        self.stmt_ctes = ctes;
    }

    /// What the values of the select item `ts[f..=l]` hold, read through
    /// the statement's first `visible` CTEs and the catalog: a bit, a
    /// number, or not known (None). `from` is the item's query's only FROM
    /// item (name, alias).
    #[allow(clippy::too_many_arguments)]
    fn held(
        &self,
        ctes: &[CteDef],
        visible: usize,
        ts: &[T],
        f: usize,
        l: usize,
        from: Option<&(Vec<String>, Option<String>)>,
        depth: u32,
    ) -> Option<Held> {
        if depth > 32 {
            return None;
        }
        let (mut f, mut l) = (f, l);
        while ts[f].k == L_PAREN && matching(ts, f) == Some(l) {
            (f, l) = (next_sig(ts, f)?, prev_sig(ts, l)?);
        }
        let v = &ts[f..=l];
        if let [i] = sig(v).as_slice() {
            match v[*i].k {
                NUMBER => return Some(Held::Number),
                VARIABLE => {
                    return match self.expr_dt(v)? {
                        Dt::Bit => Some(Held::Bit),
                        Dt::Int | Dt::BigInt | Dt::Fraction | Dt::Money => Some(Held::Number),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
        // A CASE: what every result other than NULL holds.
        if ts[f].is("CASE") && case_end(ts, f) == Some(l) {
            let mut all = None;
            for (rf, rl) in case_results(ts, f) {
                if rf == rl && ts[rf].is("NULL") {
                    continue;
                }
                let h = self.held(ctes, visible, ts, rf, rl, from, depth + 1)?;
                if all.is_some_and(|a| a != h) {
                    return None;
                }
                all = Some(h);
            }
            return all;
        }
        if ts[f].is("CAST") || ts[f].is("CONVERT") {
            return match self.expr_dt(v)? {
                Dt::Bit => Some(Held::Bit),
                Dt::Int | Dt::BigInt | Dt::Fraction | Dt::Money => Some(Held::Number),
                _ => None,
            };
        }
        // A column of the FROM item: a CTE before this query, or a table.
        let parts = super::name_parts(v)?;
        let (fname, falias) = from?;
        let (q, col) = match parts.as_slice() {
            [c] => (None, c),
            [q, c] => (Some(q), c),
            _ => return None,
        };
        if q.is_some_and(|q| match falias {
            Some(a) => a != q,
            None => fname.last() != Some(q),
        }) {
            return None;
        }
        if let Some(k) = cte_named(ctes, fname, visible) {
            let body = &ctes[k].body;
            let sel = sig(body)
                .first()
                .copied()
                .filter(|&s| body[s].is("SELECT"))?;
            let (items, stop) = select_items(body, sel)?;
            // One query: a UNION's column holds what each branch does.
            if stop.is_some_and(|o| {
                ["UNION", "EXCEPT", "INTERSECT"]
                    .iter()
                    .any(|w| body[o].is(w))
            }) {
                return None;
            }
            let at = match &ctes[k].cols {
                Some(cols) => cols.iter().position(|c| c == col)?,
                None => {
                    let named: Vec<usize> = items
                        .iter()
                        .enumerate()
                        .filter(|(_, &(a, b))| item_name(body, a, b).as_ref() == Some(col))
                        .map(|(n, _)| n)
                        .collect();
                    match named.as_slice() {
                        [n] => *n,
                        _ => return None,
                    }
                }
            };
            if ctes[k]
                .cols
                .as_ref()
                .is_some_and(|c| c.len() != items.len())
            {
                return None;
            }
            let (a, b) = *items.get(at)?;
            let (a, b) = item_expr(body, a, b);
            let inner = single_from(body, stop);
            return self.held(ctes, k, body, a, b, inner.as_ref(), depth + 1);
        }
        let entry = ScopeEntry {
            name: fname.clone(),
            alias: None,
            named: true,
        };
        let c = self.entry_table(&entry)?.column(
            Dialect::Tsql,
            &sqldiff_core::catalog::Ident::plain(col.clone()),
        )?;
        match Dt::of(&c.data_type.base, &c.data_type.args) {
            Dt::Bit => Some(Held::Bit),
            Dt::Int | Dt::BigInt | Dt::Fraction | Dt::Money => Some(Held::Number),
            _ => None,
        }
    }

    /// A value stored in a bit variable, parameter or result, as PostgreSQL
    /// text: TRUE / FALSE for a number, `(v <> 0)` for a numeric value.
    pub(super) fn bit_store_text(&mut self, ts: &[T]) -> String {
        let v = trimmed(ts);
        if v.is_empty() {
            return self.expr(v).trim().to_string();
        }
        let mut e = BitEdits::default();
        let (f, l) = match (sig(v).first(), sig(v).last()) {
            (Some(&f), Some(&l)) => (f, l),
            _ => return self.expr(v).trim().to_string(),
        };
        if self.store(v, f, l, &mut e) {
            self.bit_outer = Some(e);
        } else {
            self.unhandled(v);
        }
        self.expr(v).trim().to_string()
    }

    /// Bit uses with no exact rewrite seen since the last call.
    pub(super) fn take_bit_unhandled(&mut self) -> Vec<String> {
        std::mem::take(&mut self.bit_unhandled)
    }

    /// Runs `f` (a statement or a routine) and refuses it when a bit met a
    /// number in it with no exact rewrite. PostgreSQL rejects such code, and
    /// the engine can tell only when it knows the column types, which a check
    /// without catalogs does not; so the rules refuse it themselves.
    pub(super) fn guard_bits<X>(&mut self, f: impl FnOnce(&mut Self) -> R<X>) -> R<X> {
        let open = self.take_bit_unhandled();
        let r = f(self);
        let refusal = self.bit_refusal();
        self.bit_unhandled = open;
        match (r, refusal) {
            (Ok(_), Some(reason)) => no(reason),
            (r, _) => r,
        }
    }

    /// Whether the statements may involve a bit: a bit variable, a bit
    /// column of a table they name, or a rewrite the bit rules would make.
    /// The idiom rules know nothing of bits, so these are left to the token
    /// rules (and their refusals).
    pub(super) fn bits_in(&mut self, nodes: &[SyntaxNode]) -> bool {
        if !self.bits_possible() {
            return false;
        }
        nodes.iter().any(|n| {
            let ts = tokens(n);
            let bit_var = |t: &T| {
                t.k == VARIABLE
                    && self
                        .vars
                        .get(&t.s.trim_start_matches('@').to_ascii_lowercase())
                        .is_some_and(|v| v.dt == Dt::Bit)
            };
            // Db2 for i keeps SMALLINT, which takes the numbers as written.
            if !self.db2() && ts.iter().any(bit_var) {
                return true;
            }
            let cols = match self.db2() {
                true => Vec::new(),
                false => self.named_bit_cols(&ts),
            };
            if !self.db2()
                && ts.iter().any(|t| {
                    matches!(t.k, IDENT | QUOTED_IDENT)
                        && cols.contains(&tsql_unquote(&t.s).to_ascii_lowercase())
                })
            {
                return true;
            }
            let scope = std::mem::replace(&mut self.scope, scope_of(&ts));
            let open = self.take_bit_unhandled();
            let e = self.bit_edits(&ts);
            let hit = !(e.rep.is_empty()
                && e.pre.is_empty()
                && e.post.is_empty()
                && self.bit_unhandled.is_empty());
            self.bit_unhandled = open;
            self.scope = scope;
            hit
        })
    }

    /// Refuses a construct the engine does not check (an IF or WHILE
    /// condition, a CHECK constraint) when a bit meets a number in it.
    pub(super) fn bit_refusal(&mut self) -> Option<String> {
        let open = self.take_bit_unhandled();
        (!open.is_empty() && !self.db2()).then(|| {
            format!(
                "a bit meets a number in `{}`: BIT becomes boolean, and PostgreSQL has no comparison, arithmetic or assignment between boolean and the numbers. Rewrite it with TRUE / FALSE (or cast the boolean with ::integer)",
                open.join("`, `")
            )
        })
    }
}

impl Cx<'_> {
    /// PostgreSQL CAST / CONVERT, with what a bit needs: to bit, a number is
    /// TRUE when it is not 0 (only `integer::boolean` exists); from bit to a
    /// type other than bit and int, the 0 or 1 it stands for.
    pub(super) fn cast_pg(&mut self, x: &[T], ty: &[T]) -> Option<String> {
        self.in_cast = true;
        let t = self.ty(ty);
        self.in_cast = false;
        let (name, _, _) = t.ok()?;
        let base = trimmed(ty)
            .first()
            .map(|t| tsql_unquote(&t.s).to_ascii_uppercase())
            .unwrap_or_default();
        if base == "BIT" {
            if let Some(n) = int_literal(x) {
                return Some(if n == 0 { "FALSE" } else { "TRUE" }.into());
            }
            if self.is_numeric(x) {
                return Some(format!("({} <> 0)", self.expr(x).trim()));
            }
        } else if self.is_bit(x) && !matches!(base.as_str(), "INT" | "INTEGER") {
            return Some(format!("CAST({}::integer AS {name})", self.expr(x).trim()));
        }
        // To an integer, T-SQL drops a decimal's or float's fraction and
        // PostgreSQL rounds it (eval/casts); money rounds on both sides.
        let int = matches!(
            base.as_str(),
            "INT" | "INTEGER" | "SMALLINT" | "BIGINT" | "TINYINT"
        );
        let number = int
            || matches!(
                base.as_str(),
                "DECIMAL" | "NUMERIC" | "DEC" | "FLOAT" | "REAL"
            );
        match self.expr_dt(x) {
            Some(Dt::Fraction) if int => {
                return Some(format!("CAST(trunc({}) AS {name})", self.expr(x).trim()));
            }
            Some(Dt::Text) if number => self.note(Status::Assumptions, "ASM-CAST-TEXT-NUMBER", "Cast of a string to a number: T-SQL turns '' into 0 (and rejects '1e3' as a decimal), PostgreSQL rejects '' (and reads '1e3'). Equal if the value is never an empty string or written with an exponent."),
            None | Some(Dt::Other) if int => self.note(Status::Assumptions, "ASM-CAST-INT-FRACTION", "Cast to an integer of a value whose type is not known: T-SQL drops a fraction (1.5 becomes 1), PostgreSQL rounds it (2). Equal if the value has no fraction; otherwise write trunc(…)."),
            _ => {}
        }
        Some(format!("CAST({} AS {name})", self.expr(x).trim()))
    }

    /// The BIT columns of a catalog table, by its name as written.
    pub(super) fn table_bit_cols(&self, name: &[T]) -> Vec<String> {
        let parts: Vec<String> = name
            .iter()
            .filter(|t| matches!(t.k, IDENT | QUOTED_IDENT | VARIABLE))
            .map(|t| tsql_unquote(&t.s).to_ascii_lowercase())
            .collect();
        let entry = ScopeEntry {
            name: parts,
            alias: None,
            named: true,
        };
        self.entry_table(&entry)
            .map(|t| {
                t.columns
                    .iter()
                    .filter(|c| c.data_type.base == "BIT")
                    .map(|c| c.name.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the catalog gives a column of the named table a default (or
    /// makes it an identity or computed column).
    pub(super) fn table_col_has_default(&self, name: &[T], col: &str) -> bool {
        let parts: Vec<String> = name
            .iter()
            .filter(|t| matches!(t.k, IDENT | QUOTED_IDENT))
            .map(|t| tsql_unquote(&t.s).to_ascii_lowercase())
            .collect();
        let entry = ScopeEntry {
            name: parts,
            alias: None,
            named: true,
        };
        self.entry_table(&entry).is_some_and(|t| {
            t.columns
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(col) && (c.has_default || c.identity))
        })
    }

    /// Runs `f` with `cols` as the bit columns of the table being defined,
    /// and refuses what it wrote when a bit met a number with no exact
    /// rewrite (the engine does not compare DDL).
    pub(super) fn with_bit_cols(
        &mut self,
        cols: Vec<String>,
        f: impl FnOnce(&mut Self) -> R<String>,
    ) -> R<String> {
        let outer = std::mem::replace(&mut self.bit_cols, cols);
        let r = self.guard_bits(f);
        self.bit_cols = outer;
        r
    }
}

/// `DEFAULT n` on a boolean column: FALSE for 0, TRUE for any other number,
/// as T-SQL stores it in a bit.
pub(super) fn bool_default(rest: &str) -> String {
    let up = rest.to_ascii_uppercase();
    let mut out = String::new();
    let mut from = 0;
    while let Some(k) = up[from..].find("DEFAULT").map(|k| k + from) {
        let start = k + "DEFAULT".len();
        let tail = &rest[start..];
        let len = tail
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '(' | ')' | ' ' | '-' | '+')))
            .unwrap_or(tail.len());
        // Keep a closing parenthesis the value does not open.
        let mut span = &tail[..len];
        while span.matches(')').count() > span.matches('(').count() {
            span = &span[..span.rfind(')').unwrap()];
        }
        let digits: String = span
            .chars()
            .filter(|c| !matches!(c, '(' | ')' | ' '))
            .collect();
        out.push_str(&rest[from..start]);
        match digits.parse::<i128>() {
            Ok(n) => {
                out.push_str(if n == 0 { " FALSE" } else { " TRUE" });
                if span.ends_with(' ') {
                    out.push(' ');
                }
                from = start + span.len();
            }
            Err(_) => from = start,
        }
    }
    out.push_str(&rest[from..]);
    out
}

impl Cx<'_> {
    /// Db2 for i: BIT is SMALLINT, which compares with 0 and 1 as T-SQL
    /// does. What T-SQL converts on its own is written out: a string
    /// literal against a bit becomes its 0 or 1, and `~b` (NOT) `(1 - b)`,
    /// which is NOT for 0 and 1 (ASM-BIT-SMALLINT).
    fn bit_edits_db2(&mut self, ts: &[T], s: &[usize], e: &mut BitEdits) {
        for &i in s {
            let t = &ts[i];
            if t.k == OP && t.s == "~" {
                let Some(n) = next_sig(ts, i) else { continue };
                let Some(en) = operand_end(ts, n) else {
                    continue;
                };
                if self.is_bit(&ts[n..=en]) {
                    self.note(Status::Assumptions, "ASM-BIT-SMALLINT", BIT_SMALLINT);
                    e.rep.insert(i, "(1 - ".into());
                    e.post.insert(en, ")".into());
                    e.steps.push(("tsql.bit.not", i, en));
                }
                continue;
            }
            if !(t.k == OP && CMP.contains(&t.s.as_str())) {
                continue;
            }
            let (Some(lp), Some(rn)) = (prev_sig(ts, i), next_sig(ts, i)) else {
                continue;
            };
            let (Some(ls), Some(re)) = (operand_start(ts, lp), operand_end(ts, rn)) else {
                continue;
            };
            let (l, r) = (self.is_bit(&ts[ls..=lp]), self.is_bit(&ts[rn..=re]));
            if l == r {
                continue;
            }
            if t.s == "=" && l && self.assigns(ts, ls) {
                self.store(ts, rn, re, e);
                continue;
            }
            let (os, oe) = if l { (rn, re) } else { (ls, lp) };
            if let Some(b) = bit_string(&ts[os..=oe]) {
                e.rep.insert(os, b.into());
                e.steps.push(("tsql.bit.compare", os, oe));
            }
        }
    }
}
