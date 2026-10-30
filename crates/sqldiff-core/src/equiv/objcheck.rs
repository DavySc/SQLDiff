//! Existence checks on the system catalogs: "does table / procedure
//! dbo.X exist?", the guard deployment scripts and Ola Hallengren's
//! procedures put around CREATE TABLE and before calling a procedure.
//!
//! Only these shapes are read, each as one `EXISTS (…)` subquery:
//!
//! - T-SQL, by object id: `SELECT * FROM sys.objects [o] WHERE
//!   [o.]object_id = OBJECT_ID(N'[dbo].[X]') AND [o.]type IN (N'U')`;
//! - T-SQL, by names: `SELECT * FROM sys.objects o INNER JOIN sys.schemas s
//!   ON o.schema_id = s.schema_id WHERE o.type = 'U' AND s.name = 'dbo' AND
//!   o.name = 'X'` (conditions in any order);
//! - PostgreSQL, the form the converter writes for them ([`postgres_text`]).
//!
//! The kinds are a user table (`type` U; PostgreSQL relkind r or p, an
//! ordinary or partitioned table) and a procedure (`type` P, or P and PC;
//! PostgreSQL prokind p). The name is a plain ASCII identifier in dbo, which
//! the converter writes unqualified, so it is found in PostgreSQL's default
//! schema (`current_schema()`). Both sides compare the name without regard
//! to letter case: SQL Server through the database's case-insensitive
//! collation (checked on SQL Server 2022, SQL_Latin1_General_CP1_CI_AS:
//! `objects.name = 'COMMANDLOG'` finds dbo.CommandLog), PostgreSQL through
//! `lower(relname)`. Anything else is not read, and the subquery is lowered
//! as the query it is.
//!
//! What the two checks look at differs: SQL Server's catalog against
//! PostgreSQL's. They agree when PostgreSQL holds the converted counterpart
//! of exactly the objects SQL Server holds, which is
//! [`ASSUMPTION`]; every unit with such a check carries it.

use sqldiff_syntax::Dialect;

use super::ast::{Expr, From, Select};

/// What an existence check looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Table,
    Procedure,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Table => "table",
            Kind::Procedure => "procedure",
        }
    }
}

/// An existence check: the kind and the lower-cased name of an object in
/// dbo (T-SQL) or the default schema (PostgreSQL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub kind: Kind,
    pub name: String,
}

pub const ASSUMPTION: (&str, &str) = (
    "ASM-CATALOG-EXISTS",
    "A check whether a table or procedure exists sees the same objects on both sides: a SQL Server user table (sys.objects type U) or T-SQL procedure (type P) in dbo exists exactly when its converted table (pg_class relkind r or p) or procedure (pg_proc prokind p) exists in PostgreSQL's default schema when the check runs (current_schema()), under a name equal up to letter case. This needs: the session running the check can see the object in SQL Server's catalog (holds a permission on it; PostgreSQL's catalogs show every object to everyone), the SQL Server database collation is case-insensitive (the T-SQL name test follows it, lower() does not), and no CLR procedure (type PC) of that name exists in dbo (it has no PostgreSQL counterpart)",
);

/// The check a T-SQL `EXISTS` subquery makes, if it is one of the shapes
/// above.
pub fn tsql(q: &Select) -> Option<Check> {
    if !plain(q) {
        return None;
    }
    // The qualifiers that name sys.objects and sys.schemas.
    let (objs, schemas, on) = match q.from.as_slice() {
        [t @ From::Table { .. }] => (table(t, &["sys", "objects"])?, None, None),
        [From::Join {
            kind,
            left,
            right,
            on: Some(on),
        }] if kind == "INNER" => (
            table(left, &["sys", "objects"])?,
            Some(table(right, &["sys", "schemas"])?),
            Some(on),
        ),
        _ => return None,
    };
    let is_obj = |q: &Option<String>| match q {
        Some(q) => *q == objs,
        // `type` and `object_id` are columns of sys.objects only; `name`
        // is in both views, so it is read only when qualified.
        None => true,
    };
    let is_schema = |q: &Option<String>| match (q, &schemas) {
        (Some(q), Some(s)) => q == s,
        _ => false,
    };
    if let Some(on) = on {
        let ok = match on {
            Expr::Bin(op, l, r) if op == "=" => match (col(l), col(r)) {
                (Some((lq, lc)), Some((rq, rc))) => {
                    lc == "schema_id"
                        && rc == "schema_id"
                        && lq.is_some()
                        && rq.is_some()
                        && (is_obj(&lq) && is_schema(&rq) || is_schema(&lq) && is_obj(&rq))
                }
                _ => false,
            },
            _ => false,
        };
        if !ok {
            return None;
        }
    }
    let mut by_id: Option<String> = None;
    let mut types: Option<Vec<String>> = None;
    let mut schema: Option<String> = None;
    let mut name: Option<String> = None;
    for c in conjuncts(q.where_.as_ref()?) {
        let slot = match c {
            Expr::Bin(op, l, r) if op == "=" => {
                let (column, value) = match (col(l), col(r)) {
                    (Some(c), None) => (c, &**r),
                    (None, Some(c)) => (c, &**l),
                    _ => return None,
                };
                match (column.1.as_str(), value) {
                    ("object_id", Expr::Call(f)) if is_obj(&column.0) => {
                        let [Expr::Str(lit)] = f.args.as_slice() else {
                            return None;
                        };
                        if !(f.name.len() == 1
                            && f.name[0].eq_ignore_ascii_case("OBJECT_ID")
                            && !f.distinct
                            && !f.over
                            && !f.star)
                        {
                            return None;
                        }
                        set(&mut by_id, dbo_name(lit)?)
                    }
                    ("type", Expr::Str(t)) if is_obj(&column.0) => {
                        set(&mut types, vec![t.to_ascii_uppercase()])
                    }
                    ("name", Expr::Str(s)) if is_schema(&column.0) => set(&mut schema, s.clone()),
                    ("name", Expr::Str(s)) if column.0.is_some() && is_obj(&column.0) => {
                        set(&mut name, s.clone())
                    }
                    _ => return None,
                }
            }
            Expr::InList(l, list, false) => match col(l) {
                Some((q, c)) if c == "type" && is_obj(&q) => {
                    let mut v = Vec::new();
                    for x in list {
                        let Expr::Str(t) = x else { return None };
                        v.push(t.to_ascii_uppercase());
                    }
                    set(&mut types, v)
                }
                _ => return None,
            },
            _ => return None,
        };
        slot?;
    }
    let mut types = types?;
    types.sort();
    types.dedup();
    let kind = match types
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["U"] => Kind::Table,
        ["P"] | ["P", "PC"] => Kind::Procedure,
        _ => return None,
    };
    let name = match (schemas, by_id, schema, name) {
        (None, Some(n), None, None) => n,
        (Some(_), None, Some(s), Some(n)) if s.eq_ignore_ascii_case("dbo") && ident(&n) => {
            n.to_ascii_lowercase()
        }
        _ => return None,
    };
    Some(Check { kind, name })
}

/// The check a PostgreSQL `EXISTS` subquery makes, if it is the form
/// [`postgres_text`] writes.
pub fn postgres(q: &Select) -> Option<Check> {
    if !plain(q) {
        return None;
    }
    let [From::Join {
        kind,
        left,
        right,
        on: Some(on),
    }] = q.from.as_slice()
    else {
        return None;
    };
    if kind != "INNER" {
        return None;
    }
    let ns = table(right, &["pg_catalog", "pg_namespace"])?;
    let (obj, kind, ns_col, name_col) = if let Some(c) = table(left, &["pg_catalog", "pg_class"]) {
        (c, Kind::Table, "relnamespace", "relname")
    } else {
        let c = table(left, &["pg_catalog", "pg_proc"])?;
        (c, Kind::Procedure, "pronamespace", "proname")
    };
    let is = |e: &Expr, q: &str, c: &str| matches!(col(e), Some((Some(x), y)) if x == q && y == c);
    match on {
        Expr::Bin(op, l, r)
            if op == "="
                && (is(l, &ns, "oid") && is(r, &obj, ns_col)
                    || is(l, &obj, ns_col) && is(r, &ns, "oid")) => {}
        _ => return None,
    }
    let (mut schema, mut name, mut what) = (None, None, None);
    for c in conjuncts(q.where_.as_ref()?) {
        let slot = match c {
            Expr::Bin(op, l, r) if op == "=" => {
                let (l, r) = (&**l, &**r);
                let (l, r) = if matches!(l, Expr::Call(_)) && !matches!(r, Expr::Call(_)) {
                    (r, l)
                } else {
                    (l, r)
                };
                match (l, r) {
                    (l, Expr::Call(f)) if is(l, &ns, "nspname") => {
                        if !(call(f, "current_schema") && f.args.is_empty()) {
                            return None;
                        }
                        set(&mut schema, ())
                    }
                    (Expr::Str(s), Expr::Call(f)) | (Expr::Call(f), Expr::Str(s)) => {
                        let [arg] = f.args.as_slice() else {
                            return None;
                        };
                        if !(call(f, "lower") && is(arg, &obj, name_col) && ident(s))
                            || *s != s.to_lowercase()
                        {
                            return None;
                        }
                        set(&mut name, s.clone())
                    }
                    (l, Expr::Str(k)) if kind == Kind::Procedure && is(l, &obj, "prokind") => {
                        if k != "p" {
                            return None;
                        }
                        set(&mut what, ())
                    }
                    _ => return None,
                }
            }
            Expr::InList(l, list, false) if kind == Kind::Table && is(l, &obj, "relkind") => {
                let mut v: Vec<&str> = list
                    .iter()
                    .map(|x| match x {
                        Expr::Str(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect::<Option<_>>()?;
                v.sort_unstable();
                if v != ["p", "r"] {
                    return None;
                }
                set(&mut what, ())
            }
            _ => return None,
        };
        slot?;
    }
    schema?;
    what?;
    Some(Check { kind, name: name? })
}

/// The PostgreSQL text of a check (the whole `EXISTS (…)`).
pub fn postgres_text(c: &Check) -> String {
    match c.kind {
        Kind::Table => format!(
            "EXISTS (SELECT 1 FROM pg_catalog.pg_class pc INNER JOIN pg_catalog.pg_namespace pn ON pn.oid = pc.relnamespace WHERE pn.nspname = current_schema() AND lower(pc.relname) = '{}' AND pc.relkind IN ('r', 'p'))",
            c.name
        ),
        Kind::Procedure => format!(
            "EXISTS (SELECT 1 FROM pg_catalog.pg_proc pp INNER JOIN pg_catalog.pg_namespace pn ON pn.oid = pp.pronamespace WHERE pn.nspname = current_schema() AND lower(pp.proname) = '{}' AND pp.prokind = 'p')",
            c.name
        ),
    }
}

/// The check of an `EXISTS` subquery in dialect `d`.
pub fn of(q: &Select, d: Dialect) -> Option<Check> {
    match d {
        Dialect::Tsql => tsql(q),
        Dialect::Postgres => postgres(q),
        Dialect::Db2i => None,
    }
}

/// A query with nothing but one `*` or `1` item, FROM and WHERE.
fn plain(q: &Select) -> bool {
    let item = match q.items.as_slice() {
        [it] if it.alias.is_none() && it.assign.is_none() => &it.expr,
        _ => return false,
    };
    (matches!(item, Expr::Star(None)) || matches!(item, Expr::Num(n) if n == "1"))
        && q.ctes.is_empty()
        && q.set_ops.is_empty()
        && !q.for_json
        && !q.distinct
        && q.top.is_none()
        && q.into.is_empty()
        && q.group_by.is_empty()
        && q.having.is_none()
        && q.order_by.is_empty()
        && q.offset.is_none()
        && q.limit.is_none()
        && q.locking.is_empty()
        && q.values.is_none()
}

/// The lower-cased qualifier a FROM table `name` is referred to by (its
/// alias, else its last name part).
fn table(f: &From, name: &[&str]) -> Option<String> {
    let From::Table {
        name: n,
        alias,
        hints,
    } = f
    else {
        return None;
    };
    let same = n.len() == name.len()
        && n.iter()
            .zip(name)
            .all(|(x, y)| !x.starts_with('"') && x.eq_ignore_ascii_case(y));
    if !same || !hints.is_empty() {
        return None;
    }
    let q = alias
        .clone()
        .unwrap_or_else(|| name[name.len() - 1].to_string());
    (!q.starts_with('"')).then(|| q.to_ascii_lowercase())
}

/// A column reference: its lower-cased qualifier, if written, and name.
fn col(e: &Expr) -> Option<(Option<String>, String)> {
    let Expr::Name(p) = e else { return None };
    if p.iter().any(|x| x.starts_with('"')) {
        return None;
    }
    match p.as_slice() {
        [c] => Some((None, c.to_ascii_lowercase())),
        [q, c] => Some((Some(q.to_ascii_lowercase()), c.to_ascii_lowercase())),
        _ => None,
    }
}

fn call(f: &super::ast::Call, name: &str) -> bool {
    f.name.len() == 1
        && f.name[0].eq_ignore_ascii_case(name)
        && !f.distinct
        && !f.over
        && !f.star
        && f.order.is_empty()
}

/// The operands of a chain of ANDs.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Bin(op, l, r) if op.eq_ignore_ascii_case("AND") => {
            let mut v = conjuncts(l);
            v.extend(conjuncts(r));
            v
        }
        e => vec![e],
    }
}

/// Fills a slot once: a second condition for the same slot is not read.
fn set<T>(slot: &mut Option<T>, v: T) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(v);
    Some(())
}

/// A plain ASCII identifier PostgreSQL keeps as written when folded: no
/// quotes, spaces or characters whose case folding could differ between
/// the two engines, and within PostgreSQL's 63-byte names.
fn ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 63
}

/// `[dbo].[X]` / `dbo.X`: the lower-cased name of an object in dbo.
fn dbo_name(lit: &str) -> Option<String> {
    let part = |p: &str| -> Option<String> {
        let p = match p.strip_prefix('[') {
            Some(r) => r.strip_suffix(']')?,
            None => p,
        };
        ident(p).then(|| p.to_string())
    };
    let (s, n) = lit.split_once('.')?;
    let (s, n) = (part(s)?, part(n)?);
    s.eq_ignore_ascii_case("dbo")
        .then(|| n.to_ascii_lowercase())
}
