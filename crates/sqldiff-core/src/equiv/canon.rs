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
use super::literal::{self, Target};
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
        "Values whose type is unknown (no catalog) are not character strings, real (single precision), T-SQL tinyint or decimals of 38 digits, so comparison and arithmetic mean the same on both sides",
    );
    pub const UNTYPED_CAST: (&str, &str) = (
        "ASM-UNTYPED-CAST",
        "Values of unknown type (no catalog) that are cast to a number or string are integers, or strings cast to a string type, which convert the same on both sides (a decimal or float to an integer, a float to text and text to a number do not)",
    );
    pub const UNTYPED_STORE: (&str, &str) = (
        "ASM-UNTYPED-STORE",
        "Values stored in columns or variables of unknown type (no catalog) convert the same on both sides: no fraction goes into an integer, no number into a bit or boolean, no string longer than the column, and no text into a number",
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
    pub const PAGE_UNORDERED: (&str, &str) = (
        "ASM-PAGE-UNORDERED",
        "A row limit or offset without ORDER BY: which rows each side keeps is up to its engine, so any such rows will do",
    );
    pub const PAGE_COUNT_NOT_NULL: (&str, &str) = (
        "ASM-PAGE-COUNT-NOT-NULL",
        "The row count and offset of TOP / LIMIT / FETCH / OFFSET are never NULL: T-SQL raises an error for NULL (Msg 1014), PostgreSQL reads LIMIT NULL as no limit and OFFSET NULL as 0, and Db2 for i was not checked",
    );
    pub const RAISERROR_TEXT: (&str, &str) = (
        "ASM-RAISERROR-TEXT",
        "The RAISERROR message is 1 to 2,047 characters long: T-SQL cuts a longer one to 2,044 characters and '...', and sends an empty one as a single space, where PostgreSQL keeps the text as it is",
    );
    pub const RAISERROR_FORMAT: (&str, &str) = (
        "ASM-RAISERROR-FORMAT",
        "The variable RAISERROR takes as its message holds no % sign: T-SQL reads it as a format, so % starts a substitution",
    );
    pub const RESULT_TYPES: (&str, &str) = (
        "ASM-RESULT-TYPES",
        "Declared result column types hold the selected values without conversion",
    );
    pub const VALUES_ORDER: (&str, &str) = (
        "ASM-VALUES-ROW-ORDER",
        "A multi-row VALUES list gets the values the table generates (identity, sequence defaults) in the order its rows are written on both sides",
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
        "The Db2 for i routine has no SET OPTION USRPRF, so its authority comes from USRPRF *NAMING: assumed to be the caller's (system naming at creation; under SQL naming it runs with the owner's authority; fact V25, not checked on a real system)",
    );
    pub const DEFAULT_SCHEMA: (&str, &str) = (
        "ASM-DEFAULT-SCHEMA",
        "Unqualified names, T-SQL dbo and PostgreSQL public resolve to the same schema on both sides (the default schema, search_path or Db2 for i current schema in effect)",
    );
    /// Same id as [`TSQL_CI`]: T-SQL has one case-insensitive collation
    /// assumption, stated for names here and for string comparisons there.
    pub const TSQL_CI_NAMES: (&str, &str) = (
        "ASM-TSQL-CI-COLLATION",
        "Identifiers and variables compare case-insensitively (case-insensitive catalog collation)",
    );
    /// Decision 1: across dialects, result column names that differ only in
    /// letter case are equal when the client matches names that way.
    pub const RESULT_NAMES_CI: (&str, &str) = (
        "ASM-RESULT-NAMES-CI",
        "Clients read result columns by name case-insensitively (PostgreSQL returns an unquoted alias such as OrderId as orderid; T-SQL returns it as written)",
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

/// How a value is converted to a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// CAST / CONVERT / `::`.
    Cast,
    /// Stored in a column.
    Store,
    /// Assigned to a variable, parameter or return value.
    Var,
}

impl How {
    fn name(self) -> &'static str {
        match self {
            How::Cast => "cast",
            How::Store => "store",
            How::Var => "assign",
        }
    }
}

/// Whether a conversion means the same in every dialect.
enum Rule {
    Same,
    /// T-SQL drops the fraction first.
    Trunc,
    /// Dialect-specific (or not checked).
    Dialect,
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
    /// Bit and boolean columns of derived tables (`sub:alias`), by name: a
    /// bit read through a derived table still meets numbers as a bit.
    sub_bits: HashMap<String, Vec<(String, Ty)>>,
}

/// Lower-case hex digits of a byte string written in hex, `None` unless
/// every character is a hex digit and they make whole bytes.
pub(super) fn hex_bytes(s: &str) -> Option<String> {
    (s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// The name a client receives for a result column, from the parser's
/// spelling (`ast::quoted_name` marks a quoted name whose case matters with
/// a leading `"`): such a name as it is; otherwise as written in T-SQL,
/// folded to lower case in PostgreSQL and to upper case in Db2 for i.
fn returned_name(d: Dialect, s: &str) -> String {
    match s.strip_prefix('"') {
        Some(q) => q.to_string(),
        None => match d {
            Dialect::Tsql => s.to_string(),
            Dialect::Postgres => s.to_lowercase(),
            Dialect::Db2i => s.to_uppercase(),
        },
    }
}

/// A result column's name as decision 1 observes it: `(colname DIALECT
/// KIND NAME)`, KIND `alias`, `col` (a column reference without alias),
/// `decl` (a declared output column) or `expr` (unnamed, NAME empty).
/// `compare` checks names exactly within a dialect and up to letter case
/// across dialects, under `ASM-RESULT-NAMES-CI`.
fn colname(d: Dialect, kind: &str, name: &str) -> Term {
    n("colname", vec![a(d.id()), a(kind), a(name)])
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
            sub_bits: HashMap::new(),
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
        if let Some((_, ty)) = self
            .sub_bits
            .get(table)
            .and_then(|cs| cs.iter().find(|(c, _)| c == col))
        {
            return (Some(*ty), None);
        }
        match self
            .schema
            .table(Self::base_table(table))
            .and_then(|t| t.column(col))
        {
            Some(c) => (Some(c.ty), Some(c.not_null)),
            None => (None, None),
        }
    }

    /// A statement writes `col`: refused for a column the database computes
    /// (period, computed, GENERATED ALWAYS), whose explicit values one side
    /// rejects or replaces depending on settings and triggers the statement
    /// does not show.
    fn written(&self, table: &str, col: &str) -> R<()> {
        let generated = self
            .schema
            .table(Self::base_table(table))
            .and_then(|t| t.column(col))
            .is_some_and(|c| c.generated);
        if generated {
            return Err(format!("writes the generated column {col}"));
        }
        Ok(())
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
            } else if let Some(ids) = &self.unit.local_ids {
                ids[i].clone()
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
    fn fits(&self, dst: &Ty, src: &Ty) -> bool {
        if dst.fam != src.fam {
            // PostgreSQL drops the trailing blanks of a CHAR value converted
            // to varchar or text ('a  ' becomes 'a'); SQL Server keeps them.
            return dst.is_string()
                && src.is_string()
                && dst.fam == Fam::VarChar
                && dst.len.is_none()
                && !(self.d == Dialect::Postgres && src.fam == Fam::Char);
        }
        match dst.fam {
            // A bounded binary target cuts a longer value off in T-SQL and
            // keeps or rejects it elsewhere.
            Fam::Int | Fam::VarChar | Fam::Binary => match (dst.len, src.len) {
                (None, _) => true,
                (Some(d), Some(s)) => d >= s,
                (Some(_), None) => false,
            },
            Fam::Char => dst.len == src.len,
            Fam::Exact => {
                dst.len.is_none()
                    || (src.len.is_some() && dst.len >= src.len && dst.scale == src.scale)
            }
            Fam::Float => dst.len >= src.len,
            // Fewer fractional digits (or other ticks) round the value. A time
            // of unknown precision (SYSDATETIME(), now()) is the time source's
            // question, not this one.
            Fam::Timestamp | Fam::TimestampTz | Fam::Time => {
                src.len.is_none()
                    || (dst.scale == src.scale && dst.len.zip(src.len).is_some_and(|(d, s)| d >= s))
            }
            // The engine does not know which type an `Other` is (BINARY(n),
            // IMAGE, MONEY, an alias type, …): the store may pad, cut off,
            // round or fail.
            Fam::Other => false,
            Fam::Date | Fam::Interval | Fam::Bool => true,
        }
    }

    /// A value stored in a table column. A string literal stored in a
    /// non-string column stands for the value it converts to, as in a cast:
    /// SQL Server converts it to the column's type and PostgreSQL resolves
    /// the untyped literal to it, both keeping the column's precision and
    /// scale. One whose conversion may round, fail or depend on settings
    /// goes through `conv` and is not compared across dialects.
    fn conv_col(&mut self, table: &str, col: &str, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        let (cty, _) = self.col_ty(table, col);
        if let (Some(d), Some(s)) = (cty, v.0.is("str").then(|| v.0.kid(0)).flatten()) {
            let target = self
                .schema
                .table(Self::base_table(table))
                .and_then(|t| t.column(col))
                .and_then(|c| c.target)
                .filter(|t| !matches!(t, Target::Bit | Target::Bool));
            if let Some(target) = target.filter(|_| !d.is_string()) {
                if let Some((t, asm)) = s
                    .atom()
                    .and_then(|s| literal::convert(s, target, self.d, true))
                {
                    if let Some(x) = asm {
                        self.assume(x);
                    }
                    return (t, Some(d));
                }
            }
        }
        self.conv(&cty, v)
    }

    /// Implicit conversion of a value to a declared type: dropped when it is
    /// provably a no-op.
    fn conv(&mut self, dst: &Option<Ty>, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        self.conv_in(dst, v, false)
    }

    /// `conv` for a PL/pgSQL variable, parameter or return value, which
    /// PostgreSQL converts through text where no assignment cast exists.
    fn conv_var(&mut self, dst: &Option<Ty>, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        self.conv_in(dst, v, true)
    }

    fn conv_in(
        &mut self,
        dst: &Option<Ty>,
        v: (Term, Option<Ty>),
        var: bool,
    ) -> (Term, Option<Ty>) {
        let Some(d) = dst else {
            if !matches!(v.0.head(), "null") {
                self.assume(asm::UNTYPED_STORE);
            }
            return v;
        };
        if let Some(b) = self.bool_conv(d, &v, var) {
            return b;
        }
        // An integer literal in the range of an integer type is stored as is.
        if let (Fam::Int, Some(bits), true) = (d.fam, d.len, v.0.is("num")) {
            let lit =
                v.0.kid(0)
                    .and_then(Term::atom)
                    .and_then(|x| x.parse::<i128>().ok());
            // T-SQL tinyint holds 0 to 255.
            let range = match bits {
                8 => 0..256,
                _ => -(1i128 << (bits - 1).min(127))..(1i128 << (bits - 1).min(127)),
            };
            if bits > 1 && bits <= 64 && lit.is_some_and(|x| range.contains(&x)) {
                return (v.0, Some(*d));
            }
        }
        match &v.1 {
            Some(s) if self.fits(d, s) => v,
            None if matches!(v.0.head(), "null") => v,
            _ => self.convert(*d, v, if var { How::Var } else { How::Store }),
        }
    }

    // ---- bit and boolean ------------------------------------------------
    //
    // A T-SQL bit holds 0 or 1 and a PostgreSQL boolean false or true, so
    // the terms write both as the bit values: a boolean literal compared
    // with, stored in or combined with a boolean becomes `num 1` / `num 0`.
    // PostgreSQL has no operator, cast or assignment between boolean and the
    // numbers; where it would need one, the term is `type-error` (the
    // statement fails there). Checked on SQL Server 2022 and PostgreSQL 18
    // (eval/bit-boolean).

    /// T-SQL bit.
    fn is_bit(&self, t: &Option<Ty>) -> bool {
        self.d == Dialect::Tsql && t.is_some_and(|t| t.fam == Fam::Int && t.len == Some(1))
    }

    /// PostgreSQL boolean.
    fn is_bool(&self, t: &Option<Ty>) -> bool {
        self.d == Dialect::Postgres && t.is_some_and(|t| t.fam == Fam::Bool)
    }

    /// TRUE / FALSE as the bit value 1 / 0.
    fn bit_value(t: Term) -> Term {
        match (t.head(), t.kid(0).and_then(Term::atom)) {
            ("bool", Some("TRUE")) => n("num", vec![a("1")]),
            ("bool", Some("FALSE")) => n("num", vec![a("0")]),
            _ => t,
        }
    }

    /// A PostgreSQL statement that fails on a boolean meeting a number.
    fn type_error(&self, kind: &str, kids: Vec<Term>) -> Term {
        let mut v = vec![a("postgres"), a(kind)];
        v.extend(kids);
        n("type-error", v)
    }

    /// Whether a numeric literal term is zero, nonzero, or not a literal.
    fn literal_zero(t: &Term) -> Option<bool> {
        if !t.is("num") {
            return None;
        }
        let s = t.kid(0)?.atom()?;
        let digits = s.trim_start_matches(['-', '+']);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        Some(digits.bytes().all(|b| b == b'0' || b == b'.'))
    }

    /// `x <> 0`: a number as a bit or boolean (NULL stays NULL).
    fn nonzero(t: Term) -> Term {
        // Of a CASE, the branch taken: a literal result is 0 or 1.
        if t.is("case") {
            let branch = |v: &Term| match Self::literal_zero(v) {
                Some(z) => n("num", vec![a(if z { "0" } else { "1" })]),
                None if v.atom() == Some("null") => v.clone(),
                None => Self::nonzero(v.clone()),
            };
            let ks = t.kids();
            let mut kids = vec![ks[0].clone()];
            for k in &ks[1..] {
                kids.push(match (k.is("when"), k.kid(0), k.kid(1)) {
                    (true, Some(c), Some(v)) => n("when", vec![c.clone(), branch(v)]),
                    _ => branch(k),
                });
            }
            return n("case", kids);
        }
        let (x, y, op) = order_operands("<>", t, n("num", vec![a("0")]));
        n("cmp", vec![a(op), x, y])
    }

    /// Storing a value in a bit or boolean, when one of them is involved.
    fn bool_conv(
        &mut self,
        d: &Ty,
        v: &(Term, Option<Ty>),
        var: bool,
    ) -> Option<(Term, Option<Ty>)> {
        let dst = Some(*d);
        if self.is_bit(&dst) {
            if self.is_bit(&v.1) {
                return None;
            }
            // T-SQL stores 0 as 0 and any other number as 1.
            if let Some(zero) = Self::literal_zero(&v.0) {
                return Some((n("num", vec![a(if zero { "0" } else { "1" })]), dst));
            }
            if v.0.is("str") {
                let s = v.0.kid(0)?.atom()?;
                let (t, _) = literal::convert(s, Target::Bit, self.d, false)?;
                return Some((t, dst));
            }
            if v.1.is_some_and(|t| t.is_numeric()) {
                return Some((Self::nonzero(v.0.clone()), dst));
            }
            return None;
        }
        if !self.is_bool(&dst) {
            return None;
        }
        if self.is_bool(&v.1) {
            return Some((Self::bit_value(v.0.clone()), dst));
        }
        if v.0.is("str") {
            let s = v.0.kid(0)?.atom()?;
            let (t, _) = literal::convert(s, Target::Bool, self.d, false)?;
            return Some((Self::bit_value(t), dst));
        }
        if v.1.is_some_and(|t| t.is_numeric()) {
            if !var {
                // column "c" is of type boolean but expression is of type integer
                return Some((self.type_error("assign", vec![v.0.clone()]), dst));
            }
            // PL/pgSQL converts through text: '0' and '1' are read, any other
            // number fails.
            return Some(match Self::literal_zero(&v.0) {
                Some(zero) if matches!(v.0.kid(0).and_then(Term::atom), Some("0" | "1")) => {
                    (n("num", vec![a(if zero { "0" } else { "1" })]), dst)
                }
                _ => (n("dfn", vec![self.dtag(), a("bool-io"), v.0.clone()]), dst),
            });
        }
        None
    }

    /// A condition: in PostgreSQL a bare boolean column or variable is the
    /// same test as T-SQL's `bit = 1`, and `NOT` of one as `bit = 0`.
    fn cond(&mut self, e: &Expr) -> R<(Term, Option<Ty>)> {
        let (t, ty) = self.expr(e)?;
        Ok((self.bare_bool(t, ty), ty))
    }

    fn bare_bool(&self, t: Term, ty: Option<Ty>) -> Term {
        if self.is_bool(&ty) && matches!(t.head(), "col" | "sym") {
            let (x, y, op) = order_operands("=", t, n("num", vec![a("1")]));
            return n("cmp", vec![a(op), x, y]);
        }
        t
    }

    /// PostgreSQL: values of one CASE or COALESCE must share a type; a
    /// boolean among numbers fails, TRUE / FALSE among booleans are bit values.
    fn bool_mix(&self, vals: &mut [(Term, Option<Ty>)]) -> Option<Term> {
        if !vals.iter().any(|v| self.is_bool(&v.1)) {
            return None;
        }
        if vals.iter().any(|v| v.1.is_some_and(|t| t.is_numeric())) {
            return Some(self.type_error("mix", vals.iter().map(|v| v.0.clone()).collect()));
        }
        for v in vals.iter_mut() {
            v.0 = Self::bit_value(v.0.clone());
        }
        None
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
            Expr::Num(s)
                if self.d == Dialect::Tsql
                    && s.len() >= 2
                    && s[..2].eq_ignore_ascii_case("0x")
                    && hex_bytes(&s[2..]).is_some() =>
            {
                (
                    n("bytes", vec![a(hex_bytes(&s[2..]).unwrap_or_default())]),
                    None,
                )
            }
            Expr::Num(s) => self.num_lit(s),
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
                    if self.is_bool(&ty) && matches!(t.head(), "col" | "sym") {
                        let (x, y, op) = order_operands("=", t, n("num", vec![a("0")]));
                        (n("cmp", vec![a(op), x, y]), Some(Ty::of(Fam::Bool)))
                    } else {
                        (n("not", vec![self.bare_bool(t, ty)]), None)
                    }
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
                let mut item_tys = Vec::new();
                let (mut string, mut known) = (ty.is_some_and(|t| t.is_string()), ty.is_some());
                // String literals convert to the type of a non-string operand, as in `=`.
                let target = ty
                    .is_some_and(|t| !t.is_string())
                    .then(|| self.target_of(&t, ty));
                for l in list {
                    if let (Expr::Str(s), Some(target)) = (l, target) {
                        items.push(self.lit_as(s, target, false));
                        continue;
                    }
                    let (it, ity) = self.expr(l)?;
                    item_tys.push(ity);
                    string |= ity.is_some_and(|t| t.is_string());
                    known |= ity.is_some();
                    items.push(it);
                }
                if self.is_bool(&ty) {
                    if item_tys.iter().any(|t| t.is_some_and(|t| t.is_numeric())) {
                        let mut kids = vec![t, a(neg.to_string())];
                        kids.extend(items);
                        return Ok((self.type_error("in", kids), None));
                    }
                    items = items.into_iter().map(Self::bit_value).collect();
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
                // String literal bounds convert to the type of a non-string operand.
                let target = ty
                    .is_some_and(|t| !t.is_string())
                    .then(|| self.target_of(&t, ty));
                let bound = |e: &Expr, c: &mut Self| -> R<(Term, Option<Ty>)> {
                    Ok(match (e, target) {
                        (Expr::Str(s), Some(target)) => (c.lit_as(s, target, true), ty),
                        _ => c.expr(e)?,
                    })
                };
                let (lo, lty) = bound(lo, self)?;
                let (hi, hty) = bound(hi, self)?;
                // A boolean between numbers (or a number between booleans).
                let tys = [ty, lty, hty];
                let numeric = |t: &Option<Ty>| t.is_some_and(|t| t.is_numeric());
                if tys.iter().any(|t| self.is_bool(t)) && tys.iter().any(numeric) {
                    return Ok((self.type_error("cmp", vec![t, lo, hi]), None));
                }
                let (t, lo, hi) = match tys.iter().any(|t| self.is_bool(t)) {
                    true => (Self::bit_value(t), Self::bit_value(lo), Self::bit_value(hi)),
                    false => (t, lo, hi),
                };
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
            // PostgreSQL `'\xAB01'::bytea` (hex input format): the bytes T-SQL
            // writes 0xAB01.
            Expr::Cast(x, ty)
                if self.d == Dialect::Postgres
                    && ty.name.eq_ignore_ascii_case("BYTEA")
                    && matches!(x.as_ref(), Expr::Str(s) if hex_bytes(s.strip_prefix("\\x").unwrap_or("-")).is_some()) =>
            {
                let Expr::Str(s) = x.as_ref() else {
                    unreachable!()
                };
                let hex = hex_bytes(&s[2..]).unwrap_or_default();
                (n("bytes", vec![a(hex)]), None)
            }
            Expr::Cast(x, ty) => {
                let dst = Ty::from_type(ty, self.d, true);
                // A string literal cast to a type whose conversion is modelled
                // stands for the value it converts to ('12' as int is 12 on
                // every engine); one that may round, fail or depend on
                // settings is not compared.
                if let (Expr::Str(s), Some(target)) = (x.as_ref(), Target::of_type(ty, self.d)) {
                    match literal::convert(s, target, self.d, true) {
                        Some((t, asm)) => {
                            if let Some(x) = asm {
                                self.assume(x);
                            }
                            return Ok((t, Some(dst)));
                        }
                        None => {
                            return Ok((
                                n(
                                    "lit-conv",
                                    vec![self.dtag(), a(target.name()), a(s.clone())],
                                ),
                                Some(dst),
                            ))
                        }
                    }
                }
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
                let mut conds = Vec::new();
                let mut vals = Vec::new();
                for (w, t) in whens {
                    conds.push(if op.is_some() {
                        self.expr(w)?.0
                    } else {
                        self.cond(w)?.0
                    });
                    let (t, tty) = self.expr(t)?;
                    ty = ty.or(tty);
                    vals.push((t, tty));
                }
                if let Some(e) = els {
                    vals.push(self.expr(e)?);
                }
                if let Some(err) = self.bool_mix(&mut vals) {
                    return Ok((err, None));
                }
                let mut vals = vals.into_iter().map(|v| v.0);
                for w in conds {
                    kids.push(n("when", vec![w, vals.next().unwrap()]));
                }
                kids.push(match els {
                    Some(_) => vals.next().unwrap(),
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
        let to = Some(dst);
        if self.is_bool(&v.1) {
            // PostgreSQL casts boolean to integer (0 / 1, the bit value) and
            // to text ('true' / 'false'), and to no other type.
            return match dst.fam {
                Fam::Bool => (Self::bit_value(v.0), to),
                // 0 or 1: typed as a bit value, so its text is one digit.
                Fam::Int if dst.len == Some(32) => (
                    Self::bit_value(v.0),
                    Some(Ty {
                        len: Some(1),
                        ..dst
                    }),
                ),
                _ if dst.is_string() => (n("booltext", vec![Self::ty_term(&to), v.0]), to),
                _ => (self.type_error("cast", vec![Self::ty_term(&to), v.0]), to),
            };
        }
        if self.is_bool(&to) {
            // Only integer casts to boolean: nonzero is true.
            match v.1 {
                Some(t) if t.fam == Fam::Int && t.len == Some(32) => {
                    return (Self::nonzero(v.0), to)
                }
                Some(t) if t.is_numeric() => {
                    return (self.type_error("cast", vec![Self::ty_term(&to), v.0]), to)
                }
                _ => {}
            }
        }
        if self.is_bit(&to) && !self.is_bit(&v.1) {
            // T-SQL: 0 is 0, any other number 1.
            if let Some(zero) = Self::literal_zero(&v.0) {
                return (n("num", vec![a(if zero { "0" } else { "1" })]), to);
            }
            if v.1.is_some_and(|t| t.is_numeric()) {
                return (Self::nonzero(v.0), to);
            }
        }
        // A bit is 0 or 1 in int, as boolean::integer is. Only to int: in a
        // wider type the arithmetic that follows overflows elsewhere.
        if self.is_bit(&v.1) && dst.fam == Fam::Int && dst.len == Some(32) {
            return (v.0, to);
        }
        // Integer to string: every dialect prints the plain decimal form.
        if dst.is_string() && dst.fam == Fam::VarChar {
            if let Some(src) = v.1 {
                // The longest text of the type: -32768 for smallint.
                let width = match src.len {
                    Some(1) => 1,
                    Some(2..=8) => 3,
                    Some(9..=16) => 6,
                    Some(17..=32) => 11,
                    _ => 20,
                };
                if src.fam == Fam::Int && dst.len.map_or(true, |l| l >= width) {
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
        // A decimal of known precision and scale to a string long enough:
        // every dialect prints the digits with exactly `scale` decimals.
        if let (Fam::VarChar, Some(src)) = (dst.fam, v.1) {
            if let (Fam::Exact, Some(p), Some(s)) = (src.fam, src.len, src.scale) {
                let width = 1 + p.saturating_sub(s).max(1) + if s > 0 { s + 1 } else { 0 };
                if self.d != Dialect::Db2i && dst.len.map_or(true, |l| l >= width) {
                    return (
                        n("numstr", vec![a(s.to_string()), v.0]),
                        Some(Ty::of(Fam::VarChar)),
                    );
                }
            }
        }
        if let Some(src) = v.1 {
            if src == dst && src.fam != Fam::Other {
                return v;
            }
        }
        self.convert(dst, v, How::Cast)
    }

    // ---- casts and implicit conversions ----------------------------------
    //
    // A conversion keeps one term in every dialect only where SQL Server 2022
    // and PostgreSQL 18 give the same value or the same error (eval/casts).
    // T-SQL truncates a decimal or float converted to an integer, so that
    // conversion is written with `trunc`, which PostgreSQL's `trunc(x)` also
    // lowers to. Any other conversion keeps the dialect and the source type
    // (`dcast`), so it equals only itself. Db2 for i conversions are kept only
    // between integers, to wider decimals and for strings that fit; the open
    // questions are probes V31 (probes/db2i).

    /// The type of a numeric literal as each dialect reads it: an integer in
    /// int range is int; a larger one is decimal in T-SQL and bigint (then
    /// numeric) elsewhere; one with a decimal point is decimal(p, s); one with
    /// an exponent is float in T-SQL and Db2 for i but numeric in PostgreSQL;
    /// `0x1F` is varbinary in T-SQL and an integer in PostgreSQL.
    fn num_lit(&self, s: &str) -> (Term, Option<Ty>) {
        let d = self.d;
        let (neg, body) = match s.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, s),
        };
        let int = |bits: u32| Ty {
            fam: Fam::Int,
            len: Some(bits),
            scale: None,
        };
        let exact = |p: Option<u32>, s: u32| Ty {
            fam: Fam::Exact,
            len: p,
            scale: Some(s),
        };
        if let Some(hex) = body.strip_prefix("0x") {
            let value = (d == Dialect::Postgres)
                .then(|| i128::from_str_radix(&hex.replace('_', ""), 16).ok())
                .flatten();
            return match value {
                Some(v) => self.num_lit(&format!("{}{v}", if neg { "-" } else { "" })),
                None => (
                    n("hexlit", vec![self.dtag(), a(s)]),
                    Some(Ty::of(Fam::Other)),
                ),
            };
        }
        let text = if d == Dialect::Postgres {
            s.replace('_', "")
        } else {
            s.to_string()
        };
        let body = text.trim_start_matches('-');
        if body.contains('e') {
            if d == Dialect::Postgres {
                return (n("num", vec![a(text)]), Some(Ty::of(Fam::Exact)));
            }
            let ty = Ty {
                fam: Fam::Float,
                len: Some(53),
                scale: None,
            };
            return (n("flt", vec![a(text)]), Some(ty));
        }
        let ty = if let Some((i, f)) = body.split_once('.') {
            let digits = i.trim_start_matches('0').len() as u32 + f.len() as u32;
            exact(Some(digits.max(f.len() as u32).max(1)), f.len() as u32)
        } else {
            match text.parse::<i128>() {
                Ok(v) if i32::try_from(v).is_ok() => int(32),
                Ok(v) if d != Dialect::Tsql && i64::try_from(v).is_ok() => int(64),
                _ => exact(Some(body.trim_start_matches('0').len().max(1) as u32), 0),
            }
        };
        (n("num", vec![a(text)]), Some(ty))
    }

    /// `v` converted to `dst`: by CAST, by storing it in a column, or by
    /// assigning it to a variable, parameter or return value.
    fn convert(&mut self, dst: Ty, v: (Term, Option<Ty>), how: How) -> (Term, Option<Ty>) {
        let to = Some(dst);
        let head = if how == How::Cast { "cast" } else { "conv" };
        // A decimal literal without a fraction (2.0) converts to an integer
        // the same everywhere.
        let whole = v.0.is("num")
            && v.0.kid(0).and_then(Term::atom).is_some_and(|x| {
                x.split_once('.')
                    .is_some_and(|(_, f)| !f.contains('e') && f.bytes().all(|b| b == b'0'))
            });
        // Without a catalog most values have no type: storing or assigning
        // one, or casting it to a number or string, is assumed to convert
        // alike.
        if matches!(v.0.head(), "null") {
            return (n(head, vec![Self::ty_term(&to), v.0]), to);
        }
        if v.1.is_none() {
            let castable = matches!(
                dst.fam,
                Fam::Int | Fam::Exact | Fam::Float | Fam::VarChar | Fam::Char
            );
            if how != How::Cast || castable {
                self.assume(if how == How::Cast {
                    asm::UNTYPED_CAST
                } else {
                    asm::UNTYPED_STORE
                });
                return (n(head, vec![Self::ty_term(&to), v.0]), to);
            }
        }
        let rule = match self.conv_rule(v.1, dst, how) {
            Rule::Trunc | Rule::Dialect
                if whole && dst.fam == Fam::Int && self.d != Dialect::Db2i =>
            {
                Rule::Same
            }
            r => r,
        };
        match rule {
            Rule::Same => (n(head, vec![Self::ty_term(&to), v.0]), to),
            Rule::Trunc => (
                n(
                    head,
                    vec![Self::ty_term(&to), n("fn", vec![a("trunc"), v.0])],
                ),
                to,
            ),
            Rule::Dialect => (
                n(
                    "dcast",
                    vec![
                        self.dtag(),
                        a(how.name()),
                        Self::ty_term(&to),
                        Self::ty_term(&v.1),
                        v.0,
                    ],
                ),
                to,
            ),
        }
    }

    /// Whether converting a `src` value to `dst` gives the same result in
    /// every dialect (eval/casts has the runs behind each line).
    fn conv_rule(&self, src: Option<Ty>, dst: Ty, how: How) -> Rule {
        use Fam::*;
        let Some(src) = src else { return Rule::Dialect };
        // A binary string kept whole: the target holds any length, or one at
        // least as long as the value can be. T-SQL cuts a longer value off
        // silently where the others keep it or fail.
        if src.fam == Binary && dst.fam == Binary {
            let fits = match (dst.len, src.len) {
                (None, _) => true,
                (Some(d), Some(s)) => s <= d,
                (Some(_), None) => false,
            };
            return if fits { Rule::Same } else { Rule::Dialect };
        }
        if matches!(src.fam, Other | Binary) || matches!(dst.fam, Other | Binary) {
            return Rule::Dialect;
        }
        // A value without a fraction: an integer, a decimal of scale 0, or
        // the result of trunc().
        let integral = src.fam == Int || (matches!(src.fam, Exact | Float) && src.scale == Some(0));
        if self.d == Dialect::Db2i {
            return match (src.fam, dst.fam) {
                (Int, Int | Exact) => Rule::Same,
                // A string no longer than the column is stored as it is
                // (blank-padded in CHAR).
                (Char | VarChar, Char | VarChar)
                    if how == How::Store && src.len.zip(dst.len).is_some_and(|(s, d)| s <= d) =>
                {
                    Rule::Same
                }
                (Exact, Exact)
                    if dst.len.is_none()
                        || (src.len.is_some()
                            && src.scale.is_some()
                            && dst.scale >= src.scale
                            && dst.len.zip(src.len).is_some_and(|(d, s)| {
                                d - dst.scale.unwrap_or(0) >= s - src.scale.unwrap_or(0)
                            })) =>
                {
                    Rule::Same
                }
                _ => Rule::Dialect,
            };
        }
        match (src.fam, dst.fam) {
            // Out of range is an error on both sides.
            (Int, Int | Exact | Float) => Rule::Same,
            // Both round half away from zero (1.555 → 1.56, -2.5 → -3).
            (Exact, Exact) => Rule::Same,
            (Exact | Float, Int) if integral => Rule::Same,
            // T-SQL truncates (1.5 → 1, -1.5 → -1); PostgreSQL rounds,
            // numeric half away from zero and float half to even.
            (Exact | Float, Int) if self.d == Dialect::Tsql => Rule::Trunc,
            (Float, Float) => Rule::Same,
            // Explicit casts truncate silently on both sides, and storing too
            // long a string fails on both; a T-SQL variable truncates where a
            // PL/pgSQL one fails. CHAR pads on both; what differs is how
            // padded values compare and concatenate, which those terms keep.
            // PostgreSQL drops a CHAR value's trailing blanks when it becomes
            // varchar or text; SQL Server keeps them.
            (Char, VarChar) if self.d == Dialect::Postgres => Rule::Dialect,
            (Char | VarChar, Char | VarChar) if how != How::Var => Rule::Same,
            (Timestamp | Date, Date) | (Date, Timestamp) => Rule::Same,
            // Fewer fractional digits round differently: PostgreSQL rounds a
            // half away from 2000-01-01 and to 24:00:00.
            (Timestamp, Timestamp | Time) | (Time, Time)
                if src.scale == dst.scale
                    && src
                        .len
                        .zip(dst.len)
                        .map_or(src.len.is_none(), |(s, d)| d >= s) =>
            {
                Rule::Same
            }
            _ => Rule::Dialect,
        }
    }

    fn call(&mut self, c: &Call) -> R<(Term, Option<Ty>)> {
        let name = lc(c.name.last().map(String::as_str).unwrap_or(""));
        // A qualified name is a user function, even `dbo.max`: T-SQL calls a
        // built-in unqualified only.
        if c.name.len() > 1 {
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
            // CONVERT(type, x) without a style is CAST(x AS type).
            (Dialect::Tsql, "convert") if argc == 2 && plain => {
                let ty = match &c.args[0] {
                    Expr::Name(p) if p.len() == 1 => Some(super::ast::Type {
                        name: p[0].to_uppercase(),
                        args: Vec::new(),
                    }),
                    Expr::Call(t) if t.name.len() == 1 && t.args.iter().all(type_arg) => {
                        Some(super::ast::Type {
                            name: t.name[0].to_uppercase(),
                            args: t
                                .args
                                .iter()
                                .map(|x| match x {
                                    Expr::Num(v) => v.clone(),
                                    _ => "MAX".to_string(),
                                })
                                .collect(),
                        })
                    }
                    _ => None,
                };
                if let Some(ty) = ty {
                    return self.expr(&Expr::Cast(Box::new(c.args[1].clone()), ty));
                }
            }
            // IIF is shorthand for this CASE; SQL Server rewrites it so.
            (Dialect::Tsql, "iif") if argc == 3 && plain => {
                let [c0, x, y] = [0, 1, 2].map(|k| c.args[k].clone());
                return self.expr(&Expr::Case(None, vec![(c0, x)], Some(Box::new(y))));
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
                let mut args = args;
                if let Some(err) = self.bool_mix(&mut args) {
                    return Ok((err, None));
                }
                let ty = args.iter().find_map(|x| x.1);
                portable("coalesce", args, ty)
            }
            (Dialect::Tsql, "isnull", 2) => {
                // ISNULL takes the type of its first argument; it equals COALESCE
                // when the replacement is stored in that type unchanged.
                if self.is_bit(&first_ty) {
                    // The replacement is converted to bit: 0 stays 0, any other number is 1.
                    if let Some((t, _)) = self.bool_conv(&first_ty.unwrap(), &args[1], false) {
                        let mut args = args;
                        args[1] = (t, first_ty);
                        return Ok(portable("coalesce", args, first_ty));
                    }
                }
                let (b, bty) = (&args[1].0, args[1].1);
                let fits = matches!(b.head(), "null")
                    || (b.is("str")
                        && b.kid(0).and_then(Term::atom) == Some("")
                        && first_ty.is_some_and(|t| t.is_string()))
                    || matches!((first_ty, bty), (Some(x), Some(y)) if self.fits(&x, &y));
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
            // trunc drops the fraction. Of anything but numeric it is
            // trunc(double precision), which loses digits past 2^53.
            (Dialect::Postgres, "trunc", 1) if first_ty.is_some_and(|t| t.is_numeric()) => {
                let t = first_ty.unwrap();
                let ty = match t.fam {
                    Fam::Exact => Ty {
                        scale: Some(0),
                        ..t
                    },
                    _ => Ty {
                        fam: Fam::Float,
                        len: Some(53),
                        scale: Some(0),
                    },
                };
                portable("trunc", args, Some(ty))
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
                self.cast(dst, v)
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
                let (t, _) = self.cond(side)?;
                if t.is(&op.to_lowercase()) {
                    parts.extend(t.kids().iter().cloned());
                } else {
                    parts.push(t);
                }
            }
            parts.sort();
            return Ok((n(&op.to_lowercase(), parts), Some(Ty::of(Fam::Bool))));
        }
        let (mut lt, mut lty) = self.expr(l)?;
        let (mut rt, mut rty) = self.expr(r)?;
        let cmp_op = matches!(op, "=" | "<>" | "<" | ">" | "<=" | ">=");
        if cmp_op {
            // A string literal against a value of another type converts to
            // that type: it compares as a value, not under a collation.
            let ordered = !matches!(op, "=" | "<>");
            let non_string = |t: &Option<Ty>| t.is_some_and(|t| !t.is_string());
            match (l, r) {
                (Expr::Str(s), _) if non_string(&rty) => {
                    let target = self.target_of(&rt, rty);
                    lt = self.lit_as(s, target, ordered);
                    lty = rty;
                }
                (_, Expr::Str(s)) if non_string(&lty) => {
                    let target = self.target_of(&lt, lty);
                    rt = self.lit_as(s, target, ordered);
                    rty = lty;
                }
                (Expr::Str(_), _) if rty.is_none() => {
                    return Ok((self.untyped_lit_cmp(op, lt, rt), Some(Ty::of(Fam::Bool))));
                }
                (_, Expr::Str(_)) if lty.is_none() => {
                    return Ok((self.untyped_lit_cmp(op, lt, rt), Some(Ty::of(Fam::Bool))));
                }
                _ => {}
            }
        }
        // SQL Server: no arithmetic between two bits (Msg 402, 8117).
        if self.is_bit(&lty) && self.is_bit(&rty) && matches!(op, "+" | "-" | "*" | "/" | "%") {
            return Ok((
                n("type-error", vec![a("tsql"), a("bit-arith"), a(op), lt, rt]),
                None,
            ));
        }
        if self.is_bool(&lty) || self.is_bool(&rty) {
            let numeric = |t: &Option<Ty>| t.is_some_and(|t| t.is_numeric());
            if numeric(&lty) || numeric(&rty) {
                // operator does not exist: boolean = integer (or + - * / %)
                let kind = if cmp_op { "cmp" } else { "arith" };
                return Ok((self.type_error(kind, vec![a(op), lt, rt]), None));
            }
            if cmp_op {
                lt = Self::bit_value(lt);
                rt = Self::bit_value(rt);
            }
        }
        let any =
            |f: &dyn Fn(&Ty) -> bool| lty.as_ref().is_some_and(f) || rty.as_ref().is_some_and(f);
        match op {
            "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                let string = any(&|t| t.is_string());
                let known = lty.is_some() || rty.is_some();
                if string && any(&|t| !t.is_string()) {
                    // T-SQL converts the string to the other type; PostgreSQL
                    // has no such operator for a typed string.
                    let (x, y, op) = order_operands(op, lt, rt);
                    return Ok((
                        n("cmp-conv", vec![self.dtag(), a(op), x, y]),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                if string {
                    return Ok((
                        self.strcmp(op, (lt, lty), (rt, rty)),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                // A number against a value of unknown type, which may be real.
                let maybe_real = |t: &Option<Ty>, other: (&Term, Option<Ty>)| {
                    t.is_none()
                        && other.1.is_some_and(|x| x.is_numeric())
                        && !Self::small_int(other.0)
                };
                if !known || maybe_real(&lty, (&rt, rty)) || maybe_real(&rty, (&lt, lty)) {
                    self.assume(asm::UNTYPED);
                }
                if self.real_mix((&lt, lty), (&rt, rty)) {
                    // T-SQL converts the other value to real, PostgreSQL both
                    // to double precision: CAST(0.1 AS real) = 0.1 is true
                    // only in T-SQL.
                    let (x, y, op) = order_operands(op, lt, rt);
                    return Ok((
                        n("cmp-real", vec![self.dtag(), a(op), x, y]),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                let (x, y, op) = order_operands(op, lt, rt);
                Ok((n("cmp", vec![a(op), x, y]), Some(Ty::of(Fam::Bool))))
            }
            "||" if self.is_binary(&lt, lty) && self.is_binary(&rt, rty) => {
                Ok(self.concat_bin(lt, lty, rt, rty))
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
            // T-SQL `+` between two binary strings concatenates them.
            "+" if self.d == Dialect::Tsql
                && self.is_binary(&lt, lty)
                && self.is_binary(&rt, rty) =>
            {
                Ok(self.concat_bin(lt, lty, rt, rty))
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
                // Only numbers add alike everywhere: T-SQL `+` concatenates
                // binary strings, PostgreSQL has no bytea `+`.
                if any(&|t| !t.is_numeric()) || lt.is("bytes") || rt.is("bytes") {
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                }
                let both_int = lty.is_some_and(|t| t.fam == Fam::Int)
                    && rty.is_some_and(|t| t.fam == Fam::Int);
                if lty.is_none() || rty.is_none() {
                    self.assume(asm::UNTYPED);
                }
                if matches!(op, "*" | "/" | "%") && !both_int {
                    if let Some(ty) = (op == "*").then(|| self.mul_ty(lty, rty)).flatten() {
                        let mut v = vec![lt, rt];
                        v.sort();
                        return Ok((n("mul", v), ty));
                    }
                    // Decimal precision and scale rules differ per dialect.
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                }
                let Some(ty) = self.arith_ty(lty, rty) else {
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                };
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

    /// A variable-length binary string: a value of that type, or a binary
    /// constant (`0x01`, `'\x01'::bytea`).
    fn is_binary(&self, t: &Term, ty: Option<Ty>) -> bool {
        ty.is_some_and(|t| t.fam == Fam::Binary) || t.is("bytes")
    }

    /// The length a binary operand can reach: a constant's byte count, a
    /// typed value's declared length, None when unbounded.
    fn binary_len(t: &Term, ty: Option<Ty>) -> Option<u32> {
        if t.is("bytes") {
            return t
                .kid(0)
                .and_then(Term::atom)
                .map(|h| h.len().div_ceil(2) as u32);
        }
        ty.and_then(|t| t.len)
    }

    /// Binary concatenation (T-SQL `+`, PostgreSQL and Db2 for i `||`). NULL
    /// in either operand gives NULL everywhere. T-SQL cuts the result to
    /// 8000 bytes unless an operand is varbinary(max), so a longer bounded
    /// result stays T-SQL's own.
    fn concat_bin(
        &mut self,
        lt: Term,
        lty: Option<Ty>,
        rt: Term,
        rty: Option<Ty>,
    ) -> (Term, Option<Ty>) {
        let len = Self::binary_len(&lt, lty)
            .zip(Self::binary_len(&rt, rty))
            .map(|(x, y)| x + y);
        if self.d == Dialect::Tsql && len.is_some_and(|l| l > 8000) {
            return (n("dfn", vec![a("tsql"), a("+"), lt, rt]), None);
        }
        let len = if self.d == Dialect::Tsql { len } else { None };
        (
            n("concat-bin", vec![lt, rt]),
            Some(Ty {
                fam: Fam::Binary,
                len,
                scale: None,
            }),
        )
    }

    /// Whether a real (single precision) meets a number of another type,
    /// other than a small integer literal.
    fn real_mix(&self, l: (&Term, Option<Ty>), r: (&Term, Option<Ty>)) -> bool {
        let real = |t: &Option<Ty>| t.is_some_and(|t| t.fam == Fam::Float && t.len == Some(24));
        let other = |(t, ty): (&Term, Option<Ty>)| {
            ty.is_some_and(|x| x.is_numeric()) && !real(&ty) && !Self::small_int(t)
        };
        (real(&l.1) && other(r)) || (real(&r.1) && other(l))
    }

    /// An integer literal that a real holds exactly.
    fn small_int(t: &Term) -> bool {
        t.is("num")
            && t.kid(0)
                .and_then(Term::atom)
                .and_then(|x| x.parse::<i64>().ok())
                .is_some_and(|x| x.abs() <= 1 << 24)
    }

    /// The type of an exact product of two exact numbers (integers or
    /// decimals), or None where it may not be exact. PostgreSQL multiplies
    /// numerics exactly. T-SQL gives decimal(p1 + p2 + 1, s1 + s2), exact
    /// when that precision is at most 38; above, it cuts the scale and
    /// rounds. An integer counts as decimal(3, 0), (5, 0), (10, 0) or
    /// (19, 0) by its width.
    fn mul_ty(&self, l: Option<Ty>, r: Option<Ty>) -> Option<Option<Ty>> {
        let (x, y) = (l?, r?);
        let exact = |t: &Ty| matches!(t.fam, Fam::Int | Fam::Exact);
        if !exact(&x) || !exact(&y) {
            return None;
        }
        let digits = |t: &Ty| match t.fam {
            Fam::Int => t.len.map(|b| {
                let p = match b {
                    0..=1 => 1,
                    2..=8 => 3,
                    9..=16 => 5,
                    17..=32 => 10,
                    _ => 19,
                };
                (p, 0)
            }),
            _ => t.len.zip(t.scale),
        };
        match (digits(&x), digits(&y)) {
            (Some((p1, s1)), Some((p2, s2))) => {
                let (p, s) = (p1 + p2 + 1, s1 + s2);
                if self.d == Dialect::Tsql && p > 38 {
                    return None;
                }
                Some(Some(Ty {
                    fam: Fam::Exact,
                    len: Some(p.min(38)),
                    scale: Some(s),
                }))
            }
            _ if self.d == Dialect::Postgres => Some(Some(Ty::of(Fam::Exact))),
            _ => None,
        }
    }

    /// The type of `+` or `-` of two numbers, or None where the dialects
    /// differ: T-SQL tinyint arithmetic (PostgreSQL has no tinyint), a real
    /// with another number (real in T-SQL, double precision in PostgreSQL),
    /// and a T-SQL decimal sum of known types that needs more than 38 digits
    /// (it overflows where PostgreSQL's numeric does not).
    fn arith_ty(&self, l: Option<Ty>, r: Option<Ty>) -> Option<Option<Ty>> {
        let (Some(x), Some(y)) = (l, r) else {
            return Some(l.or(r));
        };
        if !x.is_numeric() || !y.is_numeric() {
            return Some(l.or(r));
        }
        let small = |t: &Ty| t.fam == Fam::Int && matches!(t.len, Some(1..=8));
        if self.d == Dialect::Tsql && small(&x) && small(&y) {
            return None;
        }
        let real = |t: &Ty| t.fam == Fam::Float && t.len == Some(24);
        if real(&x) != real(&y) {
            return None;
        }
        if x.fam == Fam::Int && y.fam == Fam::Int {
            let len = x.len.zip(y.len).map(|(a, b)| a.max(b));
            return Some(Some(Ty { len, ..x }));
        }
        if x.fam == Fam::Float || y.fam == Fam::Float {
            let len = if real(&x) { Some(24) } else { Some(53) };
            return Some(Some(Ty {
                fam: Fam::Float,
                len,
                scale: None,
            }));
        }
        // Decimal digits before and after the point.
        let digits = |t: &Ty| match t.fam {
            Fam::Int => t.len.map(|b| {
                let p = match b {
                    0..=1 => 1,
                    2..=8 => 3,
                    9..=16 => 5,
                    17..=32 => 10,
                    _ => 19,
                };
                (p, 0)
            }),
            _ => t.len.zip(t.scale).map(|(p, s)| (p.saturating_sub(s), s)),
        };
        match (digits(&x), digits(&y)) {
            (Some((i1, s1)), Some((i2, s2))) => {
                let (i, s) = (i1.max(i2) + 1, s1.max(s2));
                if self.d == Dialect::Tsql && i + s > 38 {
                    return None;
                }
                Some(Some(Ty {
                    fam: Fam::Exact,
                    len: Some(i + s),
                    scale: Some(s),
                }))
            }
            _ => Some(Some(Ty::of(Fam::Exact))),
        }
    }

    /// What a string literal converts to when it meets this operand: the
    /// declared type of a column, or what the operand's type says alone.
    fn target_of(&self, t: &Term, ty: Option<Ty>) -> Option<Target> {
        if t.is("col") {
            let (table, col) = (t.kid(0)?.atom()?, t.kid(1)?.atom()?);
            return self
                .schema
                .table(Self::base_table(table))?
                .column(col)?
                .target;
        }
        match ty?.fam {
            Fam::Date => Target::of("DATE", &[], self.d),
            Fam::Bool => Target::of("BOOLEAN", &[], self.d),
            _ => None,
        }
    }

    /// The value a string literal converts to, or, when the conversion is not
    /// modelled for certain, a term that only equals the same literal in the
    /// same dialect. uniqueidentifier and uuid sort differently, so their
    /// order comparisons stay dialect-specific.
    fn lit_as(&mut self, s: &str, target: Option<Target>, ordered: bool) -> Term {
        let conv = target
            .filter(|t| !(ordered && *t == Target::Uuid))
            .and_then(|t| literal::convert(s, t, self.d, false));
        match conv {
            Some((t, asm)) => {
                if let Some(x) = asm {
                    self.assume(x);
                }
                t
            }
            None => n(
                "lit-conv",
                vec![self.dtag(), a(target.map_or("?", |t| t.name())), a(s)],
            ),
        }
    }

    /// A string literal against a value of unknown type (no catalog facts):
    /// a string comparison under each side's collation if the value is a
    /// string, a conversion of the literal otherwise.
    fn untyped_lit_cmp(&mut self, op: &str, l: Term, r: Term) -> Term {
        let (x, y, op) = order_operands(op, l, r);
        n("cmp-untyped", vec![self.dtag(), a(op), x, y])
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
                // The image a T-SQL trigger's event has no rows for.
                if let Some(tr) = &self.unit.trigger {
                    if let ([x], Some(m)) = (name.as_slice(), tr.missing) {
                        if x.trim_matches(['[', ']', '"']).eq_ignore_ascii_case(m) {
                            let table = tr.table.clone();
                            let alias = Some(alias.clone().unwrap_or_else(|| m.to_string()));
                            return Ok(self.empty_image(&table, &alias));
                        }
                    }
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
                    Some(e) => self.cond(e)?.0,
                    None => a("-"),
                };
                Ok(n("join", vec![a(kind.to_lowercase()), l, r, on]))
            }
            From::Sub { query, alias, cols } => {
                if !cols.is_empty() {
                    return Err("derived column lists are not supported".into());
                }
                // `(SELECT * FROM t WHERE false)`: the conversion of an image
                // the trigger's event has no rows for.
                if let (Some(_), Some(table)) = (&self.unit.trigger, empty_query(query)) {
                    return Ok(self.empty_image(&table, alias));
                }
                let saved = std::mem::take(&mut self.scopes);
                let q = self.query_parts(query, QCtx::Set);
                self.scopes = saved;
                let (mut parts, items) = q?;
                let key = alias.clone().map(|x| lc(&x)).unwrap_or_default();
                // The bit and boolean columns, by the name the item gives them.
                if query.set_ops.is_empty() && query.items.len() == items.len() {
                    let bits: Vec<(String, Ty)> = query
                        .items
                        .iter()
                        .zip(&items)
                        .filter_map(|(it, (_, ty))| {
                            let ty = ty.filter(|_| self.is_bit(ty) || self.is_bool(ty))?;
                            let name = match (&it.alias, &it.expr) {
                                (Some(a), _) => a.clone(),
                                (None, Expr::Name(p)) => p.last()?.clone(),
                                _ => return None,
                            };
                            Some((lc(name.trim_matches(['[', ']', '"'])), ty))
                        })
                        .collect();
                    if !bits.is_empty() {
                        self.sub_bits.insert(format!("sub:{key}"), bits);
                    }
                }
                let item_terms: Vec<Term> = items.into_iter().map(|x| x.0).collect();
                parts.insert(1, n("items", item_terms));
                let q = (n("q", parts), None::<Ty>);
                self.scopes.last_mut().unwrap().push(Binding {
                    key: key.clone(),
                    table: format!("sub:{key}"),
                    derived: None,
                });
                Ok(n("sub", vec![q.0]))
            }
            From::Values { .. } | From::DataChange { .. } | From::Func { .. } => Err(
                "VALUES, table function or data-change table in FROM is not supported here".into(),
            ),
        }
    }

    /// A relation with the columns of `table` and no rows, under `alias`.
    fn empty_image(&mut self, table: &[String], alias: &Option<String>) -> Term {
        let b = self.bind_table(table, alias);
        let t = n("empty", vec![a(b.table.clone())]);
        self.scopes.last_mut().unwrap().push(b);
        t
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
            Some(w) => self.cond(w)?.0,
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
            Some(h) => self.cond(h)?.0,
            None => a("-"),
        };
        let order = self.order_terms(&s.order_by, None)?;
        let limit = match (&s.top, &s.limit) {
            (Some(t), _) | (None, Some(t)) => Some(self.row_count(t)?),
            _ => None,
        };
        let offset = match &s.offset {
            Some(o) => Some(self.row_count(o)?),
            None => None,
        };
        // EXISTS only asks whether a row qualifies: its ORDER BY and a limit
        // of one row or more change nothing (`EXISTS (SELECT TOP 1 …)`).
        let any_row = |l: &Term| {
            l.is("num")
                && l.kid(0)
                    .and_then(Term::atom)
                    .and_then(|x| x.parse::<u64>().ok())
                    .is_some_and(|x| x >= 1)
        };
        let (order, limit) =
            if ctx == QCtx::Exists && offset.is_none() && limit.as_ref().map_or(true, any_row) {
                (Vec::new(), None)
            } else {
                (order, limit)
            };
        let page = if limit.is_some() || offset.is_some() {
            if order.is_empty() {
                self.assume(asm::PAGE_UNORDERED);
            } else if !self.total_order(&from, &order) {
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
    /// A row count or offset. A literal is itself; anything else may be
    /// NULL, which each dialect reads its own way (T-SQL raises an error,
    /// PostgreSQL drops the limit), and a fractional count is an error in
    /// T-SQL but rounded in PostgreSQL: `(count DIALECT int|? term)`, which
    /// `compare` matches across dialects for integers only, under
    /// `ASM-PAGE-COUNT-NOT-NULL`.
    fn row_count(&mut self, e: &Expr) -> R<Term> {
        let (t, ty) = self.expr(e)?;
        if t.is("num") {
            return Ok(t);
        }
        let int = ty.is_some_and(|ty| ty.fam == Fam::Int);
        Ok(n(
            "count",
            vec![self.dtag(), a(if int { "int" } else { "?" }), t],
        ))
    }

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
        if exactly_one(&q) {
            return n("scalar", vec![q, a("n/a"), a("n/a")]);
        }
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
                    return self.update_from(target, alias, sets, from, where_);
                }
                self.scopes.push(Vec::new());
                let b = self.bind_table(target, alias);
                let table = b.table.clone();
                self.scopes.last_mut().unwrap().push(b);
                let r = (|| {
                    let kv = self.update_sets(&table, sets)?;
                    let w = match where_ {
                        Some(w) => self.cond(w)?.0,
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
                // A string literal into a variable whose type's conversion is
                // modelled: the value it converts to, as a CAST gives it (an
                // assignment rounds like one on both engines).
                let decl = self
                    .unit
                    .locals
                    .iter()
                    .find(|l| super::script::names(self.d, t, &l.name))
                    .and_then(|l| l.decl.as_ref());
                if let (Expr::Str(s), Some(target)) =
                    (v, decl.and_then(|d| Target::of_type(d, self.d)))
                {
                    if let Some((term, asm)) = literal::convert(s, target, self.d, true) {
                        if let Some(x) = asm {
                            self.assume(x);
                        }
                        return Ok(n("assign", vec![sym, term]));
                    }
                }
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
                let val = self.conv_var(&ty, val).0;
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
                        Ok(n("return", vec![self.conv_var(&ret, v).0]))
                    }
                    _ => {
                        let v = self.expr(e)?;
                        let ret = self.unit.returns;
                        Ok(n("return", vec![self.conv_var(&ret, v).0]))
                    }
                }
            }
            Stmt::Declare { .. } => Err("declaration".into()),
            // A constant string only: PRINT shows NULL as an empty line and a
            // number or date in its own format, RAISE NOTICE as <NULL> and in
            // PostgreSQL's text form.
            // PRINT cuts a message off after 8000 bytes (4000 characters for
            // nvarchar); shorter constants show the same on both sides.
            Stmt::Message(Expr::Str(s)) if s.chars().count() <= 4000 => {
                Ok(n("message", vec![n("str", vec![a(s.clone())])]))
            }
            Stmt::Message(_) => Err("a message that is not a string literal".into()),
        }
    }

    /// A message or raised error a format builds (`lower::Msg`): the text
    /// as literal pieces and shown values, `message` (informational, as
    /// `PRINT` and `RAISE NOTICE`) or `raise-error`. A value is shown only
    /// when its text is the same on both sides: a variable-length string
    /// (not blank-padded `char`), or an integer of at most 32 bits for
    /// T-SQL `%d` (bigint is an error there). The NULL text is part of the
    /// term, so T-SQL `(null)` meets only a PostgreSQL `COALESCE(x, '(null)')`.
    pub fn message(&mut self, m: &super::lower::Msg) -> R<Term> {
        use super::lower::{MsgPart, Spec};
        let mut parts = Vec::new();
        let mut text = String::new();
        // Most characters the text can have (None: no bound), and whether it
        // can be empty.
        let mut most = Some(0usize);
        let mut empty = true;
        for p in &m.parts {
            match p {
                MsgPart::Text(s) => {
                    most = most.map(|x| x + s.chars().count());
                    empty &= s.is_empty();
                    text.push_str(s);
                }
                MsgPart::Arg { value, null, spec } => {
                    let (t, ty) = self.expr(value)?;
                    let Some(ty) = ty else {
                        return Err("a message value of unknown type".into());
                    };
                    let (head, width) = match (spec, ty.fam) {
                        (Spec::Str, Fam::VarChar) => ("msg-str", ty.len.map(|l| l as usize)),
                        (Spec::Int, Fam::Int)
                            if self.d != Dialect::Tsql || ty.len.is_some_and(|b| b <= 32) =>
                        {
                            // "-2147483648"
                            empty = false;
                            ("msg-int", Some(11))
                        }
                        (Spec::Str, _) => {
                            return Err(
                                "a message value that is not a variable-length string".into()
                            )
                        }
                        (Spec::Int, _) => {
                            return Err("a %d message value that is not an int".into())
                        }
                    };
                    empty &= null.is_empty() || head == "msg-int";
                    most = most
                        .zip(width)
                        .map(|(x, w)| x + w.max(null.chars().count()));
                    if !text.is_empty() {
                        parts.push(n("str", vec![a(std::mem::take(&mut text))]));
                    }
                    parts.push(n(head, vec![t, a(null.clone())]));
                }
            }
        }
        if !text.is_empty() || parts.is_empty() {
            parts.push(n("str", vec![a(text)]));
        }
        if m.tsql && (empty || most.map_or(true, |x| x > 2047)) {
            self.assume(asm::RAISERROR_TEXT);
        }
        if m.format_var {
            self.assume(asm::RAISERROR_FORMAT);
        }
        Ok(n(if m.error { "raise-error" } else { "message" }, parts))
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
                item_terms.push(n(
                    "item",
                    vec![expr, colname(self.d, "decl", &returned_name(self.d, &name))],
                ));
            }
        } else {
            // The names a client receives (decision 1); derived tables and
            // subqueries keep folded names, which nothing outside observes.
            let named = items.len() == q.items.len();
            item_terms = items
                .into_iter()
                .zip(q.items.iter().map(Some).chain(std::iter::repeat(None)))
                .map(|((t, _), it)| match (named, it, t.kids()) {
                    (true, Some(it), [expr, _]) => {
                        let name = match (&it.alias, &it.expr) {
                            (Some(x), _) => colname(self.d, "alias", &returned_name(self.d, x)),
                            (None, Expr::Name(p)) => match p.last() {
                                Some(x) => colname(self.d, "col", &returned_name(self.d, x)),
                                None => colname(self.d, "expr", ""),
                            },
                            (None, _) => colname(self.d, "expr", ""),
                        };
                        n("item", vec![expr.clone(), name])
                    }
                    _ => t,
                })
                .collect();
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
            if let Some((var, v)) = sel.bare_assign() {
                return self.stmt(&Stmt::Assign(var, v.clone()));
            }
            let (sym, ty) = self.target_sym(&Expr::Var(assigns[0].assign.clone().unwrap()))?;
            let (q, qty) = self.query(sel, QCtx::Into)?;
            let v = (self.scalar(q, "last", "unchanged"), qty);
            return Ok(n("assign", vec![sym, self.conv_var(&ty, v).0]));
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
            return Ok(n("assign", vec![sym, self.conv_var(&ty, v).0]));
        }
        match (self.d, self.unit.kind, self.unit.sql_body) {
            (Dialect::Postgres, UnitKind::ScalarFn, true) => {
                let (q, qty) = self.query(sel, QCtx::Scalar)?;
                let v = (self.scalar(q, "first", "null"), qty);
                let ret = self.unit.returns;
                Ok(n("return", vec![self.conv(&ret, v).0]))
            }
            (
                Dialect::Postgres,
                UnitKind::ScalarFn | UnitKind::TableFn | UnitKind::Proc | UnitKind::Trigger,
                false,
            ) => Err("SELECT without INTO in PL/pgSQL".into()),
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
            InsertSource::Values(rows) => {
                // One statement: every row goes in or none does, on every side.
                self.single_row = true;
                let mut sets = Vec::new();
                for r in rows {
                    let kv = self.insert_row(table, cols, r)?;
                    sets.push(n(
                        "set",
                        kv.into_iter()
                            .map(|(c, v, _)| n("kv", vec![a(c), v]))
                            .collect(),
                    ));
                }
                // Values the table generates (identity, sequence defaults) go
                // to the rows in some order; only listing every column rules
                // them out.
                let all_listed = self
                    .schema
                    .table(Self::base_table(table))
                    .is_some_and(|t| t.columns.iter().all(|c| cols.contains(&lc(&c.name))));
                if !all_listed {
                    self.assume(asm::VALUES_ORDER);
                }
                Ok(n("insertn", vec![a(table), n("rows", sets)]))
            }
            InsertSource::Query(q) => {
                let (parts, items) = self.query_parts(q, QCtx::Scalar)?;
                if items.len() != cols.len() {
                    return Err("INSERT column count does not match the query".into());
                }
                for c in cols {
                    self.written(table, c)?;
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
            self.written(table, c)?;
            let v = self.expr(e)?;
            let (t, ty) = self.conv_col(table, c, v);
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
                let c = lc(c);
                self.written(table, &c)?;
                let v = self.expr(e)?;
                let v = self.conv_col(table, &c, v).0;
                Ok(n("kv", vec![a(c), v]))
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
        let on_t = self.cond(on)?.0;
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
                        let c = lc(c);
                        self.written(table, &c)?;
                        let v = self.expr(e)?;
                        let v = self.conv_col(table, &c, v).0;
                        kv.push(n("kv", vec![a(c), v]));
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

    /// The SET list of an UPDATE of `table`, sorted.
    fn update_sets(&mut self, table: &str, sets: &[(Vec<String>, Expr)]) -> R<Vec<Term>> {
        let mut kv = Vec::new();
        for (cols, e) in sets {
            for c in cols {
                self.written(table, &lc(c))?;
            }
            let (v, vty) = self.expr(e)?;
            if cols.len() == 1 {
                let col = lc(&cols[0]);
                let v = self.conv_col(table, &col, (v, vty)).0;
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
        Ok(kv)
    }

    /// `UPDATE … FROM`. T-SQL `UPDATE a SET … FROM <tables> WHERE w`, where
    /// `a` names one of the FROM tables, and PostgreSQL `UPDATE t [AS] a SET
    /// … FROM <others> WHERE w` both change a target row when the other
    /// tables have a row that satisfies the conditions, with values from
    /// that row. With several such rows each picks one unpredictably, so the
    /// statement is lowered only when the catalog shows there is at most one
    /// (`unique_match`). T-SQL is brought to PostgreSQL's shape: the target
    /// leaves the FROM list, and the ON condition of the inner join that tied
    /// it to its neighbour moves to WHERE (`tsql_update_target`). Both
    /// sides then need every other ON condition to stay clear of the target:
    /// PostgreSQL rejects a reference to it there, and a LEFT JOIN's
    /// condition cannot move.
    fn update_from(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        sets: &[(Vec<String>, Expr)],
        from: &[From],
        where_: &Option<Expr>,
    ) -> R<Term> {
        let (tname, talias, others, mut conds) = match self.d {
            Dialect::Postgres => (target.to_vec(), alias.clone(), from.to_vec(), Vec::new()),
            Dialect::Tsql => tsql_update_target(target, alias, from)?,
            Dialect::Db2i => return Err("UPDATE ... FROM is not supported".into()),
        };
        conds.extend(where_.iter().cloned());
        if others.is_empty() {
            // `UPDATE t … FROM t WHERE w` is a plain UPDATE.
            let where_ = conds
                .into_iter()
                .reduce(|l, r| Expr::Bin("AND".into(), Box::new(l), Box::new(r)));
            return self.stmt(&Stmt::Update {
                target: tname,
                alias: talias,
                sets: sets.to_vec(),
                from: Vec::new(),
                where_,
            });
        }
        let tkey = talias
            .as_ref()
            .map(|x| lc(x))
            .unwrap_or_else(|| lc(tname.last().map(String::as_str).unwrap_or("")));
        let mut ons = Vec::new();
        for f in &others {
            join_conditions(f, &mut ons)?;
        }
        if !ons.is_empty() {
            let ttab = self
                .schema
                .table(tname.last().map(String::as_str).unwrap_or(""));
            for on in &ons {
                if mentions(on, &tkey) {
                    return Err(format!(
                        "UPDATE ... FROM: a join condition other than the target's own names the target `{tkey}`"
                    ));
                }
                let mut bare = Vec::new();
                unqualified(on, &mut bare);
                for name in bare {
                    if ttab.map_or(true, |t| t.column(&name).is_some()) {
                        return Err(format!(
                            "UPDATE ... FROM: `{name}` in a join condition may name a column of the target"
                        ));
                    }
                }
            }
        }
        self.scopes.push(Vec::new());
        let r = (|| {
            let b = self.bind_table(&tname, &talias);
            let table = b.table.clone();
            self.scopes.last_mut().unwrap().push(b);
            let from_t = self.push_from(&others)?;
            let w = match conds
                .into_iter()
                .reduce(|l, r| Expr::Bin("AND".into(), Box::new(l), Box::new(r)))
            {
                Some(w) => self.cond(&w)?.0,
                None => a("-"),
            };
            self.unique_match(&table, &from_t, &w)?;
            let kv = self.update_sets(&table, sets)?;
            Ok(n(
                "update-from",
                vec![a(table), n("set", kv), n("from", from_t), w],
            ))
        })();
        self.scopes.pop();
        r
    }

    /// Every row of `target` matches at most one row of the FROM tables
    /// under the WHERE condition `w`. A table is settled once the target or
    /// tables already settled fix a value for each column of one of its
    /// unique keys (catalog), by an equality: in its own ON condition for a
    /// LEFT JOIN, else in WHERE or any inner join's ON condition. String keys
    /// count only under the dialect's own comparison, which its unique keys
    /// follow.
    fn unique_match(&mut self, target: &str, from: &[Term], w: &Term) -> R<()> {
        fn conj(t: &Term) -> Vec<Term> {
            if t.is("and") {
                t.kids().to_vec()
            } else if t.atom() == Some("-") {
                Vec::new()
            } else {
                vec![t.clone()]
            }
        }
        // (table, own ON condition of a LEFT JOIN)
        fn leaves(t: &Term, out: &mut Vec<(String, Option<Term>)>, all: &mut Vec<Term>) -> bool {
            match t.head() {
                "t" => {
                    out.push((
                        t.kid(0).and_then(Term::atom).unwrap_or("").to_string(),
                        None,
                    ));
                    true
                }
                "join" => {
                    let (Some(kind), Some(l), Some(r), Some(on)) =
                        (t.kid(0).and_then(Term::atom), t.kid(1), t.kid(2), t.kid(3))
                    else {
                        return false;
                    };
                    if !leaves(l, out, all) || !r.is("t") {
                        return false;
                    }
                    let name = r.kid(0).and_then(Term::atom).unwrap_or("").to_string();
                    match kind {
                        "inner" | "cross" => {
                            all.extend(conj(on));
                            out.push((name, None));
                        }
                        "left" => out.push((name, Some(on.clone()))),
                        _ => return false,
                    }
                    true
                }
                _ => false,
            }
        }
        let fail = || {
            Err("UPDATE ... FROM: the catalog does not show that a target row matches at most one row of the other tables; with several, both dialects pick one unpredictably".to_string())
        };
        let mut ls = Vec::new();
        let mut all = conj(w);
        for f in from {
            if !leaves(f, &mut ls, &mut all) {
                return fail();
            }
        }
        let own = if self.d == Dialect::Tsql { "ci" } else { "cs" };
        let mut bound = vec![target.to_string()];
        loop {
            let mut progress = false;
            for (t, on) in &ls {
                if bound.contains(t) {
                    continue;
                }
                let cs = match on {
                    Some(on) => conj(on),
                    None => all.clone(),
                };
                let mut cols = Vec::new();
                for c in &cs {
                    let (x, y) = match c.head() {
                        "cmp" if c.kid(0).and_then(Term::atom) == Some("=") => (c.kid(1), c.kid(2)),
                        "strcmp"
                            if c.kid(0).and_then(Term::atom) == Some("=")
                                && c.kid(1).and_then(Term::atom) == Some(own) =>
                        {
                            (c.kid(3), c.kid(4))
                        }
                        _ => continue,
                    };
                    let (Some(x), Some(y)) = (x, y) else { continue };
                    let fixed = |e: &Term| {
                        !e.any(&|s| {
                            s.is("col")
                                && !s
                                    .kid(0)
                                    .and_then(Term::atom)
                                    .is_some_and(|tb| bound.iter().any(|b| b == tb))
                        })
                    };
                    for (col, other) in [(x, y), (y, x)] {
                        if col.is("col")
                            && col.kid(0).and_then(Term::atom) == Some(t.as_str())
                            && fixed(other)
                        {
                            if let Some(c) = col.kid(1).and_then(Term::atom) {
                                cols.push(c.to_string());
                            }
                        }
                    }
                }
                let key = self.schema.table(Self::base_table(t)).and_then(|tab| {
                    tab.keys
                        .iter()
                        .find(|k| !k.is_empty() && k.iter().all(|c| cols.contains(c)))
                        .cloned()
                });
                if let Some(key) = key {
                    self.discharge(format!("{t}.{} is a unique key (catalog)", key.join(", ")));
                    bound.push(t.clone());
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        if ls.iter().all(|(t, _)| bound.contains(t)) {
            Ok(())
        } else {
            fail()
        }
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
                    Some(w) => self.cond(w)?.0,
                    None => a("-"),
                };
                return Ok(n("delete", vec![a(table), w]));
            }
            if let Some(r) = self.semi_join_delete(target, alias, from, where_) {
                return r;
            }
            self.anti_join_delete(target, from, where_)
        })();
        self.scopes.pop();
        r
    }

    /// A DELETE whose FROM/USING list only filters the target rows:
    /// `DELETE FROM t WHERE EXISTS (SELECT 1 FROM <others> WHERE <conds>)`.
    /// PostgreSQL `DELETE FROM t USING u WHERE w`: the USING tables are
    /// the others and `w` the condition. T-SQL `DELETE t FROM <tables>`:
    /// `t` names exactly one FROM table (by its alias, or by a one-part name
    /// of an unaliased table), all joins are inner or cross joins, and the
    /// others are the remaining tables with every ON condition and WHERE as
    /// the condition. In both a target row is deleted once if at least one
    /// row of the others satisfies the condition. `None` when the shape is
    /// not one of these (outer joins, APPLY, hints, a target not in FROM).
    fn semi_join_delete(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        from: &[From],
        where_: &Option<Expr>,
    ) -> Option<R<Term>> {
        let mut leaves = Vec::new();
        let mut conds = Vec::new();
        for f in from {
            if !inner_leaves(f, &mut leaves, &mut conds) {
                return None;
            }
        }
        let (tname, talias, others) = match self.d {
            Dialect::Postgres => (target.to_vec(), alias.clone(), leaves),
            Dialect::Tsql => {
                if alias.is_some() || target.len() != 1 {
                    return None;
                }
                let key = lc(&target[0]);
                let hits: Vec<usize> = (0..leaves.len())
                    .filter(|&i| match &leaves[i] {
                        From::Table { name, alias, .. } => match alias {
                            Some(a) => lc(a) == key,
                            None => lc(name.last().unwrap()) == key,
                        },
                        From::Sub { alias, .. } | From::Values { alias, .. } => {
                            alias.as_ref().is_some_and(|a| lc(a) == key)
                        }
                        _ => false,
                    })
                    .collect();
                let [hit] = hits[..] else { return None };
                let From::Table { name, alias, hints } = leaves.remove(hit) else {
                    return None;
                };
                if !hints.is_empty() {
                    return None;
                }
                (name, alias, leaves)
            }
            Dialect::Db2i => return None,
        };
        if others.iter().any(|f| match f {
            From::Table { hints, .. } => !hints.is_empty(),
            From::Sub { .. } | From::Values { .. } => false,
            _ => true,
        }) {
            return None;
        }
        conds.extend(where_.iter().cloned());
        if !others.is_empty() {
            // The conditions move into the EXISTS subquery, where an
            // unqualified name resolves to the inner tables first. In the
            // source all tables share one scope, so a name in two of them is
            // an error there; accept a name only when the catalog shows it in
            // exactly one table (or in none, for a parameter or variable).
            if conds.iter().any(has_query) {
                return Some(Err(
                    "DELETE semi-join: a subquery or CASE in the conditions may use unqualified names".into(),
                ));
            }
            let mut bare = Vec::new();
            for c in &conds {
                unqualified(c, &mut bare);
            }
            let mut tables = vec![tname.clone()];
            for f in &others {
                match f {
                    From::Table { name, .. } => tables.push(name.clone()),
                    _ if bare.is_empty() => {}
                    _ => {
                        return Some(Err(
                            "DELETE semi-join: unqualified names next to a derived table".into(),
                        ))
                    }
                }
            }
            for name in &bare {
                let mut hits = 0;
                for t in &tables {
                    match self.schema.table(t.last().unwrap()) {
                        Some(tab) => hits += usize::from(tab.column(name).is_some()),
                        None => {
                            return Some(Err(format!(
                            "DELETE semi-join: `{name}` may name a column of more than one table"
                        )))
                        }
                    }
                }
                if !(hits == 1 || hits == 0 && self.sym(name).is_some()) {
                    return Some(Err(format!(
                        "DELETE semi-join: `{name}` may name a column of more than one table"
                    )));
                }
            }
        }
        let cond = conds
            .into_iter()
            .reduce(|l, r| Expr::Bin("AND".into(), Box::new(l), Box::new(r)));
        let pred = if others.is_empty() {
            cond
        } else {
            Some(Expr::Exists(Box::new(Select {
                items: vec![SelItem {
                    expr: Expr::Num("1".into()),
                    alias: None,
                    assign: None,
                }],
                from: others,
                where_: cond,
                ..Select::default()
            })))
        };
        let b = self.bind_table(&tname, &talias);
        let table = b.table.clone();
        self.scopes.last_mut().unwrap().push(b);
        Some((|| {
            let w = match &pred {
                Some(p) => self.expr(p)?.0,
                None => a("-"),
            };
            Ok(n("delete", vec![a(table), w]))
        })())
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
                // Cheapest test first: the use count and the scan for an
                // earlier definition walk every statement, so they only run
                // in front of a `return` of the local.
                let returns_it = || y.is("return") && y.kid(0) == Some(&sym);
                let uses = || {
                    out.iter()
                        .map(|s| s.term.count(&|t| t == &sym))
                        .sum::<usize>()
                };
                // A statement that was not lowered may assign the local too.
                let first_def = || {
                    !out[..i]
                        .iter()
                        .any(|s| s.err.is_some() || s.term.any(&|t| t == &sym))
                };
                if returns_it() && uses() == 2 && first_def() {
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
                    let v = self.conv_var(&ret, (v, ty)).0;
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
/// The query returns exactly one row: an aggregate query without GROUP BY,
/// HAVING or paging (`q(distinct, items, from, where, group, having, order,
/// page, lock)`). It aggregates even when WHERE matches no row; HAVING, TOP 0
/// or OFFSET can drop that row. Only an aggregate in the select list itself
/// counts, not one inside a subquery.
fn exactly_one(q: &Term) -> bool {
    if !q.is("q") {
        return false;
    }
    let none = |k: usize| q.kid(k).and_then(Term::atom) == Some("-");
    let ungrouped = q
        .kid(4)
        .is_some_and(|g| g.is("group") && g.kids().is_empty());
    let items = q
        .kid(1)
        .filter(|i| i.is("items"))
        .map(Term::kids)
        .unwrap_or(&[]);
    ungrouped && none(5) && none(7) && items.iter().any(aggregates)
}

/// An aggregate call in this expression, outside any subquery.
fn aggregates(t: &Term) -> bool {
    if t.is("q") {
        return false;
    }
    // MIN and MAX of one argument; a string's comparison is the dialect's.
    // SUM and AVG stay the dialect's own (result types differ), but they
    // aggregate all the same (an empty input gives one NULL row).
    let named = |name: Option<&Term>, names: &[&str]| {
        name.and_then(Term::atom)
            .is_some_and(|x| names.contains(&x))
    };
    t.is("count")
        || t.is("stringagg")
        || (t.is("fn") && t.kids().len() == 2 && named(t.kid(0), &["min", "max"]))
        || (t.is("dfn") && t.kids().len() == 3 && named(t.kid(1), &["min", "max", "sum", "avg"]))
        || t.kids().iter().any(aggregates)
}

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

/// Whether an expression holds a subquery or CASE anywhere.
fn has_query(e: &Expr) -> bool {
    match e {
        Expr::Exists(_) | Expr::Subquery(_) | Expr::InQuery(..) | Expr::Case(..) => true,
        Expr::Bin(_, l, r) | Expr::Like(l, r, _, _) => has_query(l) || has_query(r),
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            has_query(x)
        }
        Expr::Call(c) => c.args.iter().any(has_query) || c.order.iter().any(|o| has_query(&o.expr)),
        Expr::InList(x, l, _) => has_query(x) || l.iter().any(has_query),
        Expr::Between(x, y, z, _) => has_query(x) || has_query(y) || has_query(z),
        Expr::Tuple(l) => l.iter().any(has_query),
        _ => false,
    }
}

/// The tables of a tree of inner and cross joins, and its ON conditions;
/// false when the tree has an outer join.
fn inner_leaves(f: &From, leaves: &mut Vec<From>, conds: &mut Vec<Expr>) -> bool {
    match f {
        From::Join {
            kind,
            left,
            right,
            on,
        } => {
            matches!(kind.as_str(), "INNER" | "CROSS")
                && inner_leaves(left, leaves, conds)
                && inner_leaves(right, leaves, conds)
                && {
                    conds.extend(on.iter().cloned());
                    true
                }
        }
        _ => {
            leaves.push(f.clone());
            true
        }
    }
}

/// T-SQL `UPDATE x … FROM <tables>`: the FROM table `x` names (by its
/// alias, or by the one-part name of an unaliased table), the FROM list
/// without it, and the ON condition that joined it, now a WHERE condition.
/// The target must head a comma item or a join chain, or follow its head,
/// joined by an inner or cross join: `(t ⋈k s) ⟕ u = t ⋈k (s ⟕ u)` when
/// the LEFT JOIN's condition does not name `t`, which `update_from` checks.
#[allow(clippy::type_complexity)]
fn tsql_update_target(
    target: &[String],
    alias: &Option<String>,
    from: &[From],
) -> R<(Vec<String>, Option<String>, Vec<From>, Vec<Expr>)> {
    let unsupported = |why: &str| Err(format!("UPDATE ... FROM: {why}"));
    if target.is_empty() || target.len() > 2 {
        return unsupported("the target is not a table name, a schema-qualified one or an alias");
    }
    if alias.is_some() {
        return unsupported("the target has an alias");
    }
    let t: Vec<String> = target.iter().map(|x| lc(x)).collect();
    let one = t.len() == 1;
    let key = t.last().cloned().unwrap_or_default();
    // SQL Server binds the target to the FROM reference of the same table,
    // aliased or not, and a one-part target also to an alias or to an
    // unaliased table's last name part (checked on SQL Server 2022). Where
    // only one side states a schema the default schema decides, which is
    // not modelled.
    let mut unclear = None;
    let mut binds = |f: &From| match f {
        From::Table { name, alias, .. } => {
            let n: Vec<String> = name.iter().map(|x| lc(x)).collect();
            if let Some(a) = alias {
                if lc(a) == key {
                    if !one {
                        unclear = Some("the alias of another FROM item has the target's name");
                    }
                    return one;
                }
            }
            if n.last() != Some(&key) {
                return false;
            }
            if n == t || (one && alias.is_none()) {
                return true;
            }
            if n.len() != t.len() {
                unclear = Some("the target and a FROM table may or may not be the same table (only one states a schema)");
            }
            false
        }
        From::Sub { alias, .. } | From::Values { alias, .. } | From::Func { alias, .. } => {
            let hit = alias.as_ref().is_some_and(|a| lc(a) == key);
            if hit && !one {
                unclear = Some("the alias of another FROM item has the target's name");
            }
            hit && one
        }
        _ => false,
    };
    let mut chains = Vec::new();
    for f in from {
        let mut c = Vec::new();
        if !chain(f, &mut c) {
            return unsupported("a join that is not a chain of inner, left and cross joins");
        }
        chains.push(c);
    }
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for (i, c) in chains.iter().enumerate() {
        for (j, s) in c.iter().enumerate() {
            if binds(&s.1) {
                hits.push((i, j));
            }
        }
    }
    if let Some(w) = unclear {
        return unsupported(w);
    }
    if hits.is_empty() {
        // The target is not in FROM: SQL Server joins it to the FROM
        // tables, in scope in SET and WHERE but not in ON, which is
        // PostgreSQL's shape (checked on SQL Server 2022). A FROM item that
        // also shows the target's name is not modelled.
        let shown = chains.iter().flatten().any(|s| match &s.1 {
            From::Table { name, alias, .. } => match alias {
                Some(a) => lc(a) == key,
                None => name.last().is_some_and(|x| lc(x) == key),
            },
            From::Sub { alias, .. } | From::Values { alias, .. } | From::Func { alias, .. } => {
                alias.as_ref().is_some_and(|a| lc(a) == key)
            }
            _ => true,
        });
        if shown {
            return unsupported("the target is not in FROM, and a FROM item is also named like it");
        }
        return Ok((target.to_vec(), None, from.to_vec(), Vec::new()));
    }
    let [(ci, li)] = hits[..] else {
        return unsupported("the target matches more than one FROM table");
    };
    let c = &mut chains[ci];
    let From::Table { name, alias, hints } = c[li].1.clone() else {
        return unsupported("the target is not a table");
    };
    if !hints.is_empty() {
        return unsupported("table hints on the target");
    }
    let inner = |k: &Option<String>| matches!(k.as_deref(), Some("INNER" | "CROSS"));
    let mut conds = Vec::new();
    match li {
        0 if c.len() == 1 => {
            chains.remove(ci);
        }
        0 if inner(&c[1].0) => {
            conds.extend(c[1].2.take());
            c.remove(0);
            c[0].0 = None;
        }
        1 if inner(&c[1].0) => {
            conds.extend(c[1].2.take());
            c.remove(1);
        }
        _ => return unsupported("the target is not joined first, by an inner or cross join"),
    }
    let others = chains
        .into_iter()
        .filter_map(|c| {
            c.into_iter().reduce(|l, r| {
                let (kind, right, on) = (r.0, r.1, r.2);
                (
                    None,
                    From::Join {
                        kind: kind.unwrap_or_default(),
                        left: Box::new(l.1),
                        right: Box::new(right),
                        on,
                    },
                    None,
                )
            })
        })
        .map(|x| x.1)
        .collect();
    Ok((name, alias, others, conds))
}

/// A left-deep chain of inner, left and cross joins as its tables, each
/// with the join kind (none for the first) and ON condition that adds it.
fn chain(f: &From, out: &mut Vec<(Option<String>, From, Option<Expr>)>) -> bool {
    match f {
        From::Join {
            kind,
            left,
            right,
            on,
        } => {
            matches!(kind.as_str(), "INNER" | "LEFT" | "CROSS")
                && !matches!(right.as_ref(), From::Join { .. })
                && chain(left, out)
                && {
                    out.push((Some(kind.clone()), right.as_ref().clone(), on.clone()));
                    true
                }
        }
        _ => {
            out.push((None, f.clone(), None));
            true
        }
    }
}

/// The ON conditions of a FROM item; an error for joins other than inner,
/// left and cross ones.
fn join_conditions(f: &From, out: &mut Vec<Expr>) -> R<()> {
    if let From::Join {
        kind,
        left,
        right,
        on,
    } = f
    {
        if !matches!(kind.as_str(), "INNER" | "LEFT" | "CROSS") {
            return Err(format!(
                "UPDATE ... FROM with a {kind} JOIN is not supported"
            ));
        }
        join_conditions(left, out)?;
        join_conditions(right, out)?;
        out.extend(on.iter().cloned());
    }
    Ok(())
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

/// A type argument written in CONVERT: a length or MAX.
fn type_arg(e: &Expr) -> bool {
    match e {
        Expr::Num(v) => v.bytes().all(|b| b.is_ascii_digit()),
        Expr::Name(p) => p.len() == 1 && p[0].eq_ignore_ascii_case("max"),
        _ => false,
    }
}

/// The table of `SELECT * FROM t WHERE false` and nothing else.
fn empty_query(q: &Select) -> Option<Vec<String>> {
    let plain = q.ctes.is_empty()
        && !q.for_json
        && q.set_ops.is_empty()
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
        && matches!(
            q.items.as_slice(),
            [SelItem {
                expr: Expr::Star(None),
                alias: None,
                assign: None
            }]
        );
    let is_false = match &q.where_ {
        Some(Expr::Special(s)) => s.eq_ignore_ascii_case("FALSE"),
        Some(Expr::Name(p)) => matches!(p.as_slice(), [x] if x.eq_ignore_ascii_case("false")),
        _ => false,
    };
    match q.from.as_slice() {
        [From::Table {
            name,
            alias: None,
            hints,
        }] if plain && is_false && hints.is_empty() => Some(name.clone()),
        _ => None,
    }
}
