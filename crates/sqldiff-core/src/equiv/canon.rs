//! Lowering of parsed statements to canonical terms.
//!
//! Dialect spellings become one form (`ISNULL`/`COALESCE`, `+`/`||`,
//! `DATEDIFF(day)`/date subtraction/`DAYS()`, `STRING_AGG`/`LISTAGG`, paging,
//! upserts, generated-key returns, anti-join deletes), names are resolved
//! (parameters by position, locals by declaration order, aliases to tables),
//! and every dialect difference in behaviour stays an explicit child. Facts
//! that make a rewrite sound are either discharged from the catalog or
//! recorded as assumptions.

use std::collections::HashMap;

use sqldiff_syntax::Dialect;

use super::ast::{
    Call, ConflictAction, Expr, From, InsertSource, MergeAction, OrderItem, SelItem, Select, Stmt,
};
use super::lower::{Unit, UnitKind};
use super::schema::{Fam, Schema, Ty};
use super::term::{a, n, Term};
use crate::model::Assumption;

pub type R<T> = Result<T, String>;

/// Lowered query parts without the select list, and the select items with their types.
type Parts = (Vec<Term>, Vec<(Term, Option<Ty>)>);

/// Assumption identifiers and texts used by the equivalence rules.
pub mod asm {
    pub const TSQL_CI: (&str, &str) = (
        "ASM-TSQL-CI-COLLATION",
        "T-SQL string comparisons use a case-insensitive collation",
    );
    pub const CASE_FOLD: (&str, &str) = (
        "ASM-CASE-FOLD",
        "UPPER/LOWER fold the same characters as the case-insensitive comparison on the other side",
    );
    pub const DB2_SRTSEQ: (&str, &str) = (
        "ASM-DB2-SRTSEQ-HEX",
        "Db2 for i compares strings with the *HEX sort sequence (case-sensitive)",
    );
    pub const UNTYPED: (&str, &str) = (
        "ASM-UNTYPED-NONCHAR",
        "Values whose type is unknown (no catalog) are not character strings, so comparison and `+` mean the same on both sides",
    );
    pub const TSQL_PLUS_STRING: (&str, &str) = (
        "ASM-TSQL-PLUS-IS-CONCAT",
        "Operands of T-SQL `+` next to a string literal are character strings, so `+` concatenates",
    );
    pub const SYSDATETIME: (&str, &str) = (
        "ASM-TSQL-SYSDATETIME-PER-STATEMENT",
        "SYSDATETIME() is evaluated once per statement (observed SQL Server behaviour, not documented; fact V20)",
    );
    pub const STRING_AGG_8000: (&str, &str) = (
        "ASM-TSQL-STRING-AGG-8000",
        "The aggregated list stays under 8000 bytes (T-SQL STRING_AGG of a non-MAX input errors past that)",
    );
    pub const LISTAGG_LEN: (&str, &str) = (
        "ASM-DB2-LISTAGG-LENGTH",
        "The aggregated list fits the LISTAGG result length",
    );
    pub const DB2_MERGE_CONCURRENT: (&str, &str) = (
        "ASM-DB2-MERGE-CONCURRENT",
        "Db2 for i MERGE does not raise a duplicate key when two sessions insert the same key concurrently (fact V3)",
    );
    pub const SINGLE_ROW: (&str, &str) = (
        "ASM-SINGLE-ROW",
        "The query returns at most one row (otherwise each side keeps a different arbitrary row)",
    );
    pub const ORDER_TOTAL: (&str, &str) = (
        "ASM-ORDER-TOTAL",
        "The ORDER BY keys are unique, so both sides return the same rows in the same order",
    );
    pub const RESULT_TYPES: (&str, &str) = (
        "ASM-RESULT-TYPES",
        "Declared result column types hold the selected values without conversion",
    );
    pub const NO_CALLER_TXN: (&str, &str) = (
        "ASM-NO-CALLER-TXN",
        "The caller does not hold an open transaction",
    );

    pub const OWNER_PRIVILEGES: (&str, &str) = (
        "ASM-OWNER-PRIVILEGES",
        "The routine owner holds every privilege its statements need on both sides (Db2 for i USRPRF *OWNER adds the owner's authority to the caller's; T-SQL EXECUTE AS OWNER and PostgreSQL SECURITY DEFINER use the owner's alone)",
    );
    pub const OWNERSHIP_CHAIN: (&str, &str) = (
        "ASM-TSQL-OWNERSHIP-CHAIN",
        "Callers hold the privileges the statements need themselves: T-SQL skips permission checks along an ownership chain (routine and objects with one owner), the other side does not",
    );
    pub const DB2_USRPRF_USER: (&str, &str) = (
        "ASM-DB2-USRPRF-USER",
        "The Db2 for i routine has no SET OPTION USRPRF, so its authority comes from USRPRF *NAMING: assumed to be the caller's (system naming at creation; under SQL naming it runs with the owner's authority)",
    );
    pub const DEFAULT_SCHEMA: (&str, &str) = (
        "ASM-DEFAULT-SCHEMA",
        "Unqualified names, T-SQL dbo and PostgreSQL public resolve to the same schema on both sides (the default schema, search_path or Db2 for i current schema in effect)",
    );
    pub const IDENT_FOLD: (&str, &str) = (
        "ASM-IDENT-FOLD",
        "Unquoted names on both sides name the same objects and columns after case folding: the PostgreSQL (lower case) and Db2 for i (upper case) objects were created from unquoted names, and T-SQL resolves names case-insensitively (case-insensitive catalog collation)",
    );
    pub const ROUTINE_LABEL: (&str, &str) = (
        "ASM-ROUTINE-LABEL",
        "The routine's STABLE, IMMUTABLE or DETERMINISTIC label is truthful: its result depends only on its arguments and data that does not change while the calling statement runs, so evaluating it fewer times gives the same results",
    );

    pub fn make(x: (&str, &str)) -> crate::model::Assumption {
        crate::model::Assumption::default(x.0, x.1)
    }
}

#[derive(Debug, Clone)]
struct Binding {
    /// Alias or table name, lower case.
    key: String,
    /// Canonical table name.
    table: String,
    /// Derived single-row sources: column → value.
    derived: Option<Vec<(String, Term, Option<Ty>)>>,
}

pub struct Canon<'a> {
    pub d: Dialect,
    pub schema: &'a Schema,
    pub unit: &'a Unit,
    scopes: Vec<Vec<Binding>>,
    pub asm: Vec<Assumption>,
    pub discharged: Vec<String>,
    /// The statement being lowered evaluates its expressions for one row.
    single_row: bool,
    seen_tables: HashMap<String, usize>,
    /// PostgreSQL `EXCLUDED.c` while lowering ON CONFLICT DO UPDATE.
    excluded: Option<Vec<(String, Term, Option<Ty>)>>,
    /// Identify parameters and locals by name instead of position. Used when
    /// only some statements of two versions are compared, so a renaming is
    /// not checked across the whole routine.
    pub by_name: bool,
    /// Table names of the statement being lowered (see `Item::names`).
    pub names: Vec<(String, Option<String>)>,
}

/// Folded name; names whose quoting is significant (leading `"`) keep their case.
fn lc(s: &str) -> String {
    if s.starts_with('"') {
        s.to_string()
    } else {
        s.to_lowercase()
    }
}

impl<'a> Canon<'a> {
    pub fn new(unit: &'a Unit, schema: &'a Schema) -> Self {
        Canon {
            d: unit.dialect,
            schema,
            unit,
            scopes: Vec::new(),
            asm: Vec::new(),
            discharged: Vec::new(),
            single_row: false,
            seen_tables: HashMap::new(),
            excluded: None,
            by_name: false,
            names: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.scopes.clear();
        self.asm.clear();
        self.discharged.clear();
        self.single_row = false;
        self.seen_tables.clear();
        self.excluded = None;
        self.names.clear();
    }

    fn assume(&mut self, x: (&str, &str)) {
        if !self.asm.iter().any(|y| y.id == x.0) {
            self.asm.push(asm::make(x));
        }
    }

    fn discharge(&mut self, s: String) {
        if !self.discharged.contains(&s) {
            self.discharged.push(s);
        }
    }

    fn dtag(&self) -> Term {
        a(self.d.id())
    }

    // ---- names ----------------------------------------------------------

    /// Key of a table name: the folded name, prefixed with its folded schema
    /// (and database) unless the schema is the default one (none written,
    /// T-SQL `dbo`, PostgreSQL `public`). Temporary tables get `temp:`.
    /// Returns the key and, for a default schema, how it was written.
    pub fn table_name(parts: &[String]) -> (String, Option<String>) {
        let last = parts.last().map(|s| lc(s)).unwrap_or_default();
        let temp = last.starts_with('#')
            || last.starts_with('@')
            || parts
                .first()
                .is_some_and(|p| p.eq_ignore_ascii_case("SESSION") && parts.len() > 1);
        if temp {
            return (
                format!("temp:{}", last.trim_start_matches(['#', '@'])),
                None,
            );
        }
        let quals: Vec<String> = parts[..parts.len().saturating_sub(1)]
            .iter()
            .map(|s| lc(s))
            .collect();
        match quals.as_slice() {
            [] => (last, Some(String::new())),
            [q] if q == "dbo" || q == "public" => (last, Some(q.clone())),
            _ => (format!("{}.{last}", quals.join(".")), None),
        }
    }

    /// `table_name`, remembering the name for the statement's item.
    fn table_key(&mut self, parts: &[String]) -> String {
        let (key, default) = Self::table_name(parts);
        let x = (key.clone(), default);
        if !self.names.contains(&x) {
            self.names.push(x);
        }
        key
    }

    fn bind_table(&mut self, parts: &[String], alias: &Option<String>) -> Binding {
        let table = self.table_key(parts);
        let k = self.seen_tables.entry(table.clone()).or_insert(0);
        *k += 1;
        let canon = if *k > 1 {
            format!("{table}#{k}")
        } else {
            table.clone()
        };
        Binding {
            key: alias
                .as_ref()
                .map(|x| lc(x))
                .unwrap_or_else(|| lc(parts.last().map(String::as_str).unwrap_or(""))),
            table: canon,
            derived: None,
        }
    }

    fn all_bindings(&self) -> usize {
        self.scopes.iter().map(Vec::len).sum()
    }

    /// Catalog name of a table key: without its occurrence number and schema
    /// (the catalog is keyed by table name).
    fn base_table(t: &str) -> &str {
        let t = t.split('#').next().unwrap_or(t);
        if t.starts_with('"') {
            return t;
        }
        t.rsplit_once('.').map_or(t, |(_, name)| name)
    }

    fn col_ty(&self, table: &str, col: &str) -> (Option<Ty>, Option<bool>) {
        match self
            .schema
            .table(Self::base_table(table))
            .and_then(|t| t.column(col))
        {
            Some(c) => (Some(c.ty), Some(c.not_null)),
            None => (None, None),
        }
    }

    fn sym(&self, name: &str) -> Option<(Term, Option<Ty>)> {
        let n0 = lc(name.trim_start_matches('@'));
        if let Some(i) = self.unit.params.iter().position(|p| p.name == n0) {
            let id = if self.by_name {
                format!("p:{n0}")
            } else {
                format!("p{}", i + 1)
            };
            return Some((n("sym", vec![a(id)]), self.unit.params[i].ty));
        }
        if let Some(i) = self.unit.locals.iter().position(|l| l.name == n0) {
            let id = if self.by_name {
                format!("l:{n0}")
            } else {
                format!("l{}", i + 1)
            };
            return Some((n("sym", vec![a(id)]), self.unit.locals[i].ty));
        }
        None
    }

    fn column_in_scope(&self, col: &str) -> bool {
        self.scopes.iter().flatten().any(|b| {
            b.derived
                .as_ref()
                .map(|d| d.iter().any(|(c, _, _)| c == col))
                .unwrap_or_else(|| self.col_ty(&b.table, col).0.is_some())
        })
    }

    fn resolve(&mut self, parts: &[String]) -> R<(Term, Option<Ty>)> {
        let parts: Vec<String> = parts.iter().map(|p| lc(p)).collect();
        if parts.len() == 1 {
            let c = &parts[0];
            if self.d != Dialect::Tsql {
                if let Some(s) = self.sym(c) {
                    if self.column_in_scope(c) {
                        return Err(format!("`{c}` names both a column and a variable"));
                    }
                    return Ok(s);
                }
            }
            // Innermost scope that is known to have the column.
            for scope in self.scopes.iter().rev() {
                for b in scope {
                    if let Some(d) = &b.derived {
                        if let Some((_, t, ty)) = d.iter().find(|(x, _, _)| x == c) {
                            return Ok((t.clone(), *ty));
                        }
                    } else if self.col_ty(&b.table, c).0.is_some() {
                        let (ty, _) = self.col_ty(&b.table, c);
                        return Ok((n("col", vec![a(b.table.clone()), a(c.clone())]), ty));
                    }
                }
            }
            if self.all_bindings() == 1 {
                let b = self.scopes.iter().flatten().next().unwrap().clone();
                if b.derived.is_none() {
                    return Ok((n("col", vec![a(b.table), a(c.clone())]), None));
                }
            }
            return Ok((n("col", vec![a("?"), a(c.clone())]), None));
        }
        let (qual, col) = (&parts[parts.len() - 2], &parts[parts.len() - 1]);
        if parts.len() == 2 && qual == "excluded" && self.d == Dialect::Postgres {
            if let Some(ex) = &self.excluded {
                if let Some((_, t, ty)) = ex.iter().find(|(x, _, _)| x == col) {
                    return Ok((t.clone(), *ty));
                }
                return Err(format!("EXCLUDED.{col} is not an inserted column"));
            }
        }
        for scope in self.scopes.iter().rev() {
            for b in scope {
                let matches = b.key == *qual || Self::base_table(&b.table) == qual.as_str();
                if !matches {
                    continue;
                }
                if let Some(d) = &b.derived {
                    return match d.iter().find(|(x, _, _)| x == col) {
                        Some((_, t, ty)) => Ok((t.clone(), *ty)),
                        None => Err(format!("{qual}.{col} is not a column of the source")),
                    };
                }
                let (ty, _) = self.col_ty(&b.table, col);
                return Ok((n("col", vec![a(b.table.clone()), a(col.clone())]), ty));
            }
        }
        // Function-qualified parameter (PL/pgSQL) or unknown qualifier.
        Ok((n("col", vec![a(format!("?{qual}")), a(col.clone())]), None))
    }

    fn not_null(&self, t: &Term) -> Option<bool> {
        if t.is("col") {
            let tab = t.kid(0)?.atom()?;
            let col = t.kid(1)?.atom()?;
            return self.col_ty(tab, col).1;
        }
        match t.head() {
            "num" | "str" | "time" => Some(true),
            _ => None,
        }
    }

    // ---- types ----------------------------------------------------------

    pub fn ty_term(t: &Option<Ty>) -> Term {
        match t {
            None => a("?"),
            Some(t) => n(
                "ty",
                vec![
                    a(format!("{:?}", t.fam)),
                    a(t.len.map(|x| x.to_string()).unwrap_or_else(|| "-".into())),
                    a(t.scale.map(|x| x.to_string()).unwrap_or_else(|| "-".into())),
                ],
            ),
        }
    }

    /// Whether a value of type `src` is stored in `dst` unchanged.
    fn fits(dst: &Ty, src: &Ty) -> bool {
        if dst.fam != src.fam {
            return dst.is_string()
                && src.is_string()
                && dst.fam == Fam::VarChar
                && dst.len.is_none();
        }
        match dst.fam {
            Fam::Int | Fam::VarChar => match (dst.len, src.len) {
                (None, _) => true,
                (Some(d), Some(s)) => d >= s,
                (Some(_), None) => false,
            },
            Fam::Char => dst.len == src.len,
            Fam::Exact => {
                dst.len.is_none()
                    || (src.len.is_some() && dst.len >= src.len && dst.scale == src.scale)
            }
            _ => true,
        }
    }

    /// Implicit conversion of a value to a declared type: dropped when it is
    /// provably a no-op.
    fn conv(&mut self, dst: &Option<Ty>, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        let Some(d) = dst else { return v };
        match &v.1 {
            Some(s) if Self::fits(d, s) => v,
            None if matches!(v.0.head(), "null") => v,
            _ => (n("conv", vec![Self::ty_term(dst), v.0]), Some(*d)),
        }
    }

    // ---- expressions ----------------------------------------------------

    pub fn expr(&mut self, e: &Expr) -> R<(Term, Option<Ty>)> {
        Ok(match e {
            Expr::Name(parts) => self.resolve(parts)?,
            Expr::Var(v) => {
                if v.starts_with("@@") {
                    (n("dfn", vec![a("tsql"), a(lc(v))]), None)
                } else {
                    self.sym(v)
                        .unwrap_or_else(|| (n("sym", vec![a(lc(v))]), None))
                }
            }
            Expr::Num(s) => {
                let ty = if s.contains(['.', 'e']) {
                    Ty::of(Fam::Exact)
                } else {
                    Ty {
                        fam: Fam::Int,
                        len: Some(32),
                        scale: None,
                    }
                };
                (n("num", vec![a(s.clone())]), Some(ty))
            }
            Expr::Str(s) => (
                n("str", vec![a(s.clone())]),
                Some(Ty {
                    fam: Fam::VarChar,
                    len: Some(s.chars().count().max(1) as u32),
                    scale: None,
                }),
            ),
            Expr::Null => (a("null"), None),
            Expr::Star(q) => (
                n(
                    "star",
                    vec![a(q.clone().map(|x| lc(&x)).unwrap_or_default())],
                ),
                None,
            ),
            Expr::Call(c) => self.call(c)?,
            Expr::Bin(op, l, r) => self.binary(op, l, r)?,
            Expr::Unary(op, x) => {
                let (t, ty) = self.expr(x)?;
                if op == "NOT" {
                    (n("not", vec![t]), None)
                } else {
                    (n("neg", vec![t]), ty)
                }
            }
            Expr::IsNull(x, neg) => {
                let (t, _) = self.expr(x)?;
                (n("isnull", vec![t, a(neg.to_string())]), None)
            }
            Expr::InList(x, list, neg) => {
                let (t, ty) = self.expr(x)?;
                let mut items = Vec::new();
                let (mut string, mut known) = (ty.is_some_and(|t| t.is_string()), ty.is_some());
                for l in list {
                    let (it, ity) = self.expr(l)?;
                    string |= ity.is_some_and(|t| t.is_string());
                    known |= ity.is_some();
                    items.push(it);
                }
                items.sort();
                let mut kids = vec![t, n("list", items), a(neg.to_string())];
                // Like `=`: string membership depends on the collation.
                if string {
                    kids.push(self.dtag());
                } else if !known {
                    self.assume(asm::UNTYPED);
                }
                (n("in", kids), None)
            }
            Expr::InQuery(x, q, neg) => {
                let (t, ty) = self.expr(x)?;
                let q = self.query(q, QCtx::Scalar)?;
                let mut kids = vec![t, q.0, a(neg.to_string())];
                // The subquery's column type is not tracked, so only a known
                // non-string operand avoids the collation.
                if ty.map_or(true, |t| t.is_string()) {
                    kids.push(self.dtag());
                }
                (n("inq", kids), None)
            }
            Expr::Between(x, lo, hi, neg) => {
                let (t, ty) = self.expr(x)?;
                let lo = self.expr(lo)?.0;
                let hi = self.expr(hi)?.0;
                let mut kids = vec![t, lo, hi, a(neg.to_string())];
                if ty.map_or(true, |t| t.is_string()) {
                    kids.push(self.dtag());
                }
                (n("between", kids), None)
            }
            Expr::Like(x, p, neg, ci) => {
                let x = self.expr(x)?.0;
                let p = self.expr(p)?.0;
                // Case sensitivity and trailing blanks of LIKE differ per dialect.
                (
                    n(
                        "like",
                        vec![self.dtag(), x, p, a(neg.to_string()), a(ci.to_string())],
                    ),
                    None,
                )
            }
            Expr::Cast(x, ty) => {
                let dst = Ty::from_type(ty, self.d, true);
                let v = self.expr(x)?;
                self.cast(dst, v)
            }
            Expr::Case(op, whens, els) => {
                let mut kids = Vec::new();
                match op {
                    Some(o) => {
                        let (t, ty) = self.expr(o)?;
                        if ty.map_or(true, |t| t.is_string()) {
                            kids.push(self.dtag());
                        }
                        kids.push(t);
                    }
                    None => kids.push(a("-")),
                }
                let mut ty = None;
                for (w, t) in whens {
                    let w = self.expr(w)?.0;
                    let (t, tty) = self.expr(t)?;
                    ty = ty.or(tty);
                    kids.push(n("when", vec![w, t]));
                }
                kids.push(match els {
                    Some(e) => self.expr(e)?.0,
                    None => a("null"),
                });
                (n("case", kids), ty)
            }
            Expr::Exists(q) => (n("exists", vec![self.query(q, QCtx::Exists)?.0]), None),
            Expr::Subquery(q) => {
                let (t, ty) = self.query(q, QCtx::Scalar)?;
                (n("subq", vec![t]), ty)
            }
            Expr::Tuple(list) => {
                let mut v = Vec::new();
                for x in list {
                    v.push(self.expr(x)?.0);
                }
                (n("tuple", v), None)
            }
            Expr::Special(s) => self.special(s)?,
            Expr::Extract(f, x) => {
                let x = self.expr(x)?.0;
                (
                    n("extract", vec![a(f.clone()), x]),
                    Some(Ty::of(Fam::Exact)),
                )
            }
        })
    }

    fn time(&mut self, kind: &str) -> (Term, Option<Ty>) {
        if kind == "stmt" && self.d == Dialect::Tsql && !self.single_row {
            self.assume(asm::SYSDATETIME);
        }
        (n("time", vec![a(kind)]), Some(Ty::of(Fam::Timestamp)))
    }

    fn special(&mut self, s: &str) -> R<(Term, Option<Ty>)> {
        Ok(match (self.d, s) {
            (Dialect::Db2i, "CURRENT_TIMESTAMP") => self.time("stmt"),
            (Dialect::Postgres, "CURRENT_TIMESTAMP" | "LOCALTIMESTAMP") => self.time("txn"),
            (Dialect::Tsql, "CURRENT_TIMESTAMP") => self.time("stmt-datetime"),
            (_, "TRUE" | "FALSE") => (n("bool", vec![a(s)]), Some(Ty::of(Fam::Bool))),
            _ => (n("dfn", vec![self.dtag(), a(lc(s))]), None),
        })
    }

    fn cast(&mut self, dst: Ty, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        // Integer to string: every dialect prints the plain decimal form.
        if dst.is_string() && dst.fam == Fam::VarChar {
            if let Some(src) = v.1 {
                if src.fam == Fam::Int
                    && dst
                        .len
                        .map_or(true, |l| l >= 20 || (l >= 11 && src.len <= Some(32)))
                {
                    let len = None;
                    return (
                        n("tostr", vec![v.0]),
                        Some(Ty {
                            fam: Fam::VarChar,
                            len,
                            scale: None,
                        }),
                    );
                }
            }
        }
        if dst.fam == Fam::Date
            && v.1
                .is_some_and(|t| matches!(t.fam, Fam::Timestamp | Fam::Date))
        {
            return (n("todate", vec![v.0]), Some(dst));
        }
        if let Some(src) = v.1 {
            if src == dst {
                return v;
            }
        }
        (n("cast", vec![Self::ty_term(&Some(dst)), v.0]), Some(dst))
    }

    fn call(&mut self, c: &Call) -> R<(Term, Option<Ty>)> {
        let name = lc(c.name.last().map(String::as_str).unwrap_or(""));
        if c.name.len() > 1 && !(c.name.len() == 2 && c.name[0].eq_ignore_ascii_case("dbo")) {
            let mut args = vec![
                self.dtag(),
                a(c.name.iter().map(|x| lc(x)).collect::<Vec<_>>().join(".")),
            ];
            for x in &c.args {
                args.push(self.expr(x)?.0);
            }
            return Ok((n("dfn", args), None));
        }
        let d = self.d;
        let argc = c.args.len();
        let plain = !c.distinct && c.order.is_empty() && !c.star;
        // Aggregates and functions with special forms first.
        match (d, name.as_str()) {
            (_, "count") if c.order.is_empty() => {
                let mut kids = vec![a(c.distinct.to_string())];
                if c.star {
                    kids.push(a("*"));
                } else {
                    for x in &c.args {
                        kids.push(self.expr(x)?.0);
                    }
                }
                // COUNT returns int in T-SQL and bigint elsewhere; equal while it fits.
                return Ok((n("count", kids), Some(Ty::of(Fam::Int))));
            }
            (Dialect::Tsql, "string_agg")
            | (Dialect::Postgres, "string_agg")
            | (Dialect::Db2i, "listagg")
                if argc == 2 && !c.distinct =>
            {
                let (x, _) = self.expr(&c.args[0])?;
                let (sep, _) = self.expr(&c.args[1])?;
                let order = self.order_terms(&c.order, None)?;
                match d {
                    Dialect::Tsql => self.assume(asm::STRING_AGG_8000),
                    Dialect::Db2i => self.assume(asm::LISTAGG_LEN),
                    Dialect::Postgres => {}
                }
                return Ok((
                    n("stringagg", vec![x, sep, n("order", order)]),
                    Some(Ty::of(Fam::VarChar)),
                ));
            }
            _ => {}
        }
        if !plain {
            return Err(format!(
                "{name}() with DISTINCT, ORDER BY or * is not supported"
            ));
        }
        let mut args = Vec::new();
        for x in &c.args {
            args.push(self.expr(x)?);
        }
        let first_ty = args.first().and_then(|x| x.1);
        let portable = |h: &str, args: Vec<(Term, Option<Ty>)>, ty| {
            (
                n(
                    "fn",
                    std::iter::once(a(h))
                        .chain(args.into_iter().map(|x| x.0))
                        .collect(),
                ),
                ty,
            )
        };
        Ok(match (d, name.as_str(), argc) {
            (_, "coalesce", _) => {
                let ty = args.iter().find_map(|x| x.1);
                portable("coalesce", args, ty)
            }
            (Dialect::Tsql, "isnull", 2) => {
                // ISNULL takes the type of its first argument; it equals COALESCE
                // when the replacement is stored in that type unchanged.
                let (b, bty) = (&args[1].0, args[1].1);
                let fits = matches!(b.head(), "null")
                    || (b.is("str")
                        && b.kid(0).and_then(Term::atom) == Some("")
                        && first_ty.is_some_and(|t| t.is_string()))
                    || matches!((first_ty, bty), (Some(x), Some(y)) if Self::fits(&x, &y));
                if fits {
                    portable("coalesce", args, first_ty)
                } else if let (Some(x), Some(y)) = (first_ty, bty) {
                    let _ = y;
                    (
                        n(
                            "isnull-narrow",
                            vec![
                                Self::ty_term(&Some(x)),
                                args[0].0.clone(),
                                args[1].0.clone(),
                            ],
                        ),
                        Some(x),
                    )
                } else {
                    (
                        n(
                            "dfn",
                            vec![a("tsql"), a("isnull"), args[0].0.clone(), args[1].0.clone()],
                        ),
                        first_ty,
                    )
                }
            }
            (_, "upper" | "ucase", 1) => portable("upper", args, first_ty),
            (_, "lower" | "lcase", 1) => portable("lower", args, first_ty),
            (_, "abs", 1) => portable("abs", args, first_ty),
            (_, "min" | "max", 1) => match first_ty {
                Some(t) if t.is_string() => (
                    n("dfn", vec![self.dtag(), a(name.clone()), args[0].0.clone()]),
                    first_ty,
                ),
                None => {
                    self.assume(asm::UNTYPED);
                    portable(&name, args, first_ty)
                }
                _ => portable(&name, args, first_ty),
            },
            (Dialect::Tsql, "sysdatetime", 0) => self.time("stmt"),
            (Dialect::Tsql, "getdate", 0) => self.time("stmt-datetime"),
            (Dialect::Postgres, "statement_timestamp", 0) => self.time("stmt"),
            (Dialect::Postgres, "now" | "transaction_timestamp", 0) => self.time("txn"),
            (Dialect::Postgres, "clock_timestamp", 0) => self.time("clock"),
            (Dialect::Db2i, "now", 0) => self.time("stmt"),
            (Dialect::Tsql, "scope_identity", 0) => {
                (n("scope_identity", vec![]), Some(Ty::of(Fam::Exact)))
            }
            (Dialect::Tsql, "datediff", 3) => {
                let part = args[0].0.clone();
                let unit = part.kid(1).and_then(Term::atom).unwrap_or("");
                if part.is("col") && matches!(unit, "day" | "dd" | "d") {
                    (
                        n("daydiff", vec![args[1].0.clone(), args[2].0.clone()]),
                        Some(Ty {
                            fam: Fam::Int,
                            len: Some(32),
                            scale: None,
                        }),
                    )
                } else {
                    (
                        n(
                            "dfn",
                            vec![a("tsql"), a("datediff")]
                                .into_iter()
                                .chain(args.into_iter().map(|x| x.0))
                                .collect(),
                        ),
                        None,
                    )
                }
            }
            (Dialect::Db2i, "days", 1) => {
                (n("days", vec![args[0].0.clone()]), Some(Ty::of(Fam::Int)))
            }
            (Dialect::Db2i, "varchar" | "char", 1) => {
                let dst = Ty {
                    fam: if name == "char" {
                        Fam::Char
                    } else {
                        Fam::VarChar
                    },
                    len: None,
                    scale: None,
                };
                let v = args.into_iter().next().unwrap();
                if v.1.is_some_and(|t| t.fam == Fam::Int) && name == "varchar" {
                    (n("tostr", vec![v.0]), Some(Ty::of(Fam::VarChar)))
                } else {
                    (n("cast", vec![Self::ty_term(&Some(dst)), v.0]), Some(dst))
                }
            }
            _ => {
                let ty = None;
                (
                    n(
                        "dfn",
                        std::iter::once(self.dtag())
                            .chain(std::iter::once(a(name.clone())))
                            .chain(args.into_iter().map(|x| x.0))
                            .collect(),
                    ),
                    ty,
                )
            }
        })
    }

    fn binary(&mut self, op: &str, l: &Expr, r: &Expr) -> R<(Term, Option<Ty>)> {
        if op == "AND" || op == "OR" {
            let mut parts = Vec::new();
            for side in [l, r] {
                let (t, _) = self.expr(side)?;
                if t.is(&op.to_lowercase()) {
                    parts.extend(t.kids().iter().cloned());
                } else {
                    parts.push(t);
                }
            }
            parts.sort();
            return Ok((n(&op.to_lowercase(), parts), Some(Ty::of(Fam::Bool))));
        }
        let (lt, lty) = self.expr(l)?;
        let (rt, rty) = self.expr(r)?;
        let any =
            |f: &dyn Fn(&Ty) -> bool| lty.as_ref().is_some_and(f) || rty.as_ref().is_some_and(f);
        match op {
            "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                let string = any(&|t| t.is_string());
                let known = lty.is_some() || rty.is_some();
                if string {
                    return Ok((
                        self.strcmp(op, (lt, lty), (rt, rty)),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                if !known {
                    self.assume(asm::UNTYPED);
                }
                let (x, y, op) = order_operands(op, lt, rt);
                Ok((n("cmp", vec![a(op), x, y]), Some(Ty::of(Fam::Bool))))
            }
            "||" => {
                if any(&|t| t.fam == Fam::Char) {
                    return Ok((
                        n("concat-char", vec![self.dtag(), lt, rt]),
                        Some(Ty::of(Fam::VarChar)),
                    ));
                }
                let len = match (lty.and_then(|t| t.len), rty.and_then(|t| t.len)) {
                    (Some(x), Some(y)) => Some(x + y),
                    _ => None,
                };
                Ok((
                    n("concat", vec![lt, rt]),
                    Some(Ty {
                        fam: Fam::VarChar,
                        len,
                        scale: None,
                    }),
                ))
            }
            "+" if self.d == Dialect::Tsql && (any(&|t| t.is_string()) || l_or_r_is_str(l, r)) => {
                if any(&|t| t.is_numeric() || t.is_temporal()) {
                    return Ok((n("dfn", vec![a("tsql"), a("+"), lt, rt]), None));
                }
                if lty.is_none() || rty.is_none() {
                    self.assume(asm::TSQL_PLUS_STRING);
                }
                if any(&|t| t.fam == Fam::Char) {
                    return Ok((
                        n("concat-char", vec![self.dtag(), lt, rt]),
                        Some(Ty::of(Fam::VarChar)),
                    ));
                }
                let len = match (lty.and_then(|t| t.len), rty.and_then(|t| t.len)) {
                    (Some(x), Some(y)) => Some(x + y),
                    _ => None,
                };
                Ok((
                    n("concat", vec![lt, rt]),
                    Some(Ty {
                        fam: Fam::VarChar,
                        len,
                        scale: None,
                    }),
                ))
            }
            "-" if lt.is("days") && rt.is("days") => Ok((
                n("daydiff", vec![rt.kids()[0].clone(), lt.kids()[0].clone()]),
                Some(Ty {
                    fam: Fam::Int,
                    len: Some(32),
                    scale: None,
                }),
            )),
            "-" if lt.is("todate") && rt.is("todate") && self.d == Dialect::Postgres => Ok((
                n("daydiff", vec![rt.kids()[0].clone(), lt.kids()[0].clone()]),
                Some(Ty {
                    fam: Fam::Int,
                    len: Some(32),
                    scale: None,
                }),
            )),
            "+" | "-" | "*" | "/" | "%" => {
                if any(&|t| t.is_temporal()) || any(&|t| t.is_string()) {
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                }
                let both_int = lty.is_some_and(|t| t.fam == Fam::Int)
                    && rty.is_some_and(|t| t.fam == Fam::Int);
                if lty.is_none() && rty.is_none() {
                    self.assume(asm::UNTYPED);
                }
                if matches!(op, "*" | "/" | "%") && !both_int {
                    // Decimal precision and scale rules differ per dialect.
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                }
                let ty = if both_int { lty } else { lty.or(rty) };
                let h = match op {
                    "+" => "add",
                    "-" => "sub",
                    "*" => "mul",
                    "/" => "div",
                    _ => "mod",
                };
                if h == "add" || h == "mul" {
                    let mut v = vec![lt, rt];
                    v.sort();
                    return Ok((n(h, v), ty));
                }
                Ok((n(h, vec![lt, rt]), ty))
            }
            _ => Ok((n("dfn", vec![self.dtag(), a(op), lt, rt]), None)),
        }
    }

    /// String comparison with its collation behaviour made explicit:
    /// case sensitivity (`ci` / `cs`) and trailing blanks (`pad` / `nopad`).
    fn strcmp(&mut self, op: &str, l: (Term, Option<Ty>), r: (Term, Option<Ty>)) -> Term {
        let folded = |t: &Term| {
            t.is("fn") && matches!(t.kid(0).and_then(Term::atom), Some("upper" | "lower"))
        };
        let (mut lt, mut rt) = (l.0, r.0);
        let both_folded = folded(&lt) && folded(&rt);
        let fold = if both_folded {
            let f = lt.kid(0).and_then(Term::atom).unwrap_or("").to_string();
            let g = rt.kid(0).and_then(Term::atom).unwrap_or("").to_string();
            lt = lt.kids()[1].clone();
            rt = rt.kids()[1].clone();
            if f != g {
                return n("cmp-fold-mixed", vec![self.dtag(), a(op), lt, rt]);
            }
            true
        } else {
            false
        };
        let ordering = op != "=" && op != "<>";
        if ordering {
            // Ordering comparisons depend on the collation, not only on case and padding.
            return n("strcmp-order", vec![self.dtag(), a(op), lt, rt]);
        }
        let char_l = l.1.is_some_and(|t| t.fam == Fam::Char);
        let char_r = r.1.is_some_and(|t| t.fam == Fam::Char);
        let ci = if fold {
            if self.d != Dialect::Tsql {
                self.assume(asm::CASE_FOLD);
            }
            "ci"
        } else {
            match self.d {
                Dialect::Tsql => {
                    self.assume(asm::TSQL_CI);
                    "ci"
                }
                Dialect::Postgres => "cs",
                Dialect::Db2i => {
                    self.assume(asm::DB2_SRTSEQ);
                    "cs"
                }
            }
        };
        let pad = match self.d {
            Dialect::Tsql | Dialect::Db2i => "pad",
            Dialect::Postgres if char_l && char_r => "pad",
            Dialect::Postgres if char_l || char_r => "pad-mixed",
            Dialect::Postgres => "nopad",
        };
        let (x, y, op) = order_operands(op, lt, rt);
        n("strcmp", vec![a(op), a(ci), a(pad), x, y])
    }

    fn order_terms(&mut self, items: &[OrderItem], total_from: Option<&str>) -> R<Vec<Term>> {
        let _ = total_from;
        let mut v = Vec::new();
        for o in items {
            let (t, ty) = self.expr(&o.expr)?;
            let nulls_first = o.nulls_first.unwrap_or(match self.d {
                // T-SQL: NULL is the lowest value. PostgreSQL and Db2 for i: the highest
                // (fact V19, Db2 for i part not checked on a real system).
                Dialect::Tsql => !o.desc,
                Dialect::Postgres | Dialect::Db2i => o.desc,
            });
            let nulls = match self.not_null(&t) {
                Some(true) => "n/a".to_string(),
                Some(false) => if nulls_first { "nf" } else { "nl" }.to_string(),
                None => if nulls_first { "nf?" } else { "nl?" }.to_string(),
            };
            let mut kids = vec![t, a(if o.desc { "desc" } else { "asc" }), a(nulls)];
            match ty {
                // Character ordering depends on the collation of each dialect.
                Some(t) if t.is_string() => kids.push(self.dtag()),
                None => self.assume(asm::UNTYPED),
                _ => {}
            }
            v.push(n("ord", kids));
        }
        Ok(v)
    }

    // ---- queries --------------------------------------------------------

    fn push_from(&mut self, from: &[From]) -> R<Vec<Term>> {
        let mut terms = Vec::new();
        for f in from {
            terms.push(self.table_ref(f)?);
        }
        Ok(terms)
    }

    fn table_ref(&mut self, f: &From) -> R<Term> {
        match f {
            From::Table { name, alias, hints } => {
                if !hints.is_empty() {
                    return Err("table hints in FROM are not supported".into());
                }
                let b = self.bind_table(name, alias);
                let t = n("t", vec![a(b.table.clone())]);
                self.scopes.last_mut().unwrap().push(b);
                Ok(t)
            }
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                let l = self.table_ref(left)?;
                let r = self.table_ref(right)?;
                let on = match on {
                    Some(e) => self.expr(e)?.0,
                    None => a("-"),
                };
                Ok(n("join", vec![a(kind.to_lowercase()), l, r, on]))
            }
            From::Sub { query, alias, cols } => {
                if !cols.is_empty() {
                    return Err("derived column lists are not supported".into());
                }
                let saved = std::mem::take(&mut self.scopes);
                let q = self.query(query, QCtx::Set);
                self.scopes = saved;
                let q = q?;
                let key = alias.clone().map(|x| lc(&x)).unwrap_or_default();
                self.scopes.last_mut().unwrap().push(Binding {
                    key: key.clone(),
                    table: format!("sub:{key}"),
                    derived: None,
                });
                Ok(n("sub", vec![q.0]))
            }
            From::Values { .. } | From::DataChange { .. } => {
                Err("VALUES or data-change table in FROM is not supported here".into())
            }
        }
    }

    /// Lowers a query; returns the term and the type of its first column.
    pub fn query(&mut self, s: &Select, ctx: QCtx) -> R<(Term, Option<Ty>)> {
        let (mut parts, items) = self.query_parts(s, ctx)?;
        let ty = items.first().and_then(|x| x.1);
        let item_terms: Vec<Term> = items.into_iter().map(|x| x.0).collect();
        parts.insert(1, n("items", item_terms));
        Ok((n("q", parts), ty))
    }

    /// Query parts without the select list, plus the select items.
    fn query_parts(&mut self, s: &Select, ctx: QCtx) -> R<Parts> {
        if let Some(rows) = &s.values {
            let mut rs = Vec::new();
            for r in rows {
                let mut v = Vec::new();
                for x in r {
                    v.push(self.expr(x)?.0);
                }
                rs.push(n("row", v));
            }
            return Ok((vec![n("values", rs)], Vec::new()));
        }
        if !s.into.is_empty() && ctx != QCtx::Into {
            return Err("SELECT INTO here is not supported".into());
        }
        self.scopes.push(Vec::new());
        let r = self.query_inner(s, ctx);
        self.scopes.pop();
        r
    }

    fn query_inner(&mut self, s: &Select, ctx: QCtx) -> R<Parts> {
        let from = self.push_from(&s.from)?;
        let mut items = Vec::new();
        for it in &s.items {
            if it.assign.is_some() && ctx != QCtx::Into {
                return Err("assignment in a SELECT list here is not supported".into());
            }
            let (t, ty) = self.expr(&it.expr)?;
            let name = it
                .alias
                .clone()
                .map(|x| lc(&x))
                .or_else(|| match &it.expr {
                    Expr::Name(p) => p.last().map(|x| lc(x)),
                    _ => None,
                })
                .unwrap_or_default();
            let t = match ctx {
                QCtx::Set => n("item", vec![t, a(name)]),
                _ => t,
            };
            items.push((t, ty));
        }
        if ctx == QCtx::Exists {
            items.clear();
        }
        let where_ = match &s.where_ {
            Some(w) => self.expr(w)?.0,
            None => a("-"),
        };
        let mut group = Vec::new();
        for g in &s.group_by {
            let (t, ty) = self.expr(g)?;
            group.push(match ty {
                Some(t2) if t2.is_string() => n("collate", vec![self.dtag(), t]),
                None => {
                    self.assume(asm::UNTYPED);
                    t
                }
                _ => t,
            });
        }
        // DISTINCT compares strings under each dialect's collation.
        let distinct = if !s.distinct {
            a("all")
        } else if items.iter().any(|x| x.1.is_some_and(|t| t.is_string())) {
            n("distinct", vec![self.dtag()])
        } else {
            if items.iter().any(|x| x.1.is_none()) {
                self.assume(asm::UNTYPED);
            }
            a("distinct")
        };
        let having = match &s.having {
            Some(h) => self.expr(h)?.0,
            None => a("-"),
        };
        let order = self.order_terms(&s.order_by, None)?;
        let limit = match (&s.top, &s.limit) {
            (Some(t), _) | (None, Some(t)) => Some(self.expr(t)?.0),
            _ => None,
        };
        let offset = match &s.offset {
            Some(o) => Some(self.expr(o)?.0),
            None => None,
        };
        let page = if limit.is_some() || offset.is_some() {
            if !self.total_order(&from, &order) {
                self.assume(asm::ORDER_TOTAL);
            }
            n(
                "page",
                vec![
                    offset.unwrap_or(n("num", vec![a("0")])),
                    limit.unwrap_or(a("all")),
                ],
            )
        } else {
            a("-")
        };
        let lock = if s.locking.is_empty() {
            a("-")
        } else {
            n(
                "lock",
                std::iter::once(self.dtag())
                    .chain(s.locking.iter().map(a))
                    .collect(),
            )
        };
        Ok((
            vec![
                distinct,
                n("from", from),
                where_,
                n("group", group),
                having,
                n("order", order),
                page,
                lock,
            ],
            items,
        ))
    }

    /// The ORDER BY keys include a unique key of the only table in FROM.
    fn total_order(&mut self, from: &[Term], order: &[Term]) -> bool {
        let [f] = from else { return false };
        if !f.is("t") {
            return false;
        }
        let table = f.kid(0).and_then(Term::atom).unwrap_or("").to_string();
        let cols: Vec<String> = order
            .iter()
            .filter_map(|o| o.kid(0))
            .filter(|c| c.is("col") && c.kid(0).and_then(Term::atom) == Some(table.as_str()))
            .filter_map(|c| c.kid(1).and_then(Term::atom).map(str::to_string))
            .collect();
        match self.schema.table(Self::base_table(&table)) {
            Some(t) if t.is_unique(&cols) => {
                self.discharge(format!(
                    "ORDER BY {} is unique in {table} (catalog)",
                    cols.join(", ")
                ));
                true
            }
            _ => false,
        }
    }

    /// The query returns at most one row: equality on a unique key of the
    /// only table, or an aggregate without GROUP BY.
    fn at_most_one(&mut self, q: &Term) -> bool {
        let Some(Term::N(_, from)) = q.kid(2) else {
            return false;
        };
        let items = q.kid(1).map(Term::kids).unwrap_or(&[]);
        let group = q.kid(4).map(Term::kids).unwrap_or(&[]);
        if group.is_empty()
            && !items.is_empty()
            && items.iter().all(|i| {
                i.is("count")
                    || i.is("stringagg")
                    || (i.is("fn") && matches!(i.kid(0).and_then(Term::atom), Some("min" | "max")))
            })
        {
            return true;
        }
        let [f] = from.as_slice() else { return false };
        if !f.is("t") {
            return false;
        }
        let table = f.kid(0).and_then(Term::atom).unwrap_or("").to_string();
        let w = q.kid(3).cloned().unwrap_or(a("-"));
        let conj: Vec<Term> = if w.is("and") {
            w.kids().to_vec()
        } else {
            vec![w]
        };
        let mut cols = Vec::new();
        for c in &conj {
            let (x, y) = match c.head() {
                "cmp" if c.kid(0).and_then(Term::atom) == Some("=") => (c.kid(1), c.kid(2)),
                // Only the dialect's own comparison matches its unique indexes: a
                // folded (upper/lower) comparison can match several keys.
                "strcmp"
                    if c.kid(0).and_then(Term::atom) == Some("=")
                        && c.kid(1).and_then(Term::atom)
                            == Some(if self.d == Dialect::Tsql { "ci" } else { "cs" }) =>
                {
                    (c.kid(3), c.kid(4))
                }
                _ => continue,
            };
            let (Some(x), Some(y)) = (x, y) else { continue };
            let refs_table = |t: &Term| {
                t.any(&|s| s.is("col") && s.kid(0).and_then(Term::atom) == Some(table.as_str()))
            };
            for (col, other) in [(x, y), (y, x)] {
                if col.is("col")
                    && col.kid(0).and_then(Term::atom) == Some(table.as_str())
                    && !refs_table(other)
                {
                    if let Some(c) = col.kid(1).and_then(Term::atom) {
                        cols.push(c.to_string());
                    }
                }
            }
        }
        match self.schema.table(Self::base_table(&table)) {
            Some(t) if t.is_unique(&cols) => {
                let key = t
                    .keys
                    .iter()
                    .find(|k| k.iter().all(|c| cols.contains(c)))
                    .cloned()
                    .unwrap_or_default();
                self.discharge(format!(
                    "{table}.{} is a unique key (catalog)",
                    key.join(", ")
                ));
                true
            }
            _ => false,
        }
    }

    /// `scalar(query, on-several-rows, on-zero-rows)`.
    fn scalar(&mut self, q: Term, many: &str, zero: &str) -> Term {
        let many = if self.at_most_one(&q) {
            "n/a".to_string()
        } else if self.uniqueness_known(&q) {
            many.to_string()
        } else {
            // Without catalog facts for the table, "several rows" may be impossible.
            format!("{many}?")
        };
        n("scalar", vec![q, a(many), a(zero)])
    }

    /// The query reads one table that the catalog describes.
    fn uniqueness_known(&self, q: &Term) -> bool {
        let Some(Term::N(_, from)) = q.kid(2) else {
            return false;
        };
        match from.as_slice() {
            [f] if f.is("t") => f
                .kid(0)
                .and_then(Term::atom)
                .is_some_and(|t| self.schema.table(Self::base_table(t)).is_some()),
            _ => false,
        }
    }

    // ---- statements -----------------------------------------------------

    fn target_sym(&mut self, e: &Expr) -> R<(Term, Option<Ty>)> {
        let s = match e {
            Expr::Var(v) => self.sym(v),
            Expr::Name(p) if p.len() == 1 => self.sym(&p[0]),
            _ => None,
        };
        s.ok_or_else(|| "assignment target is not a parameter or local variable".to_string())
    }

    pub fn stmt(&mut self, s: &Stmt) -> R<Term> {
        self.seen_tables.clear();
        self.single_row = false;
        match s {
            Stmt::Select(sel) => self.select_stmt(sel),
            Stmt::Insert {
                target,
                cols,
                source,
                returning,
                returning_into,
                on_conflict,
            } => {
                if cols.is_empty() {
                    return Err("INSERT without a column list".into());
                }
                let table = self.table_key(target);
                let cols: Vec<String> = cols.iter().map(|c| lc(c)).collect();
                if let Some((keys, action)) = on_conflict {
                    return self.on_conflict(&table, &cols, source, keys, action, returning);
                }
                let ins = self.insert(&table, &cols, source)?;
                if returning.is_empty() {
                    return Ok(ins);
                }
                self.returning(ins, &table, returning, returning_into)
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => {
                if !from.is_empty() {
                    return Err("UPDATE ... FROM is not supported".into());
                }
                self.scopes.push(Vec::new());
                let b = self.bind_table(target, alias);
                let table = b.table.clone();
                self.scopes.last_mut().unwrap().push(b);
                let r = (|| {
                    let mut kv = Vec::new();
                    for (cols, e) in sets {
                        let (v, vty) = self.expr(e)?;
                        if cols.len() == 1 {
                            let col = lc(&cols[0]);
                            let (cty, _) = self.col_ty(&table, &col);
                            let v = self.conv(&cty, (v, vty)).0;
                            kv.push(n("kv", vec![a(col), v]));
                        } else {
                            // `SET (a, b) = (subquery)`: errors on several rows, NULLs on none.
                            let v = if v.is("subq") {
                                self.scalar(v.kids()[0].clone(), "error", "null")
                            } else {
                                v
                            };
                            kv.push(n(
                                "kvs",
                                vec![n("cols", cols.iter().map(|c| a(lc(c))).collect()), v],
                            ));
                        }
                    }
                    kv.sort();
                    let w = match where_ {
                        Some(w) => self.expr(w)?.0,
                        None => a("-"),
                    };
                    Ok(n("update", vec![a(table.clone()), n("set", kv), w]))
                })();
                self.scopes.pop();
                r
            }
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => self.delete(target, alias, from, where_),
            Stmt::Merge {
                target,
                alias,
                hints,
                source,
                on,
                whens,
            } => self.merge(target, alias, hints, source, on, whens),
            Stmt::Assign(t, v) => {
                let (sym, ty) = self.target_sym(t)?;
                self.single_row = true;
                let val = match v {
                    Expr::Subquery(q) => {
                        let (q, qty) = self.query(q, QCtx::Scalar)?;
                        (self.scalar(q, "error", "null"), qty)
                    }
                    e => self.expr(e)?,
                };
                if val.0.is("scope_identity") {
                    return Ok(n("assign", vec![sym, val.0]));
                }
                let val = self.conv(&ty, val).0;
                Ok(n("assign", vec![sym, val]))
            }
            Stmt::Return(None) => Ok(n("return", vec![])),
            Stmt::Return(Some(e)) => {
                self.single_row = true;
                match (self.unit.kind, e) {
                    (UnitKind::TableFn, Expr::Subquery(q)) => self.resultset(q),
                    (_, Expr::Subquery(q)) => {
                        let (q, qty) = self.query(q, QCtx::Scalar)?;
                        let v = (self.scalar(q, "error", "null"), qty);
                        let ret = self.unit.returns;
                        Ok(n("return", vec![self.conv(&ret, v).0]))
                    }
                    _ => {
                        let v = self.expr(e)?;
                        let ret = self.unit.returns;
                        Ok(n("return", vec![self.conv(&ret, v).0]))
                    }
                }
            }
            Stmt::Declare { .. } => Err("declaration".into()),
        }
    }

    fn resultset(&mut self, q: &Select) -> R<Term> {
        let (mut parts, items) = self.query_parts(q, QCtx::Set)?;
        let decl = &self.unit.ret_cols;
        let mut item_terms = Vec::new();
        if !decl.is_empty() {
            if decl.len() != items.len() {
                return Err("RETURNS TABLE columns do not match the select list".into());
            }
            for (i, (t, ty)) in items.into_iter().enumerate() {
                let expr = t.kids()[0].clone();
                let (name, dty) = decl[i].clone();
                let expr = match (dty, ty) {
                    (Some(_), Some(_)) => self.conv(&dty, (expr, ty)).0,
                    _ => {
                        self.assume(asm::RESULT_TYPES);
                        expr
                    }
                };
                item_terms.push(n("item", vec![expr, a(name)]));
            }
        } else {
            item_terms = items.into_iter().map(|x| x.0).collect();
        }
        parts.insert(1, n("items", item_terms));
        Ok(n("resultset", vec![n("q", parts)]))
    }

    fn select_stmt(&mut self, sel: &Select) -> R<Term> {
        let assigns: Vec<&SelItem> = sel.items.iter().filter(|i| i.assign.is_some()).collect();
        if !assigns.is_empty() {
            // T-SQL `SELECT @v = col FROM ...`: keeps the last row; zero rows leave @v unchanged.
            if assigns.len() != sel.items.len() || assigns.len() != 1 {
                return Err("SELECT assigning several variables is not supported".into());
            }
            let (sym, ty) = self.target_sym(&Expr::Var(assigns[0].assign.clone().unwrap()))?;
            let (q, qty) = self.query(sel, QCtx::Into)?;
            let v = (self.scalar(q, "last", "unchanged"), qty);
            return Ok(n("assign", vec![sym, self.conv(&ty, v).0]));
        }
        if !sel.into.is_empty() {
            if let [From::DataChange {
                kind,
                stmt,
                alias: _,
            }] = sel.from.as_slice()
            {
                return self.final_table(sel, kind, stmt);
            }
            if sel.into.len() != 1 {
                return Err("SELECT INTO several variables is not supported".into());
            }
            let (sym, ty) = self.target_sym(&sel.into[0])?;
            let (q, qty) = self.query(sel, QCtx::Into)?;
            let (many, zero) = match self.d {
                Dialect::Postgres if sel.into_strict => ("error", "error"),
                Dialect::Postgres => ("first", "null"),
                _ => ("error", "unchanged"),
            };
            let v = (self.scalar(q, many, zero), qty);
            return Ok(n("assign", vec![sym, self.conv(&ty, v).0]));
        }
        match (self.d, self.unit.kind, self.unit.sql_body) {
            (Dialect::Postgres, UnitKind::ScalarFn, true) => {
                let (q, qty) = self.query(sel, QCtx::Scalar)?;
                let v = (self.scalar(q, "first", "null"), qty);
                let ret = self.unit.returns;
                Ok(n("return", vec![self.conv(&ret, v).0]))
            }
            (Dialect::Postgres, UnitKind::ScalarFn | UnitKind::TableFn | UnitKind::Proc, false) => {
                Err("SELECT without INTO in PL/pgSQL".into())
            }
            _ => self.resultset(sel),
        }
    }

    fn insert(&mut self, table: &str, cols: &[String], source: &InsertSource) -> R<Term> {
        match source {
            InsertSource::Values(rows) if rows.len() == 1 => {
                self.single_row = true;
                let kv = self.insert_row(table, cols, &rows[0])?;
                Ok(n(
                    "insert1",
                    vec![
                        a(table),
                        n(
                            "set",
                            kv.into_iter()
                                .map(|(c, v, _)| n("kv", vec![a(c), v]))
                                .collect(),
                        ),
                    ],
                ))
            }
            InsertSource::Values(_) => Err("multi-row VALUES is not supported".into()),
            InsertSource::Query(q) => {
                let (parts, items) = self.query_parts(q, QCtx::Scalar)?;
                if items.len() != cols.len() {
                    return Err("INSERT column count does not match the query".into());
                }
                let mut kv: Vec<Term> = cols
                    .iter()
                    .zip(items)
                    .map(|(c, (v, vty))| {
                        let (cty, _) = self.col_ty(table, c);
                        n("kv", vec![a(c.clone()), self.conv(&cty, (v, vty)).0])
                    })
                    .collect();
                kv.sort();
                Ok(n("insertq", vec![a(table), n("set", kv), n("q", parts)]))
            }
            InsertSource::Default => Ok(n("insertdefault", vec![a(table)])),
        }
    }

    fn insert_row(
        &mut self,
        table: &str,
        cols: &[String],
        row: &[Expr],
    ) -> R<Vec<(String, Term, Option<Ty>)>> {
        if row.len() != cols.len() {
            return Err("INSERT column count does not match VALUES".into());
        }
        let mut kv = Vec::new();
        for (c, e) in cols.iter().zip(row) {
            let v = self.expr(e)?;
            let (cty, _) = self.col_ty(table, c);
            let (t, ty) = self.conv(&cty, v);
            kv.push((c.clone(), t, ty));
        }
        kv.sort_by(|x, y| x.0.cmp(&y.0));
        Ok(kv)
    }

    fn returning(
        &mut self,
        ins: Term,
        table: &str,
        returning: &[SelItem],
        into: &[Expr],
    ) -> R<Term> {
        if !ins.is("insert1") {
            return Err("RETURNING on a multi-row insert is not supported".into());
        }
        self.scopes.push(vec![Binding {
            key: table.to_string(),
            table: table.to_string(),
            derived: None,
        }]);
        let items: R<Vec<Term>> = returning
            .iter()
            .map(|i| self.expr(&i.expr).map(|x| x.0))
            .collect();
        self.scopes.pop();
        let mut targets = Vec::new();
        for t in into {
            targets.push(self.target_sym(t)?.0);
        }
        if targets.is_empty() {
            return Err("RETURNING without INTO is not supported".into());
        }
        Ok(n(
            "insret",
            vec![ins, n("items", items?), n("into", targets)],
        ))
    }

    /// Db2 `SELECT c INTO v FROM FINAL TABLE (INSERT ...)`.
    fn final_table(&mut self, sel: &Select, kind: &str, stmt: &Stmt) -> R<Term> {
        if kind != "FINAL"
            || sel.where_.is_some()
            || !sel.group_by.is_empty()
            || !sel.order_by.is_empty()
        {
            return Err("unsupported data-change table reference".into());
        }
        let Stmt::Insert {
            target,
            cols,
            source,
            returning,
            on_conflict,
            ..
        } = stmt
        else {
            return Err("FINAL TABLE of a statement other than INSERT".into());
        };
        if !returning.is_empty() || on_conflict.is_some() {
            return Err("unsupported INSERT in FINAL TABLE".into());
        }
        let table = self.table_key(target);
        let cols: Vec<String> = cols.iter().map(|c| lc(c)).collect();
        let ins = self.insert(&table, &cols, source)?;
        self.returning(ins, &table, &sel.items, &sel.into)
    }

    fn on_conflict(
        &mut self,
        table: &str,
        cols: &[String],
        source: &InsertSource,
        keys: &[String],
        action: &ConflictAction,
        returning: &[SelItem],
    ) -> R<Term> {
        let InsertSource::Values(rows) = source else {
            return Err("ON CONFLICT with a query source is not supported".into());
        };
        if rows.len() != 1 || !returning.is_empty() {
            return Err("only single-row ON CONFLICT is supported".into());
        }
        let ConflictAction::Update(sets, None) = action else {
            return Err("ON CONFLICT DO NOTHING or with WHERE is not supported".into());
        };
        self.single_row = true;
        let kv = self.insert_row(table, cols, &rows[0])?;
        let keys: Vec<String> = keys.iter().map(|k| lc(k)).collect();
        let mut keyvals = Vec::new();
        for k in &keys {
            let Some((_, v, _)) = kv.iter().find(|(c, _, _)| c == k) else {
                return Err("ON CONFLICT key is not an inserted column".into());
            };
            keyvals.push(n("kv", vec![a(k.clone()), v.clone()]));
        }
        self.excluded = Some(kv.clone());
        self.scopes.push(vec![Binding {
            key: table.to_string(),
            table: table.to_string(),
            derived: None,
        }]);
        let upd: R<Vec<Term>> = sets
            .iter()
            .map(|(c, e)| {
                let v = self.expr(e)?;
                let c = lc(c);
                let (cty, _) = self.col_ty(table, &c);
                Ok(n("kv", vec![a(c), self.conv(&cty, v).0]))
            })
            .collect();
        self.scopes.pop();
        self.excluded = None;
        let mut upd = upd?;
        upd.sort();
        Ok(self.upsert(table, keyvals, kv, upd, "safe"))
    }

    fn upsert(
        &mut self,
        table: &str,
        mut keyvals: Vec<Term>,
        ins: Vec<(String, Term, Option<Ty>)>,
        upd: Vec<Term>,
        conc: &str,
    ) -> Term {
        keyvals.sort();
        n(
            "upsert",
            vec![
                a(table),
                n("key", keyvals),
                n(
                    "insert",
                    ins.into_iter()
                        .map(|(c, v, _)| n("kv", vec![a(c), v]))
                        .collect(),
                ),
                n("update", upd),
                n("concurrency", vec![a(conc)]),
            ],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn merge(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        hints: &[String],
        source: &From,
        on: &Expr,
        whens: &[super::ast::When],
    ) -> R<Term> {
        let table = self.table_key(target);
        self.single_row = true;
        // Single-row source: SELECT without FROM, or one VALUES row.
        let src: Vec<(String, Term, Option<Ty>)> = match source {
            From::Sub {
                query,
                alias: Some(sa),
                cols,
            } if query.from.is_empty()
                && query.values.is_none()
                && cols.is_empty()
                && query.where_.is_none() =>
            {
                let mut v = Vec::new();
                for it in &query.items {
                    let name = it
                        .alias
                        .clone()
                        .map(|x| lc(&x))
                        .ok_or("unnamed MERGE source column")?;
                    let (t, ty) = self.expr(&it.expr)?;
                    v.push((name, t, ty));
                }
                let _ = sa;
                v
            }
            From::Values {
                rows,
                alias: Some(_),
                cols,
            } if rows.len() == 1 && rows[0].len() == cols.len() => {
                let mut v = Vec::new();
                for (c, e) in cols.iter().zip(&rows[0]) {
                    let (t, ty) = self.expr(e)?;
                    v.push((lc(c), t, ty));
                }
                v
            }
            _ => return Err("MERGE from a multi-row source is not supported".into()),
        };
        let src_key = match source {
            From::Sub {
                alias: Some(sa), ..
            }
            | From::Values {
                alias: Some(sa), ..
            } => lc(sa),
            _ => String::new(),
        };
        let tb = Binding {
            key: alias
                .clone()
                .map(|x| lc(&x))
                .unwrap_or_else(|| table.clone()),
            table: table.clone(),
            derived: None,
        };
        self.scopes.push(vec![
            tb,
            Binding {
                key: src_key,
                table: "merge:source".into(),
                derived: Some(src),
            },
        ]);
        let r = self.merge_inner(&table, hints, on, whens);
        self.scopes.pop();
        r
    }

    fn merge_inner(
        &mut self,
        table: &str,
        hints: &[String],
        on: &Expr,
        whens: &[super::ast::When],
    ) -> R<Term> {
        // ON: conjunction of target.col = source value.
        let on_t = self.expr(on)?.0;
        let conj: Vec<Term> = if on_t.is("and") {
            on_t.kids().to_vec()
        } else {
            vec![on_t]
        };
        let mut keyvals = Vec::new();
        for c in conj {
            let (x, y) = match c.head() {
                "cmp" if c.kid(0).and_then(Term::atom) == Some("=") => {
                    (c.kids()[1].clone(), c.kids()[2].clone())
                }
                _ => return Err("MERGE ON is not a key equality".into()),
            };
            let is_target = |t: &Term| t.is("col") && t.kid(0).and_then(Term::atom) == Some(table);
            let (col, val) = if is_target(&x) && !is_target(&y) {
                (x, y)
            } else if is_target(&y) && !is_target(&x) {
                (y, x)
            } else {
                return Err("MERGE ON is not a key equality".into());
            };
            keyvals.push(n("kv", vec![col.kids()[1].clone(), val]));
        }
        let mut ins = None;
        let mut upd = None;
        for w in whens {
            if w.cond.is_some() || w.by_source {
                return Err("conditional or BY SOURCE MERGE clauses are not supported".into());
            }
            match (&w.action, w.matched) {
                (MergeAction::Update(sets), true) if upd.is_none() => {
                    let mut kv = Vec::new();
                    for (c, e) in sets {
                        let v = self.expr(e)?;
                        let c = lc(c);
                        let (cty, _) = self.col_ty(table, &c);
                        kv.push(n("kv", vec![a(c), self.conv(&cty, v).0]));
                    }
                    kv.sort();
                    upd = Some(kv);
                }
                (MergeAction::Insert(cols, vals), false)
                    if ins.is_none() && cols.len() == vals.len() && !cols.is_empty() =>
                {
                    let cols: Vec<String> = cols.iter().map(|c| lc(c)).collect();
                    ins = Some(self.insert_row(table, &cols, vals)?);
                }
                _ => return Err("unsupported MERGE clause combination".into()),
            }
        }
        let (Some(ins), Some(upd)) = (ins, upd) else {
            return Err("MERGE is not an upsert".into());
        };
        // The inserted key must equal the matched key.
        for kv in &keyvals {
            let col = kv.kids()[0].atom().unwrap_or("");
            if ins.iter().find(|(c, _, _)| c == col).map(|x| &x.1) != Some(&kv.kids()[1]) {
                return Err("MERGE inserts a different key than it matches".into());
            }
        }
        let key_cols: Vec<String> = keyvals
            .iter()
            .filter_map(|k| k.kids()[0].atom().map(str::to_string))
            .collect();
        match self.schema.table(table) {
            Some(t) if t.is_unique(&key_cols) => {
                self.discharge(format!(
                    "{table}.{} is a unique key (catalog)",
                    key_cols.join(", ")
                ));
            }
            _ => self.assume((
                "ASM-UPSERT-KEY-UNIQUE",
                "The MERGE match columns are a unique key of the target",
            )),
        }
        let conc = match self.d {
            Dialect::Tsql => {
                if hints.iter().any(|h| h == "HOLDLOCK" || h == "SERIALIZABLE") {
                    "safe"
                } else {
                    "race"
                }
            }
            Dialect::Postgres => "race",
            Dialect::Db2i => {
                self.assume(asm::DB2_MERGE_CONCURRENT);
                "safe"
            }
        };
        Ok(self.upsert(table, keyvals, ins, upd, conc))
    }

    fn delete(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        from: &[From],
        where_: &Option<Expr>,
    ) -> R<Term> {
        self.scopes.push(Vec::new());
        let r = (|| {
            if from.is_empty() {
                let b = self.bind_table(target, alias);
                let table = b.table.clone();
                self.scopes.last_mut().unwrap().push(b);
                let w = match where_ {
                    Some(w) => self.expr(w)?.0,
                    None => a("-"),
                };
                return Ok(n("delete", vec![a(table), w]));
            }
            self.anti_join_delete(target, from, where_)
        })();
        self.scopes.pop();
        r
    }

    /// `DELETE t FROM t LEFT JOIN u ON <eqs> WHERE u.c IS NULL [AND rest]`
    /// is `DELETE FROM t WHERE NOT EXISTS (SELECT 1 FROM u WHERE <eqs>) [AND rest]`
    /// when `u.c` takes part in an equality of the ON clause: a matched row
    /// has `u.c` non-null, so `u.c IS NULL` holds exactly for unmatched rows.
    fn anti_join_delete(
        &mut self,
        target: &[String],
        from: &[From],
        where_: &Option<Expr>,
    ) -> R<Term> {
        let unsupported =
            || Err("DELETE with FROM/USING is not supported unless it is an anti-join".to_string());
        let [From::Join {
            kind,
            left,
            right,
            on: Some(on),
        }] = from
        else {
            return unsupported();
        };
        if kind != "LEFT" {
            return unsupported();
        }
        let (
            From::Table {
                name: lname,
                alias: lalias,
                hints: lh,
            },
            From::Table {
                name: rname,
                alias: ralias,
                hints: rh,
            },
        ) = (left.as_ref(), right.as_ref())
        else {
            return unsupported();
        };
        if !lh.is_empty() || !rh.is_empty() || target.len() != 1 {
            return unsupported();
        }
        let tkey = lc(&target[0]);
        let lkey = lalias
            .clone()
            .map(|x| lc(&x))
            .unwrap_or_else(|| lc(lname.last().unwrap()));
        if tkey != lkey && tkey != lc(lname.last().unwrap()) {
            return unsupported();
        }
        let rkey = ralias
            .clone()
            .map(|x| lc(&x))
            .unwrap_or_else(|| lc(rname.last().unwrap()));
        // WHERE: u.c IS NULL [AND rest].
        let Some(w) = where_ else {
            return unsupported();
        };
        let mut conj = Vec::new();
        flatten_and(w, &mut conj);
        let is_null_pos = conj.iter().position(|c| {
            matches!(c, Expr::IsNull(x, false) if matches!(x.as_ref(), Expr::Name(p) if p.len() == 2 && lc(&p[0]) == rkey))
        });
        let Some(pos) = is_null_pos else {
            return unsupported();
        };
        let Expr::IsNull(x, _) = conj.remove(pos) else {
            unreachable!()
        };
        let Expr::Name(p) = *x else { unreachable!() };
        let null_col = lc(&p[1]);
        let mut on_conj = Vec::new();
        flatten_and(on, &mut on_conj);
        let in_eq = on_conj.iter().any(|c| match c {
            Expr::Bin(op, l, r) if op == "=" => [l, r].iter().any(|s| matches!(s.as_ref(), Expr::Name(q) if q.len() == 2 && lc(&q[0]) == rkey && lc(&q[1]) == null_col)),
            _ => false,
        });
        if !in_eq || conj.iter().any(|c| mentions(c, &rkey)) {
            return unsupported();
        }
        // Unqualified names in the rest of WHERE could belong to the joined
        // table; keep them only when the catalog shows they cannot.
        let mut bare = Vec::new();
        for c in &conj {
            unqualified(c, &mut bare);
        }
        let ltab = self.schema.table(lname.last().unwrap());
        let rtab = self.schema.table(rname.last().unwrap());
        for name in &bare {
            let in_l = ltab.map(|t| t.column(name).is_some());
            let in_r = rtab.map(|t| t.column(name).is_some());
            let ok = match (in_l, in_r) {
                (Some(true), Some(false)) => true,
                (Some(false), Some(false)) => self.sym(name).is_some(),
                _ => false,
            };
            if !ok {
                return Err(format!(
                    "DELETE anti-join: `{name}` may name a column of the joined table"
                ));
            }
        }
        let sub = Select {
            items: vec![SelItem {
                expr: Expr::Num("1".into()),
                alias: None,
                assign: None,
            }],
            from: vec![From::Table {
                name: rname.clone(),
                alias: ralias.clone(),
                hints: Vec::new(),
            }],
            where_: Some(on.clone()),
            ..Select::default()
        };
        let mut pred = Expr::Unary("NOT".into(), Box::new(Expr::Exists(Box::new(sub))));
        for c in conj {
            pred = Expr::Bin("AND".into(), Box::new(pred), Box::new(c));
        }
        let b = self.bind_table(lname, lalias);
        let table = b.table.clone();
        self.scopes.last_mut().unwrap().push(b);
        let w = self.expr(&pred)?.0;
        Ok(n("delete", vec![a(table), w]))
    }

    /// Statement list of the unit, after the cross-statement lifts.
    pub fn lift(&mut self, stmts: Vec<Item>) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for mut it in stmts {
            // T-SQL: INSERT ...; SET @v = SCOPE_IDENTITY();
            let t = it.term.clone();
            if t.is("assign") && t.kid(1).is_some_and(|v| v.is("scope_identity")) {
                if let Some(prev) = out.last() {
                    if prev.term.is("insert1") && prev.err.is_none() {
                        let table = prev.term.kids()[0].atom().unwrap_or("").to_string();
                        let id = match self.schema.table(&table).and_then(|x| x.identity()) {
                            Some(c) => {
                                it.disc.push(format!(
                                    "{table}.{} is the identity column (catalog)",
                                    c.name
                                ));
                                n("col", vec![a(table.clone()), a(c.name.clone())])
                            }
                            None => n("identity", vec![a(table.clone())]),
                        };
                        let mut ins = out.pop().unwrap();
                        ins.term = n(
                            "insret",
                            vec![
                                ins.term,
                                n("items", vec![id]),
                                n("into", vec![t.kids()[0].clone()]),
                            ],
                        );
                        ins.absorb(it);
                        out.push(ins);
                        continue;
                    }
                }
            }
            out.push(it);
        }
        // Inline `assign(local, v); return(local)` when the local is fresh.
        let mut i = 0;
        while i + 1 < out.len() {
            let (x, y) = (&out[i].term, &out[i + 1].term);
            let local = x
                .kid(0)
                .filter(|s| {
                    x.is("assign")
                        && s.is("sym")
                        && s.kid(0)
                            .and_then(Term::atom)
                            .is_some_and(|v| v.starts_with('l'))
                })
                .cloned();
            if let Some(sym) = local {
                let returns_it = y.is("return") && y.kid(0) == Some(&sym);
                let uses = out
                    .iter()
                    .map(|s| s.term.count(&|t| t == &sym))
                    .sum::<usize>();
                // A statement that was not lowered may assign the local too.
                let first_def = !out[..i]
                    .iter()
                    .any(|s| s.err.is_some() || s.term.any(&|t| t == &sym));
                if returns_it && uses == 2 && first_def {
                    let v = x.kids()[1].map(&|t| {
                        (t.is("scalar") && t.kid(2).and_then(Term::atom) == Some("unchanged")).then(
                            || {
                                n(
                                    "scalar",
                                    vec![t.kids()[0].clone(), t.kids()[1].clone(), a("null")],
                                )
                            },
                        )
                    });
                    let ret = self.unit.returns;
                    let local_ty = sym
                        .kid(0)
                        .and_then(Term::atom)
                        .and_then(|l| l[1..].parse::<usize>().ok())
                        .and_then(|k| self.unit.locals.get(k - 1))
                        .and_then(|l| l.ty);
                    let ty = ret_ty_of(&v).or(local_ty);
                    let v = self.conv(&ret, (v, ty)).0;
                    let next = out.remove(i + 1);
                    out[i].term = n("return", vec![v]);
                    out[i].absorb(next);
                    continue;
                }
            }
            i += 1;
        }
        out
    }
}

/// One lowered statement with its source nodes and the facts it relies on.
#[derive(Debug, Clone)]
pub struct Item {
    pub term: Term,
    pub nodes: Vec<sqldiff_syntax::SyntaxNode>,
    pub asm: Vec<Assumption>,
    pub disc: Vec<String>,
    pub err: Option<String>,
    /// Tables the statement names: (key, schema as written when it is the
    /// default one: "", "dbo" or "public"; `None` for an explicit schema).
    pub names: Vec<(String, Option<String>)>,
}

impl Item {
    fn absorb(&mut self, other: Item) {
        self.nodes.extend(other.nodes);
        for x in other.asm {
            if !self.asm.iter().any(|y| y.id == x.id) {
                self.asm.push(x);
            }
        }
        for d in other.disc {
            if !self.disc.contains(&d) {
                self.disc.push(d);
            }
        }
        if self.err.is_none() {
            self.err = other.err;
        }
        for x in other.names {
            if !self.names.contains(&x) {
                self.names.push(x);
            }
        }
    }
}

/// Best-effort type of a lowered value, for dropping no-op conversions.
fn ret_ty_of(t: &Term) -> Option<Ty> {
    if t.is("conv") {
        // conv(ty(fam, len, scale), v)
        let ty = t.kid(0)?;
        let fam = match ty.kid(0)?.atom()? {
            "Int" => Fam::Int,
            "Exact" => Fam::Exact,
            "VarChar" => Fam::VarChar,
            "Char" => Fam::Char,
            "Timestamp" => Fam::Timestamp,
            "Date" => Fam::Date,
            _ => return None,
        };
        let num = |i: usize| ty.kid(i).and_then(Term::atom).and_then(|s| s.parse().ok());
        return Some(Ty {
            fam,
            len: num(1),
            scale: num(2),
        });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QCtx {
    /// A result set: column names matter.
    Set,
    /// One value per row; names do not matter.
    Scalar,
    /// EXISTS: the select list does not matter.
    Exists,
    /// SELECT INTO / assignment select.
    Into,
}

fn order_operands(op: &str, x: Term, y: Term) -> (Term, Term, String) {
    if matches!(op, "=" | "<>") {
        if x <= y {
            (x, y, op.to_string())
        } else {
            (y, x, op.to_string())
        }
    } else if x <= y {
        (x, y, op.to_string())
    } else {
        let flipped = match op {
            "<" => ">",
            ">" => "<",
            "<=" => ">=",
            ">=" => "<=",
            o => o,
        };
        (y, x, flipped.to_string())
    }
}

fn l_or_r_is_str(l: &Expr, r: &Expr) -> bool {
    matches!(l, Expr::Str(_)) || matches!(r, Expr::Str(_))
}

fn flatten_and(e: &Expr, out: &mut Vec<Expr>) {
    match e {
        Expr::Bin(op, l, r) if op == "AND" => {
            flatten_and(l, out);
            flatten_and(r, out);
        }
        _ => out.push(e.clone()),
    }
}

/// Single-part names in an expression (subqueries are refused by `mentions`).
fn unqualified(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Name(p) if p.len() == 1 => out.push(p[0].clone()),
        Expr::Bin(_, l, r) | Expr::Like(l, r, _, _) => {
            unqualified(l, out);
            unqualified(r, out);
        }
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            unqualified(x, out)
        }
        Expr::Call(c) => {
            c.args.iter().for_each(|x| unqualified(x, out));
            c.order.iter().for_each(|o| unqualified(&o.expr, out));
        }
        Expr::InList(x, l, _) => {
            unqualified(x, out);
            l.iter().for_each(|y| unqualified(y, out));
        }
        Expr::Between(x, y, z, _) => [x, y, z].iter().for_each(|v| unqualified(v, out)),
        Expr::Tuple(l) => l.iter().for_each(|y| unqualified(y, out)),
        _ => {}
    }
}

fn mentions(e: &Expr, alias: &str) -> bool {
    match e {
        Expr::Name(p) => p.len() > 1 && lc(&p[p.len() - 2]) == alias,
        Expr::Bin(_, l, r) => mentions(l, alias) || mentions(r, alias),
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            mentions(x, alias)
        }
        Expr::Call(c) => c.args.iter().any(|x| mentions(x, alias)),
        Expr::InList(x, l, _) => mentions(x, alias) || l.iter().any(|y| mentions(y, alias)),
        Expr::Between(x, y, z, _) => mentions(x, alias) || mentions(y, alias) || mentions(z, alias),
        Expr::Like(x, y, _, _) => mentions(x, alias) || mentions(y, alias),
        Expr::Tuple(l) => l.iter().any(|y| mentions(y, alias)),
        // Subqueries could reference the alias; treat as mentioning it.
        Expr::Exists(_) | Expr::Subquery(_) | Expr::InQuery(..) | Expr::Case(..) => true,
        _ => false,
    }
}
