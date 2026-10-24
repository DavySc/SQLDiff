//! Concrete evaluation of one statement over a small database, used to find
//! a witness for a same-dialect edit (`edit`).
//!
//! The evaluator runs both versions of a statement on the same inputs: a
//! "world" of at most two rows per table, parameter and variable values, and
//! the values of subexpressions it does not model. Inputs it needs are
//! created on first use and filled in by a backtracking search over a few
//! candidate values each (`search`). A witness is an assignment under which
//! both versions finish and their observable results differ for certain.
//!
//! Everything it cannot decide for certain stops the evaluation instead of
//! guessing: collation-dependent string comparisons, numeric results that
//! depend on a type it does not know, nondeterministic order, functions it
//! does not model (their results are opaque values that only compare equal
//! to the same function of the same arguments). So a witness is never more
//! than the evaluator can show; when no witness is found the edit stays
//! "unknown".

use std::collections::{HashMap, HashSet};

use sqldiff_syntax::Dialect;

use super::ast::{Call, Cte, Expr, From, InsertSource, OrderItem, Select, Stmt, Type};
use super::schema::{Fam, Schema, Ty};

// ---- values ------------------------------------------------------------

/// A value in a world.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Null,
    Int(i128),
    /// Exact or approximate number other than an integer.
    Num(f64),
    Str(String),
    Bool(bool),
    /// A value of a type the evaluator does not know; values are ordered.
    Gen(u8),
    /// The result of a deterministic subexpression it does not model,
    /// identified by function and argument values. Never NULL.
    Opq(usize),
    /// Some value of the same type that differs from opaque value `k`.
    Ne(usize),
    /// The statement raises an error.
    Err(String),
}

impl V {
    fn is_null(&self) -> bool {
        matches!(self, V::Null)
    }

    /// Plain-language rendering for witnesses.
    pub fn show(&self, opaque: &[String]) -> String {
        match self {
            V::Null => "NULL".into(),
            V::Int(i) => i.to_string(),
            V::Num(f) => {
                let s = format!("{f}");
                if s.contains('.') || s.contains('e') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
            V::Str(s) => format!("'{}'", s.replace('\'', "''")),
            V::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            V::Gen(g) => format!("value {}", (b'a' + g) as char),
            V::Opq(i) => opaque
                .get(*i)
                .map(|s| format!("the value of {s}"))
                .unwrap_or_else(|| "an opaque value".into()),
            V::Ne(i) => opaque
                .get(*i)
                .map(|s| format!("a value other than {s}"))
                .unwrap_or_else(|| "another value".into()),
            V::Err(e) => format!("an error ({e})"),
        }
    }
}

/// Why an evaluation did not produce a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// Needs the value of this input; the search assigns it and retries.
    Need(usize),
    /// This assignment cannot serve as a witness (ambiguous result).
    Reject,
    /// The statement uses something the evaluator does not model at all.
    Unsupported(String),
}

pub type R<T> = Result<T, Stop>;

fn unsupported<T>(what: impl Into<String>) -> R<T> {
    Err(Stop::Unsupported(what.into()))
}

/// Three-valued truth of a value used as a condition.
fn truth(v: &V) -> R<Option<bool>> {
    match v {
        V::Null => Ok(None),
        V::Bool(b) => Ok(Some(*b)),
        V::Err(_) => Ok(None),
        _ => Err(Stop::Reject),
    }
}

fn tri(b: Option<bool>) -> V {
    match b {
        None => V::Null,
        Some(b) => V::Bool(b),
    }
}

// ---- dates ---------------------------------------------------------------

/// A date or timestamp literal: format pattern and value (days, seconds of
/// the day as a fraction string kept verbatim).
#[derive(Debug, Clone, PartialEq)]
struct DateLit {
    sep: bool,
    days: i64,
    rest: String,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_date(s: &str) -> Option<DateLit> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (sep, y, m, d, end) = if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        (true, digits(0..4)?, digits(5..7)?, digits(8..10)?, 10)
    } else if b.len() >= 8 {
        (false, digits(0..4)?, digits(4..6)?, digits(6..8)?, 8)
    } else {
        return None;
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    if civil_from_days(days) != (y, m, d) {
        return None;
    }
    let rest = &s[end..];
    // A time part: ` 23:59:59.9999999` or `T10:00:00`.
    if !rest.is_empty() {
        let t = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('T'))?;
        if !t
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b':' || c == b'.')
            || t.len() < 5
        {
            return None;
        }
    }
    Some(DateLit {
        sep,
        days,
        rest: rest.to_string(),
    })
}

fn format_date(d: &DateLit) -> String {
    let (y, m, dd) = civil_from_days(d.days);
    if d.sep {
        format!("{y:04}-{m:02}-{dd:02}{}", d.rest)
    } else {
        format!("{y:04}{m:02}{dd:02}{}", d.rest)
    }
}

/// Same date literal one day later or earlier.
pub fn date_neighbour(s: &str, delta: i64) -> Option<String> {
    let mut d = parse_date(s)?;
    d.days += delta;
    let (y, _, _) = civil_from_days(d.days);
    if !(1..=9999).contains(&y) {
        return None;
    }
    Some(format_date(&d))
}

// ---- string comparison ---------------------------------------------------

/// Equality of two strings that holds under every common collation, or
/// None when it depends on the collation (case, trailing spaces, accents).
fn str_eq(a: &str, b: &str) -> Option<bool> {
    if a == b {
        return Some(true);
    }
    let fold = |s: &str| s.trim_end_matches(' ').to_lowercase();
    if fold(a) == fold(b) || !a.is_ascii() || !b.is_ascii() {
        return None;
    }
    Some(false)
}

/// Order of two strings that holds under every common collation.
fn str_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::*;
    if a == b {
        return Some(Equal);
    }
    // Two dates in the same format order by date in every type and collation.
    if let (Some(x), Some(y)) = (parse_date(a), parse_date(b)) {
        if x.sep == y.sep && x.rest.len() == y.rest.len() {
            if x.days != y.days {
                return Some(x.days.cmp(&y.days));
            }
            if x.rest
                .bytes()
                .zip(y.rest.bytes())
                .all(|(p, q)| p == q || (p.is_ascii_digit() && q.is_ascii_digit()))
            {
                return Some(x.rest.cmp(&y.rest));
            }
            return None;
        }
    }
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let common = ab.iter().zip(bb).take_while(|(x, y)| x == y).count();
    if !ab[..common].iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    match (ab.get(common), bb.get(common)) {
        (Some(x), Some(y)) => {
            let same_class = (x.is_ascii_digit() && y.is_ascii_digit())
                || (x.is_ascii_lowercase() && y.is_ascii_lowercase())
                || (x.is_ascii_uppercase() && y.is_ascii_uppercase());
            same_class.then(|| x.cmp(y))
        }
        // A proper prefix sorts first when the rest starts with a letter or digit.
        (None, Some(y)) if y.is_ascii_alphanumeric() => Some(Less),
        (Some(x), None) if x.is_ascii_alphanumeric() => Some(Greater),
        _ => None,
    }
}

// ---- inputs ----------------------------------------------------------------

/// What an input stands for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A parameter or variable, lower case without `@`.
    Var(String),
    /// A column of a table row: table key, row index, column name.
    Col(String, usize, String),
    /// A field of a record variable.
    Field(String, String),
}

/// How much the search may choose an input's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Chosen by the caller or the database state (parameters, columns).
    Free,
    /// A local value whose earlier assignments were not traced: using it
    /// needs the assumption that the value can occur.
    Assumed,
    /// Fixed by the routine (a constant), never chosen.
    Fixed,
}

/// Value class of an input, from its declared type or how it is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Int,
    Num,
    Str,
    Bool,
    /// Dates and timestamps, as ISO strings.
    Date,
    /// Unknown type: only ordered, distinct values.
    Gen,
}

#[derive(Debug, Clone)]
pub struct Input {
    pub key: Key,
    pub origin: Origin,
    pub cands: Vec<V>,
}

/// Facts about a parameter or variable of the routine.
#[derive(Debug, Clone)]
pub struct VarFact {
    pub ty: Option<Ty>,
    pub origin: Origin,
    /// Constant value (origin Fixed).
    pub value: Option<V>,
    pub nullable: bool,
    /// Whether only non-negative integers occur (counts, row counts).
    pub non_negative: bool,
    /// Extra candidate values (e.g. `@@FETCH_STATUS`).
    pub cands: Vec<V>,
}

/// What a statement shows its caller or the rest of the routine.
#[derive(Debug, Clone, PartialEq)]
pub enum Obs {
    /// Result rows (ordered when the query orders them).
    Rows(Vec<Vec<V>>, bool),
    /// A value (assignment, RETURN, argument).
    Value(V),
    /// Assignment from a query: the value, or None when no row qualifies
    /// (the variable keeps its value).
    Assigned(Vec<Option<V>>),
    /// Rows written: (table row index, new values or None for a delete).
    Writes(Vec<(usize, Vec<V>)>),
    /// Which branch of a condition chain runs (None: no branch).
    Branch(Option<usize>),
    /// The statement fails.
    Error(String),
}

// ---- evaluation context ---------------------------------------------------------

/// One FROM item's row in scope.
#[derive(Debug, Clone)]
enum Src {
    /// Row `row` of base table `table`.
    Base(String, usize),
    /// A derived row: column name → value.
    Derived(Vec<(String, V)>),
    /// The NULL-extended side of an outer join.
    Nulls,
}

#[derive(Debug, Clone)]
struct Binding {
    /// Names the row can be referred to by (alias, or table name and its last part).
    names: Vec<String>,
    /// Table key for base tables (to find catalog columns).
    table: Option<String>,
    src: Src,
}

type Scope = Vec<Binding>;

/// Hints about an input name: value class and literal values it is
/// compared with.
#[derive(Debug, Clone, Default)]
pub struct Hint {
    pub class: Option<Class>,
    pub lits: Vec<V>,
    /// Tested with IS [NOT] NULL.
    pub null_tested: bool,
}

pub struct Ctx<'s> {
    pub d: Dialect,
    pub schema: &'s Schema,
    pub vars: HashMap<String, VarFact>,
    /// Record variables (PL/pgSQL `r` of `FOR r IN ...`).
    pub records: Vec<String>,
    /// Hints by lower-case column or variable name (last name part).
    pub hints: HashMap<String, Hint>,
    /// Rows per table key.
    pub shape: HashMap<String, usize>,
    pub inputs: Vec<Input>,
    index: HashMap<Key, usize>,
    pub values: Vec<Option<V>>,
    /// Opaque values by identity.
    pub opaque: Vec<String>,
    opaque_ids: HashMap<String, usize>,
    /// Side tag mixed into nondeterministic opaque values (A and B never agree).
    pub side: u8,
    /// Whether a nondeterministic value was used.
    pub nondeterministic: bool,
    /// Tables seen, in order.
    pub tables: Vec<String>,
    /// A candidate value for the next input created (the other side of a comparison).
    extra: Option<V>,
    /// Conditions each side's statement is only reached under: (condition,
    /// whether it must be true or must not be true), for side A and side B.
    pub guards: [Vec<(Expr, bool)>; 2],
    /// A guard could not be evaluated in the last run (reaching the
    /// statement with the witness inputs is then assumed).
    pub guard_skipped: bool,
    /// Common table expressions in scope, innermost last.
    ctes: Vec<Vec<Cte>>,
    /// Result columns of table functions by table key.
    func_schema: HashMap<String, Vec<(String, Ty)>>,
    /// Origin of a table function's rows (that of the variable it reads).
    func_origin: HashMap<String, Origin>,
    /// Variables read only through a table function, and variables read
    /// directly, in this run (one variable may not be both).
    func_vars: HashSet<String>,
    read_vars: HashSet<String>,
}

impl<'s> Ctx<'s> {
    pub fn new(d: Dialect, schema: &'s Schema) -> Self {
        Ctx {
            d,
            schema,
            vars: HashMap::new(),
            records: Vec::new(),
            hints: HashMap::new(),
            shape: HashMap::new(),
            inputs: Vec::new(),
            index: HashMap::new(),
            values: Vec::new(),
            opaque: Vec::new(),
            opaque_ids: HashMap::new(),
            side: 0,
            nondeterministic: false,
            tables: Vec::new(),
            extra: None,
            guards: [Vec::new(), Vec::new()],
            guard_skipped: false,
            ctes: Vec::new(),
            func_schema: HashMap::new(),
            func_origin: HashMap::new(),
            func_vars: HashSet::new(),
            read_vars: HashSet::new(),
        }
    }

    fn input(&mut self, key: Key, origin: Origin, mut cands: Vec<V>) -> R<V> {
        if let Some(&i) = self.index.get(&key) {
            return match &self.values[i] {
                Some(v) => Ok(v.clone()),
                None => Err(Stop::Need(i)),
            };
        }
        if let Some(x) = self.extra.take() {
            if !matches!(x, V::Null | V::Err(_)) && !cands.contains(&x) {
                if let V::Opq(k) = x {
                    cands.insert(0, V::Ne(k));
                }
                cands.insert(0, x);
            }
        }
        let i = self.inputs.len();
        self.inputs.push(Input {
            key: key.clone(),
            origin,
            cands,
        });
        self.index.insert(key, i);
        self.values.push(None);
        Err(Stop::Need(i))
    }

    fn opaque_value(&mut self, id: String, show: String) -> V {
        if let Some(&i) = self.opaque_ids.get(&id) {
            return V::Opq(i);
        }
        let i = self.opaque.len();
        self.opaque.push(show);
        self.opaque_ids.insert(id, i);
        V::Opq(i)
    }

    fn hint(&self, name: &str) -> Hint {
        self.hints.get(name).cloned().unwrap_or_default()
    }

    /// Candidate values for an input of a class.
    fn candidates(&self, class: Class, hint: &Hint, nullable: bool, non_negative: bool) -> Vec<V> {
        let mut out: Vec<V> = Vec::new();
        let mut push = |v: V| {
            if !out.contains(&v) {
                out.push(v);
            }
        };
        match class {
            Class::Int | Class::Num => {
                for l in &hint.lits {
                    match l {
                        V::Int(i) => {
                            push(V::Int(*i));
                            push(V::Int(i + 1));
                            push(V::Int(i - 1));
                        }
                        V::Num(f) if class == Class::Num => {
                            push(V::Num(*f));
                            push(V::Num(f + 1.0));
                            push(V::Num(f - 1.0));
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                for i in [0, 1, 2, -1] {
                    push(if class == Class::Int {
                        V::Int(i)
                    } else {
                        V::Num(i as f64)
                    });
                }
            }
            Class::Str => {
                for l in &hint.lits {
                    match l {
                        V::Str(s) => {
                            push(V::Str(s.clone()));
                            if let Some(n) = date_neighbour(s, 1) {
                                push(V::Str(n));
                            }
                            if let Some(n) = date_neighbour(s, -1) {
                                push(V::Str(n));
                            }
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                if !hint
                    .lits
                    .iter()
                    .any(|l| matches!(l, V::Str(s) if parse_date(s).is_some()))
                {
                    push(V::Str("x".into()));
                    push(V::Str("y".into()));
                }
            }
            Class::Bool => {
                push(V::Bool(true));
                push(V::Bool(false));
            }
            Class::Date => {
                for l in &hint.lits {
                    match l {
                        V::Str(s) if parse_date(s).is_some() => {
                            push(V::Str(s.clone()));
                            for delta in [1, -1] {
                                if let Some(n) = date_neighbour(s, delta) {
                                    push(V::Str(n));
                                }
                            }
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                push(V::Str("2020-01-01".into()));
                push(V::Str("2020-01-02".into()));
            }
            Class::Gen => {
                for l in &hint.lits {
                    if let V::Opq(k) = l {
                        push(l.clone());
                        push(V::Ne(*k));
                    }
                }
                push(V::Gen(0));
                push(V::Gen(1));
            }
        }
        if non_negative {
            out.retain(|v| !matches!(v, V::Int(i) if *i < 0));
        }
        if nullable {
            out.push(V::Null);
        }
        out
    }

    fn class_of_ty(ty: &Ty) -> Option<Class> {
        Some(match ty.fam {
            Fam::Int => Class::Int,
            Fam::Exact | Fam::Float => Class::Num,
            Fam::Char | Fam::VarChar => Class::Str,
            Fam::Bool => Class::Bool,
            Fam::Date | Fam::Timestamp | Fam::TimestampTz => Class::Date,
            Fam::Time => return None,
            Fam::Other => return None,
        })
    }

    /// A parameter or variable.
    fn var(&mut self, name: &str) -> R<V> {
        let n = var_key(name);
        if self.func_vars.contains(&n) {
            return unsupported(format!("{name} is read besides a table function"));
        }
        self.read_vars.insert(n.clone());
        let Some(f) = self.vars.get(&n).cloned() else {
            // T-SQL system functions such as @@ROWCOUNT are declared by the caller.
            return unsupported(format!("unknown variable {name}"));
        };
        if let Some(v) = &f.value {
            return Ok(v.clone());
        }
        let hint = self.hint(&format!("@{n}"));
        // A declared type the evaluator does not model (interval, xml, ...)
        // takes no class from how the variable is compared.
        let class = match &f.ty {
            Some(ty) => Self::class_of_ty(ty).unwrap_or(Class::Gen),
            None => hint.class.unwrap_or(Class::Gen),
        };
        let mut cands = f.cands.clone();
        for c in self.candidates(class, &hint, f.nullable, f.non_negative) {
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
        self.input(Key::Var(n), f.origin, cands)
    }

    fn column(&mut self, table: &str, row: usize, col: &str) -> R<V> {
        let col = col.to_lowercase();
        let hint = self.hint(&col);
        if let Some(schema) = self.func_schema.get(table) {
            // A table function's column: typed by its WITH schema, nullable.
            let Some((_, ty)) = schema.iter().find(|(c, _)| *c == col) else {
                return unsupported(format!("unknown column {col} of {table}"));
            };
            let class = Self::class_of_ty(ty).or(hint.class).unwrap_or(Class::Gen);
            let mut cands = self.candidates(class, &hint, true, false);
            if let (true, Some(len)) = (ty.is_string(), ty.len) {
                cands.retain(|v| !matches!(v, V::Str(s) if s.chars().count() > len as usize));
            }
            let origin = self.func_origin.get(table).copied().unwrap_or(Origin::Free);
            return self.input(Key::Col(table.to_string(), row, col), origin, cands);
        }
        // Temporal reads (`t FOR SYSTEM_TIME AS OF x`) use the catalog of `t`.
        let base = table.split(' ').next().unwrap_or(table);
        let cat = self
            .schema
            .table(last_part(base))
            .and_then(|t| t.column(&col))
            .cloned();
        let class = match &cat {
            Some(c) => Self::class_of_ty(&c.ty).unwrap_or(Class::Gen),
            None => hint.class.unwrap_or(Class::Gen),
        };
        // Without catalog facts a column may be NULL only when the code tests it.
        let nullable = match &cat {
            Some(c) => !c.not_null,
            None => hint.null_tested,
        };
        let cands = self.candidates(class, &hint, nullable, false);
        self.input(Key::Col(table.to_string(), row, col), Origin::Free, cands)
    }

    fn field(&mut self, rec: &str, col: &str) -> R<V> {
        let col = col.to_lowercase();
        let hint = self.hint(&col);
        let class = hint.class.unwrap_or(Class::Gen);
        let cands = self.candidates(class, &hint, hint.null_tested, false);
        self.input(Key::Field(rec.to_string(), col), Origin::Assumed, cands)
    }

    fn rows(&mut self, table: &str) -> usize {
        if !self.tables.iter().any(|t| t == table) {
            self.tables.push(table.to_string());
        }
        *self.shape.get(table).unwrap_or(&1)
    }
}

/// Key of a parameter or variable: lower case without `@`; T-SQL system
/// values (`@@ROWCOUNT`) keep one `@`.
pub fn var_key(name: &str) -> String {
    match name.strip_prefix("@@") {
        Some(rest) => format!("@{}", rest.to_lowercase()),
        None => name.trim_start_matches('@').to_lowercase(),
    }
}

/// Key of a table read: temporal reads (`FOR SYSTEM_TIME AS OF x`) are a
/// table of their own.
pub fn table_key_with(name: &[String], hints: &[String]) -> String {
    let base = table_key(name);
    match hints.iter().find(|h| h.starts_with("SYSTEM_TIME")) {
        Some(h) => format!("{base} {}", h.to_lowercase()),
        None => base,
    }
}

/// Key of a table function call: its name and arguments.
pub fn func_key(name: &[String], args: &[Expr]) -> String {
    let shown: Vec<String> = args
        .iter()
        .map(|a| match a {
            Expr::Var(v) => v.clone(),
            Expr::Name(p) => p.join("."),
            Expr::Str(s) => format!("'{}'", s.replace('\'', "''")),
            Expr::Num(n) => n.clone(),
            _ => "…".into(),
        })
        .collect();
    format!("{}({})", name.join("."), shown.join(", "))
}

/// The JSON text of `FOR JSON` rows, as a value that is equal exactly when
/// the rows are (NULL properties are left out, as FOR JSON does; unordered
/// rows are compared as a multiset).
fn for_json(r: &Rows) -> R<V> {
    if r.rows.is_empty() {
        return Ok(V::Null);
    }
    let mut objs = Vec::new();
    for row in &r.rows {
        let mut props = Vec::new();
        for (n, v) in r.names.iter().zip(row) {
            let s = match v {
                V::Null => continue,
                V::Int(i) => i.to_string(),
                V::Num(f) => format!("{f}"),
                V::Str(s) => format!("{s:?}"),
                V::Bool(b) => b.to_string(),
                V::Gen(g) => format!("\"<value {}>\"", (b'a' + g) as char),
                V::Err(e) => return Ok(V::Err(e.clone())),
                // Opaque values may equal anything.
                V::Opq(_) | V::Ne(_) => return Err(Stop::Reject),
            };
            props.push(format!("{n:?}:{s}"));
        }
        objs.push(format!("{{{}}}", props.join(",")));
    }
    if !r.ordered {
        objs.sort();
    }
    Ok(V::Str(format!("[{}]", objs.join(","))))
}

pub fn last_part(t: &str) -> &str {
    t.rsplit('.').next().unwrap_or(t)
}

pub fn table_key(name: &[String]) -> String {
    name.iter()
        .map(|p| p.trim_matches('"').to_lowercase())
        .collect::<Vec<_>>()
        .join(".")
}

// ---- expressions --------------------------------------------------------------

/// Aggregate context: the rows of the current group.
struct Group<'g> {
    rows: &'g [Scope],
}

const AGGREGATES: &[&str] = &[
    "COUNT",
    "SUM",
    "MIN",
    "MAX",
    "AVG",
    "COUNT_BIG",
    "STRING_AGG",
    "ARRAY_AGG",
    "BOOL_AND",
    "BOOL_OR",
    "EVERY",
    "JSON_AGG",
    "JSONB_AGG",
    "STDEV",
    "VAR",
];

/// Functions that read the clock: one input per function, the same for both versions.
const CLOCK: &[&str] = &[
    "GETDATE",
    "SYSDATETIME",
    "SYSUTCDATETIME",
    "GETUTCDATE",
    "CURRENT_TIMESTAMP",
    "NOW",
    "LOCALTIMESTAMP",
];

/// Hours, minutes, seconds and the verbatim fraction of a time part
/// (` 10:00:00.123`); midnight when there is none.
fn time_of(rest: &str) -> Option<(i64, i64, i64, String)> {
    if rest.is_empty() {
        return Some((0, 0, 0, String::new()));
    }
    let t = &rest[1..];
    let (hms, frac) = match t.find('.') {
        Some(i) => (&t[..i], t[i..].to_string()),
        None => (t, String::new()),
    };
    let parts: Vec<i64> = hms
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [h, m] => Some((*h, *m, 0, frac)),
        [h, m, s] => Some((*h, *m, *s, frac)),
        _ => None,
    }
}

/// T-SQL CONVERT date styles.
fn date_style(d: &str, style: u32) -> Option<String> {
    let dl = parse_date(d)?;
    let (y, m, dd) = civil_from_days(dl.days);
    let (h, mi, s, frac) = time_of(&dl.rest)?;
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let ms = format!("{:0<3}", frac.trim_start_matches('.'))
        .chars()
        .take(3)
        .collect::<String>();
    Some(match style {
        112 => format!("{y:04}{m:02}{dd:02}"),
        23 => format!("{y:04}-{m:02}-{dd:02}"),
        101 => format!("{m:02}/{dd:02}/{y:04}"),
        103 => format!("{dd:02}/{m:02}/{y:04}"),
        111 => format!("{y:04}/{m:02}/{dd:02}"),
        113 => format!(
            "{dd:02} {} {y:04} {h:02}:{mi:02}:{s:02}:{ms}",
            MON[(m - 1) as usize]
        ),
        _ => return None,
    })
}

const NONDETERMINISTIC: &[&str] = &[
    "NEXT VALUE FOR",
    "NEWID",
    "NEWSEQUENTIALID",
    "RAND",
    "RANDOM",
    "GETDATE",
    "SYSDATETIME",
    "SYSUTCDATETIME",
    "GETUTCDATE",
    "SYSDATETIMEOFFSET",
    "NOW",
    "CLOCK_TIMESTAMP",
    "STATEMENT_TIMESTAMP",
    "TRANSACTION_TIMESTAMP",
    "TIMEOFDAY",
    "GEN_RANDOM_UUID",
    "UUID_GENERATE_V4",
    "CRYPT_GEN_RANDOM",
    "NEXTVAL",
    "CURRVAL",
    "SETVAL",
];

fn call_name(c: &Call) -> String {
    c.name.last().map(|s| s.to_uppercase()).unwrap_or_default()
}

pub fn is_aggregate(e: &Expr) -> bool {
    match e {
        Expr::Call(c) => {
            AGGREGATES.contains(&call_name(c).as_str()) && c.name.len() == 1 && !c.over
        }
        _ => false,
    }
}

fn has_aggregate(e: &Expr) -> bool {
    let mut found = false;
    walk_expr(e, &mut |x| {
        if is_aggregate(x) {
            found = true;
        }
    });
    found
}

/// Visits an expression and its subexpressions (not into subqueries).
pub fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Call(c) => {
            for a in &c.args {
                walk_expr(a, f);
            }
        }
        Expr::Bin(_, l, r) => {
            walk_expr(l, f);
            walk_expr(r, f);
        }
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            walk_expr(x, f)
        }
        Expr::InList(x, l, _) => {
            walk_expr(x, f);
            for y in l {
                walk_expr(y, f);
            }
        }
        Expr::InQuery(x, _, _) => walk_expr(x, f),
        Expr::Between(x, l, h, _) => {
            walk_expr(x, f);
            walk_expr(l, f);
            walk_expr(h, f);
        }
        Expr::Like(x, p, _, _) => {
            walk_expr(x, f);
            walk_expr(p, f);
        }
        Expr::Case(o, w, e2) => {
            if let Some(o) = o {
                walk_expr(o, f);
            }
            for (a, b) in w {
                walk_expr(a, f);
                walk_expr(b, f);
            }
            if let Some(e2) = e2 {
                walk_expr(e2, f);
            }
        }
        Expr::Tuple(l) => {
            for y in l {
                walk_expr(y, f);
            }
        }
        _ => {}
    }
}

fn num_of(v: &V) -> Option<f64> {
    match v {
        V::Int(i) => Some(*i as f64),
        V::Num(f) => Some(*f),
        _ => None,
    }
}

fn parse_num(s: &str) -> R<V> {
    if let Ok(i) = s.parse::<i128>() {
        return Ok(V::Int(i));
    }
    match s.parse::<f64>() {
        Ok(f) => Ok(V::Num(f)),
        Err(_) => unsupported(format!("number {s}")),
    }
}

/// SQL equality of two non-NULL values; None when it is not certain.
fn eq_vals(a: &V, b: &V) -> Option<bool> {
    match (a, b) {
        (V::Int(x), V::Int(y)) => Some(x == y),
        (V::Str(x), V::Str(y)) => str_eq(x, y),
        (V::Bool(x), V::Bool(y)) => Some(x == y),
        (V::Gen(x), V::Gen(y)) => Some(x == y),
        (V::Opq(x), V::Opq(y)) if x == y => Some(true),
        (V::Ne(x), V::Ne(y)) if x == y => Some(true),
        (V::Opq(x), V::Ne(y)) | (V::Ne(y), V::Opq(x)) if x == y => Some(false),
        _ => match (num_of(a), num_of(b)) {
            (Some(x), Some(y)) => {
                if x == y {
                    Some(true)
                } else if (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0) {
                    None
                } else {
                    Some(false)
                }
            }
            _ => None,
        },
    }
}

fn cmp_vals(a: &V, b: &V) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (V::Int(x), V::Int(y)) => Some(x.cmp(y)),
        (V::Str(x), V::Str(y)) => str_cmp(x, y),
        (V::Gen(x), V::Gen(y)) => Some(x.cmp(y)),
        (V::Bool(x), V::Bool(y)) => Some(x.cmp(y)),
        (V::Opq(x), V::Opq(y)) if x == y => Some(std::cmp::Ordering::Equal),
        _ => match (num_of(a), num_of(b)) {
            (Some(x), Some(y)) => match eq_vals(a, b) {
                Some(true) => Some(std::cmp::Ordering::Equal),
                Some(false) => x.partial_cmp(&y),
                None => None,
            },
            _ => None,
        },
    }
}

/// Whether two values are the same for an observer (NULL equals NULL).
pub fn same(a: &V, b: &V) -> Option<bool> {
    match (a, b) {
        (V::Null, V::Null) => Some(true),
        (V::Null, _) | (_, V::Null) => Some(false),
        (V::Err(_), V::Err(_)) => Some(true),
        (V::Err(_), _) | (_, V::Err(_)) => Some(false),
        (V::Opq(x), V::Ne(y)) | (V::Ne(y), V::Opq(x)) if x == y => Some(false),
        (V::Opq(x), V::Opq(y)) | (V::Ne(x), V::Ne(y)) => Some(x == y).filter(|e| *e).or(None),
        (V::Opq(_) | V::Ne(_), _) | (_, V::Opq(_) | V::Ne(_)) => None,
        _ => {
            // Stored values: strings must be identical, numbers equal.
            if let (V::Str(x), V::Str(y)) = (a, b) {
                return if x == y {
                    Some(true)
                } else {
                    str_eq(x, y).map(|_| false).or(Some(false))
                };
            }
            eq_vals(a, b)
        }
    }
}

impl Ctx<'_> {
    fn resolve(&mut self, parts: &[String], scopes: &[Scope]) -> R<V> {
        let lower: Vec<String> = parts.iter().map(|p| p.to_lowercase()).collect();
        let col = lower.last().cloned().unwrap_or_default();
        // A parameter or variable (PL/pgSQL names; T-SQL variables always have `@`).
        if lower.len() == 1 && self.d != Dialect::Tsql && self.vars.contains_key(&col) {
            return self.var(&col);
        }
        if lower.len() == 2 && self.d != Dialect::Tsql && self.records.contains(&lower[0]) {
            return self.field(&lower[0], &lower[1]);
        }
        let qual = if lower.len() >= 2 {
            Some(lower[..lower.len() - 1].join("."))
        } else {
            None
        };
        // Innermost scope first.
        for scope in scopes.iter().rev() {
            let hits: Vec<&Binding> = match &qual {
                Some(q) => scope
                    .iter()
                    .filter(|b| b.names.iter().any(|n| n == q || n == last_part(q)))
                    .collect(),
                None => {
                    if scope.len() == 1 {
                        scope.iter().collect()
                    } else {
                        // Several tables: the catalog or derived columns decide.
                        let with: Vec<&Binding> = scope
                            .iter()
                            .filter(|b| self.has_column(b, &col) == Some(true))
                            .collect();
                        let unknown = scope.iter().any(|b| self.has_column(b, &col).is_none());
                        if with.len() == 1 && !unknown {
                            with
                        } else if with.is_empty() && !unknown {
                            Vec::new()
                        } else {
                            return unsupported(format!("ambiguous column {col}"));
                        }
                    }
                }
            };
            match hits.as_slice() {
                [] => continue,
                [b] => {
                    let b = (*b).clone();
                    return self.binding_col(&b, &col);
                }
                _ => return unsupported(format!("ambiguous name {}", lower.join("."))),
            }
        }
        unsupported(format!("unresolved name {}", lower.join(".")))
    }

    fn has_column(&self, b: &Binding, col: &str) -> Option<bool> {
        match &b.src {
            Src::Derived(cols) => Some(cols.iter().any(|(n, _)| n == col)),
            _ => {
                let t = self.schema.table(last_part(b.table.as_deref()?))?;
                Some(t.column(col).is_some())
            }
        }
    }

    fn binding_col(&mut self, b: &Binding, col: &str) -> R<V> {
        match &b.src {
            Src::Base(t, row) => self.column(t, *row, col),
            Src::Derived(cols) => match cols.iter().find(|(n, _)| n == col) {
                Some((_, v)) => Ok(v.clone()),
                None => unsupported(format!("unknown derived column {col}")),
            },
            Src::Nulls => Ok(V::Null),
        }
    }

    fn expr(&mut self, e: &Expr, scopes: &[Scope], g: Option<&Group>) -> R<V> {
        match e {
            Expr::Null => Ok(V::Null),
            Expr::Num(n) => parse_num(n),
            Expr::Str(s) => Ok(V::Str(s.clone())),
            Expr::Var(v) => self.var(v),
            Expr::Name(parts) => self.resolve(parts, scopes),
            Expr::Special(s) => match s.as_str() {
                "TRUE" => Ok(V::Bool(true)),
                "FALSE" => Ok(V::Bool(false)),
                _ => Ok(self.nondet(s)),
            },
            Expr::Star(_) => unsupported("* outside a select list"),
            Expr::Bin(op, l, r) => self.bin(op, l, r, scopes, g),
            Expr::Unary(op, x) => {
                let v = self.expr(x, scopes, g)?;
                match (op.as_str(), v) {
                    (_, V::Err(e)) => Ok(V::Err(e)),
                    ("NOT", v) => Ok(tri(truth(&v)?.map(|b| !b))),
                    ("-", V::Null) => Ok(V::Null),
                    ("-", V::Int(i)) => Ok(V::Int(-i)),
                    ("-", V::Num(f)) => Ok(V::Num(-f)),
                    _ => Err(Stop::Reject),
                }
            }
            Expr::IsNull(x, neg) => {
                let v = self.expr(x, scopes, g)?;
                if let V::Err(e) = v {
                    return Ok(V::Err(e));
                }
                Ok(V::Bool(v.is_null() != *neg))
            }
            Expr::InList(x, list, neg) => {
                let v = self.expr(x, scopes, g)?;
                let mut acc = Some(false);
                for y in list {
                    let w = self.expr(y, scopes, g)?;
                    let t = self.compare("=", &v, &w)?;
                    acc = or3(acc, truth(&t)?);
                    if acc == Some(true) {
                        break;
                    }
                }
                Ok(tri(if *neg { acc.map(|b| !b) } else { acc }))
            }
            Expr::InQuery(x, q, neg) => {
                let v = self.expr(x, scopes, g)?;
                let rows = self.query(q, scopes, false)?;
                let mut acc = Some(false);
                for r in &rows.rows {
                    let Some(w) = r.first() else {
                        return Err(Stop::Reject);
                    };
                    let t = self.compare("=", &v, w)?;
                    acc = or3(acc, truth(&t)?);
                }
                Ok(tri(if *neg { acc.map(|b| !b) } else { acc }))
            }
            Expr::Between(x, lo, hi, neg) => {
                let v = self.expr(x, scopes, g)?;
                let l = self.expr(lo, scopes, g)?;
                let h = self.expr(hi, scopes, g)?;
                let a = truth(&self.compare(">=", &v, &l)?)?;
                let b = truth(&self.compare("<=", &v, &h)?)?;
                let r = and3(a, b);
                Ok(tri(if *neg { r.map(|b| !b) } else { r }))
            }
            Expr::Like(x, p, neg, ci) => {
                let v = self.expr(x, scopes, g)?;
                let pat = self.expr(p, scopes, g)?;
                match (&v, &pat) {
                    (V::Null, _) | (_, V::Null) => Ok(V::Null),
                    (V::Str(s), V::Str(pt)) => {
                        let m = like(s, pt, *ci || self.d == Dialect::Tsql).ok_or(Stop::Reject)?;
                        Ok(V::Bool(m != *neg))
                    }
                    _ => Err(Stop::Reject),
                }
            }
            Expr::Cast(x, ty) => {
                let v = self.expr(x, scopes, g)?;
                self.cast(v, ty)
            }
            Expr::Case(op, whens, els) => {
                let base = match op {
                    Some(o) => Some(self.expr(o, scopes, g)?),
                    None => None,
                };
                for (w, t) in whens {
                    let c = match &base {
                        Some(b) => {
                            let wv = self.expr(w, scopes, g)?;
                            self.compare("=", b, &wv)?
                        }
                        None => self.expr(w, scopes, g)?,
                    };
                    if let V::Err(e) = c {
                        return Ok(V::Err(e));
                    }
                    if truth(&c)? == Some(true) {
                        return self.expr(t, scopes, g);
                    }
                }
                match els {
                    Some(e) => self.expr(e, scopes, g),
                    None => Ok(V::Null),
                }
            }
            Expr::Exists(q) => {
                let rows = self.query(q, scopes, true)?;
                Ok(V::Bool(!rows.rows.is_empty()))
            }
            Expr::Subquery(q) if q.for_json => {
                let rows = self.query(q, scopes, false)?;
                for_json(&rows)
            }
            Expr::Subquery(q) => {
                let rows = self.query(q, scopes, false)?;
                match rows.rows.as_slice() {
                    [] => Ok(V::Null),
                    [r] if r.len() == 1 => Ok(r[0].clone()),
                    [_] => Err(Stop::Reject),
                    _ => Ok(V::Err("subquery returned more than one row".into())),
                }
            }
            Expr::Tuple(_) => unsupported("row value"),
            Expr::Extract(f, x) => {
                let v = self.expr(x, scopes, g)?;
                self.opaque_call(&format!("EXTRACT_{f}"), vec![v])
            }
            Expr::Call(c) => self.call(c, scopes, g),
        }
    }

    fn nondet(&mut self, what: &str) -> V {
        self.nondeterministic = true;
        let id = format!("{}#{}", what, self.side);
        self.opaque_value(id, what.to_string())
    }

    fn bin(&mut self, op: &str, l: &Expr, r: &Expr, scopes: &[Scope], g: Option<&Group>) -> R<V> {
        match op {
            "AND" | "OR" => {
                let a = self.expr(l, scopes, g)?;
                if let V::Err(e) = a {
                    return Ok(V::Err(e));
                }
                let ta = truth(&a)?;
                // Short circuit only where the result is fixed either way.
                if op == "AND" && ta == Some(false) {
                    return Ok(V::Bool(false));
                }
                if op == "OR" && ta == Some(true) {
                    return Ok(V::Bool(true));
                }
                let b = self.expr(r, scopes, g)?;
                if let V::Err(e) = b {
                    return Ok(V::Err(e));
                }
                let tb = truth(&b)?;
                Ok(tri(if op == "AND" {
                    and3(ta, tb)
                } else {
                    or3(ta, tb)
                }))
            }
            "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                // A bare name is read after the other side, which becomes
                // one of its candidate values (`col = f(x)`, `a.x = b.y`).
                let bare = |e: &Expr| matches!(e, Expr::Name(_) | Expr::Var(_));
                if bare(l) && !bare(r) {
                    let b = self.expr(r, scopes, g)?;
                    self.extra = Some(b.clone());
                    let a = self.expr(l, scopes, g);
                    self.extra = None;
                    return self.compare(op, &a?, &b);
                }
                let a = self.expr(l, scopes, g)?;
                if bare(r) {
                    self.extra = Some(a.clone());
                }
                let b = self.expr(r, scopes, g);
                self.extra = None;
                self.compare(op, &a, &b?)
            }
            "+" | "-" | "*" | "/" | "%" | "||" => {
                let a = self.expr(l, scopes, g)?;
                let b = self.expr(r, scopes, g)?;
                self.arith(op, a, b, l, r)
            }
            _ => unsupported(format!("operator {op}")),
        }
    }

    pub fn compare(&mut self, op: &str, a: &V, b: &V) -> R<V> {
        use std::cmp::Ordering::*;
        match (a, b) {
            (V::Err(e), _) | (_, V::Err(e)) => return Ok(V::Err(e.clone())),
            (V::Null, _) | (_, V::Null) => return Ok(V::Null),
            _ => {}
        }
        let r = match op {
            "=" => eq_vals(a, b),
            "<>" => eq_vals(a, b).map(|x| !x),
            _ => cmp_vals(a, b).map(|o| match op {
                "<" => o == Less,
                ">" => o == Greater,
                "<=" => o != Greater,
                _ => o != Less,
            }),
        };
        match r {
            Some(x) => Ok(V::Bool(x)),
            None => Err(Stop::Reject),
        }
    }

    fn arith(&mut self, op: &str, a: V, b: V, le: &Expr, re: &Expr) -> R<V> {
        if let V::Err(e) = &a {
            return Ok(V::Err(e.clone()));
        }
        if let V::Err(e) = &b {
            return Ok(V::Err(e.clone()));
        }
        let strings = |x: &V, y: &V| matches!(x, V::Str(_)) && matches!(y, V::Str(_));
        // String operands of a numeric operator fail in both dialects, when
        // both are certainly strings.
        if matches!(op, "-" | "*" | "/" | "%")
            && self.static_string(le) == Some(true)
            && self.static_string(re) == Some(true)
        {
            return Ok(V::Err(format!(
                "the {} operator does not accept character strings",
                match op {
                    "-" => "subtract",
                    "*" => "multiply",
                    "/" => "divide",
                    _ => "modulo",
                }
            )));
        }
        if a.is_null() || b.is_null() {
            // PostgreSQL `||` and T-SQL `+` both yield NULL (CONCAT_NULL_YIELDS_NULL is ON by default).
            return Ok(V::Null);
        }
        let concat = op == "||" || (op == "+" && self.d == Dialect::Tsql && strings(&a, &b));
        if concat {
            return match (&a, &b) {
                (V::Str(x), V::Str(y)) => Ok(V::Str(format!("{x}{y}"))),
                (V::Str(x), V::Int(y)) if op == "||" => Ok(V::Str(format!("{x}{y}"))),
                (V::Int(x), V::Str(y)) if op == "||" => Ok(V::Str(format!("{x}{y}"))),
                (V::Opq(_) | V::Gen(_) | V::Ne(_), _) | (_, V::Opq(_) | V::Gen(_) | V::Ne(_)) => {
                    self.opaque_call(op, vec![a.clone(), b.clone()])
                }
                _ => Err(Stop::Reject),
            };
        }
        match (&a, &b) {
            (V::Int(x), V::Int(y)) => {
                let r = match op {
                    "+" => x.checked_add(*y),
                    "-" => x.checked_sub(*y),
                    "*" => x.checked_mul(*y),
                    "/" | "%" if *y == 0 => return Err(Stop::Reject),
                    "/" => x.checked_div(*y),
                    "%" => x.checked_rem(*y),
                    _ => None,
                };
                // Stay well inside every integer type the values could have.
                match r {
                    Some(v) if v.abs() < (1 << 30) => Ok(V::Int(v)),
                    _ => Err(Stop::Reject),
                }
            }
            (V::Opq(_) | V::Gen(_) | V::Ne(_), _) | (_, V::Opq(_) | V::Gen(_) | V::Ne(_)) => {
                self.opaque_call(op, vec![a.clone(), b.clone()])
            }
            _ => {
                let (Some(x), Some(y)) = (num_of(&a), num_of(&b)) else {
                    return Err(Stop::Reject);
                };
                let r = match op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    "/" if y == 0.0 => return Err(Stop::Reject),
                    "/" => x / y,
                    _ => return Err(Stop::Reject),
                };
                if r.is_finite() && r.abs() < 1e12 {
                    Ok(V::Num(r))
                } else {
                    Err(Stop::Reject)
                }
            }
        }
    }

    /// Whether an expression is certainly a character string (Some(true)),
    /// certainly not (Some(false)), or unknown.
    fn static_string(&self, e: &Expr) -> Option<bool> {
        match e {
            Expr::Str(_) => Some(true),
            Expr::Num(_) => Some(false),
            Expr::Var(v) => {
                let n = var_key(v);
                self.vars.get(&n)?.ty.as_ref().map(|t| t.is_string())
            }
            Expr::Name(p) if p.len() == 1 && self.d != Dialect::Tsql => {
                let n = p[0].to_lowercase();
                self.vars.get(&n)?.ty.as_ref().map(|t| t.is_string())
            }
            Expr::Bin(op, l, r) if op == "+" || op == "||" => {
                match (self.static_string(l), self.static_string(r)) {
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }
            }
            Expr::Cast(_, ty) => Some(Ty::from_type(ty, self.d, true).is_string()),
            Expr::Call(c) => match call_name(c).as_str() {
                "QUOTENAME" | "UPPER" | "LOWER" | "LTRIM" | "RTRIM" | "TRIM" | "REPLACE"
                | "CONCAT" | "FORMAT" | "NCHAR" | "CHAR" | "LEFT" | "RIGHT" | "SUBSTRING"
                | "STUFF" | "REPLICATE" => Some(true),
                _ => None,
            },
            _ => None,
        }
    }

    fn cast(&mut self, v: V, ty: &Type) -> R<V> {
        if matches!(v, V::Null | V::Err(_)) {
            return Ok(v);
        }
        let t = Ty::from_type(ty, self.d, true);
        match (t.fam, &v) {
            (Fam::Int, V::Int(_)) => Ok(v),
            (Fam::Int, V::Str(s)) => match s.trim().parse::<i128>() {
                Ok(i) => Ok(V::Int(i)),
                Err(_) => Err(Stop::Reject),
            },
            (Fam::Exact | Fam::Float, V::Int(i)) => Ok(V::Num(*i as f64)),
            (Fam::Exact | Fam::Float, V::Num(_)) => Ok(v),
            (Fam::VarChar | Fam::Char, V::Str(s)) => {
                let n = s.chars().count() as u32;
                match t.len {
                    Some(l) if n > l => Err(Stop::Reject),
                    _ if t.fam == Fam::Char => Err(Stop::Reject),
                    _ => Ok(v),
                }
            }
            (Fam::VarChar, V::Int(i)) => Ok(V::Str(i.to_string())),
            // PostgreSQL `name` and other string-like casts of strings.
            (Fam::Other, V::Str(_)) if matches!(ty.name.as_str(), "NAME" | "REGCLASS") => {
                if ty.name == "NAME" {
                    Ok(v)
                } else {
                    self.opaque_call(&format!("CAST_{}", ty.name), vec![v])
                }
            }
            (Fam::Bool, V::Bool(_)) => Ok(v),
            (_, V::Opq(_) | V::Gen(_) | V::Ne(_)) => {
                self.opaque_call(&format!("CAST_{}", ty.name), vec![v])
            }
            _ => Err(Stop::Reject),
        }
    }

    /// A deterministic function the evaluator does not model: an opaque
    /// value per function and argument values.
    fn opaque_call(&mut self, name: &str, args: Vec<V>) -> R<V> {
        if args.iter().any(|a| matches!(a, V::Err(_))) {
            return Ok(args.into_iter().find(|a| matches!(a, V::Err(_))).unwrap());
        }
        let show_args: Vec<String> = args.iter().map(|a| a.show(&self.opaque)).collect();
        let id = format!("{name}({:?})", args);
        let show = format!("{}({})", name.to_lowercase(), show_args.join(", "));
        Ok(self.opaque_value(id, show))
    }

    fn call(&mut self, c: &Call, scopes: &[Scope], g: Option<&Group>) -> R<V> {
        let name = call_name(c);
        if c.over || c.within_group {
            return unsupported("window or ordered-set function");
        }
        if is_aggregate(&Expr::Call(Box::new(c.clone()))) {
            let Some(g) = g else {
                return unsupported("aggregate outside a grouped query");
            };
            return self.aggregate(c, &name, scopes, g);
        }
        if CLOCK.contains(&name.as_str()) {
            // Both versions run at the same moment: the time is an input.
            let cands = vec![
                V::Str("2020-01-01 10:00:00".into()),
                V::Str("2020-01-31 23:59:59".into()),
            ];
            return self.input(
                Key::Var(format!("@now:{}", name.to_lowercase())),
                Origin::Free,
                cands,
            );
        }
        if NONDETERMINISTIC.contains(&name.as_str()) {
            return Ok(self.nondet(&name));
        }
        // Date part names are words, not columns.
        if matches!(
            name.as_str(),
            "DATEADD" | "DATEPART" | "DATEDIFF" | "DATENAME"
        ) {
            if let Some(Expr::Name(p)) = c.args.first() {
                let part = p.join(".").to_uppercase();
                let mut args = Vec::new();
                for a in &c.args[1..] {
                    args.push(self.expr(a, scopes, g)?);
                }
                if let Some(e) = args.iter().find(|a| matches!(a, V::Err(_))) {
                    return Ok(e.clone());
                }
                return self.date_call(&name, &part, args);
            }
        }
        // CONVERT(type, value [, style]).
        if matches!(name.as_str(), "CONVERT" | "TRY_CONVERT")
            && self.d == Dialect::Tsql
            && c.args.len() >= 2
        {
            let ty = match &c.args[0] {
                Expr::Name(p) if p.len() == 1 => Type {
                    name: p[0].to_uppercase(),
                    args: Vec::new(),
                },
                Expr::Call(t) if t.name.len() == 1 => Type {
                    name: t.name[0].to_uppercase(),
                    args: t
                        .args
                        .iter()
                        .map(|a| match a {
                            Expr::Num(n) => n.clone(),
                            Expr::Name(p) => p.join("."),
                            _ => String::new(),
                        })
                        .collect(),
                },
                _ => return unsupported("CONVERT target type"),
            };
            let v = self.expr(&c.args[1], scopes, g)?;
            let style = match c.args.get(2) {
                Some(Expr::Num(n)) => n.parse::<u32>().ok(),
                None => None,
                _ => return unsupported("CONVERT style"),
            };
            return match (style, &v) {
                (_, V::Null | V::Err(_)) => Ok(v),
                (Some(st), V::Str(d)) if parse_date(d).is_some() => {
                    let Some(text) = date_style(d, st) else {
                        return unsupported(format!("CONVERT style {st}"));
                    };
                    self.cast(V::Str(text), &ty).or_else(|_| {
                        // Silent truncation to the target length.
                        let t = Ty::from_type(&ty, self.d, true);
                        match (t.fam, t.len) {
                            (Fam::VarChar | Fam::Char, Some(l)) => Ok(V::Str(
                                date_style(d, st)
                                    .unwrap()
                                    .chars()
                                    .take(l as usize)
                                    .collect(),
                            )),
                            _ => Err(Stop::Reject),
                        }
                    })
                }
                (None, _) => self.cast(v, &ty),
                _ => self.opaque_call(
                    &format!("CONVERT_{}", ty.name),
                    vec![v, V::Int(style.unwrap_or(0) as i128)],
                ),
            };
        }
        let mut args = Vec::with_capacity(c.args.len());
        for a in &c.args {
            args.push(self.expr(a, scopes, g)?);
        }
        if let Some(e) = args.iter().find(|a| matches!(a, V::Err(_))) {
            return Ok(e.clone());
        }
        let tsql = self.d == Dialect::Tsql;
        match (name.as_str(), args.as_slice()) {
            ("COALESCE", _) if !args.is_empty() => Ok(args
                .iter()
                .find(|a| !a.is_null())
                .cloned()
                .unwrap_or(V::Null)),
            ("ISNULL", [a, b]) if tsql => {
                // The replacement is converted to the first argument's type.
                if a.is_null() && !matches!(b, V::Null | V::Int(_)) {
                    return Err(Stop::Reject);
                }
                Ok(if a.is_null() { b.clone() } else { a.clone() })
            }
            ("NULLIF", [a, b]) => {
                if a.is_null() {
                    return Ok(V::Null);
                }
                match truth(&self.compare("=", a, b)?)? {
                    Some(true) => Ok(V::Null),
                    _ => Ok(a.clone()),
                }
            }
            ("YEAR" | "MONTH" | "DAY", [V::Str(d)]) if tsql && parse_date(d).is_some() => {
                let (y, m, dd) = civil_from_days(parse_date(d).unwrap().days);
                Ok(V::Int(match name.as_str() {
                    "YEAR" => y,
                    "MONTH" => m,
                    _ => dd,
                } as i128))
            }
            ("NCHAR" | "CHAR", [V::Int(i)])
                if tsql && ((32..127).contains(i) || matches!(i, 9 | 10 | 13)) =>
            {
                Ok(V::Str(char::from(*i as u8).to_string()))
            }
            (
                "IS TRUE" | "IS NOT TRUE" | "IS FALSE" | "IS NOT FALSE" | "IS UNKNOWN"
                | "IS NOT UNKNOWN",
                [v],
            ) => {
                let t = truth(v)?;
                let hit = match name.trim_start_matches("IS ").trim_start_matches("NOT ") {
                    "TRUE" => t == Some(true),
                    "FALSE" => t == Some(false),
                    _ => t.is_none(),
                };
                Ok(V::Bool(hit != name.starts_with("IS NOT")))
            }
            ("ABS", [V::Null]) => Ok(V::Null),
            ("ABS", [V::Int(i)]) => Ok(V::Int(i.abs())),
            ("ABS", [V::Num(f)]) => Ok(V::Num(f.abs())),
            (
                "UPPER" | "LOWER" | "LEN" | "LENGTH" | "CHAR_LENGTH" | "LTRIM" | "RTRIM" | "TRIM",
                [V::Null],
            ) => Ok(V::Null),
            ("LEN", [V::Str(s)]) if tsql => {
                Ok(V::Int(s.trim_end_matches(' ').chars().count() as i128))
            }
            ("LENGTH" | "CHAR_LENGTH", [V::Str(s)]) if !tsql => {
                Ok(V::Int(s.chars().count() as i128))
            }
            ("UPPER", [V::Str(s)]) if s.is_ascii() => Ok(V::Str(s.to_uppercase())),
            ("LOWER", [V::Str(s)]) if s.is_ascii() => Ok(V::Str(s.to_lowercase())),
            ("CONCAT", _)
                if tsql
                    || args
                        .iter()
                        .all(|a| matches!(a, V::Str(_) | V::Null | V::Int(_))) =>
            {
                let mut s = String::new();
                for a in &args {
                    match a {
                        V::Null => {}
                        V::Str(x) => s.push_str(x),
                        V::Int(i) => s.push_str(&i.to_string()),
                        _ => return self.opaque_call(&name, args.clone()),
                    }
                }
                Ok(V::Str(s))
            }
            _ => {
                if args.iter().any(|a| a.is_null()) && null_strict(&name) {
                    return Ok(V::Null);
                }
                self.opaque_call(&name, args)
            }
        }
    }

    /// DATEADD / DATEPART / DATEDIFF on dates kept as ISO strings.
    fn date_call(&mut self, name: &str, part: &str, args: Vec<V>) -> R<V> {
        if args.iter().any(V::is_null) {
            return Ok(V::Null);
        }
        let unit = match part {
            "YEAR" | "YY" | "YYYY" => "year",
            "MONTH" | "MM" | "M" => "month",
            "DAY" | "DD" | "D" => "day",
            "HOUR" | "HH" => "hour",
            "MINUTE" | "MI" | "N" => "minute",
            "SECOND" | "SS" | "S" => "second",
            _ => return self.opaque_call(&format!("{name}_{part}"), args),
        };
        match (name, args.as_slice()) {
            ("DATEPART", [V::Str(d)]) => {
                let Some(dl) = parse_date(d) else {
                    return self.opaque_call(name, args);
                };
                let (y, m, dd) = civil_from_days(dl.days);
                let Some((h, mi, sec, _)) = time_of(&dl.rest) else {
                    return self.opaque_call(name, args);
                };
                Ok(V::Int(match unit {
                    "year" => y,
                    "month" => m,
                    "day" => dd,
                    "hour" => h,
                    "minute" => mi,
                    _ => sec,
                } as i128))
            }
            ("DATEADD", [V::Int(n), V::Str(d)]) => {
                let Some(mut dl) = parse_date(d) else {
                    return self.opaque_call(name, args);
                };
                let n = *n as i64;
                match unit {
                    "day" => dl.days += n,
                    "month" | "year" => {
                        let (y, m, dd) = civil_from_days(dl.days);
                        let months = y * 12 + (m - 1) + if unit == "year" { n * 12 } else { n };
                        let (ny, nm) = (months.div_euclid(12), months.rem_euclid(12) + 1);
                        // The day is clamped to the end of the month.
                        let mut nd = dd;
                        while civil_from_days(days_from_civil(ny, nm, nd)) != (ny, nm, nd) {
                            nd -= 1;
                        }
                        dl.days = days_from_civil(ny, nm, nd);
                    }
                    _ => {
                        // Time units need a time part to add to.
                        let Some((h, mi, sec, frac)) =
                            time_of(&dl.rest).filter(|_| !dl.rest.is_empty())
                        else {
                            return self.opaque_call(name, args);
                        };
                        let step = match unit {
                            "hour" => 3600,
                            "minute" => 60,
                            _ => 1,
                        };
                        let total = h * 3600 + mi * 60 + sec + n * step;
                        dl.days += total.div_euclid(86400);
                        let t = total.rem_euclid(86400);
                        let lead = &dl.rest[..1];
                        dl.rest = format!(
                            "{lead}{:02}:{:02}:{:02}{frac}",
                            t / 3600,
                            t / 60 % 60,
                            t % 60
                        );
                    }
                }
                let (y, _, _) = civil_from_days(dl.days);
                if !(1..=9999).contains(&y) {
                    return Ok(V::Err("date out of range".into()));
                }
                Ok(V::Str(format_date(&dl)))
            }
            _ => self.opaque_call(&format!("{name}_{part}"), args),
        }
    }

    fn aggregate(&mut self, c: &Call, name: &str, scopes: &[Scope], g: &Group) -> R<V> {
        let rows = g.rows;
        if c.star {
            return Ok(V::Int(rows.len() as i128));
        }
        let Some(arg) = c.args.first() else {
            return unsupported("aggregate without argument");
        };
        let mut vals = Vec::new();
        for r in rows {
            let mut sc: Vec<Scope> = scopes.to_vec();
            sc.push(r.clone());
            let v = self.expr(arg, &sc, None)?;
            if let V::Err(e) = v {
                return Ok(V::Err(e));
            }
            if !v.is_null() {
                vals.push(v);
            }
        }
        if c.distinct {
            let mut d: Vec<V> = Vec::new();
            for v in vals {
                let mut dup = false;
                for x in &d {
                    match eq_vals(x, &v) {
                        Some(true) => dup = true,
                        Some(false) => {}
                        None => return Err(Stop::Reject),
                    }
                }
                if !dup {
                    d.push(v);
                }
            }
            vals = d;
        }
        match name {
            "COUNT" | "COUNT_BIG" => Ok(V::Int(vals.len() as i128)),
            _ if vals.is_empty() => Ok(V::Null),
            "MIN" | "MAX" => {
                let mut best = vals[0].clone();
                for v in &vals[1..] {
                    let o = cmp_vals(v, &best).ok_or(Stop::Reject)?;
                    if (name == "MIN" && o == std::cmp::Ordering::Less)
                        || (name == "MAX" && o == std::cmp::Ordering::Greater)
                    {
                        best = v.clone();
                    }
                }
                Ok(best)
            }
            "SUM" => {
                let mut acc = vals[0].clone();
                for v in &vals[1..] {
                    acc = self.arith("+", acc, v.clone(), &Expr::Null, &Expr::Null)?;
                }
                Ok(acc)
            }
            // Other aggregates of one value: a function of that value.
            _ if vals.len() == 1 && c.order.is_empty() && name != "AVG" => {
                let extra: Vec<V> = Vec::new();
                let mut args = vals.clone();
                args.extend(extra);
                self.opaque_call(name, args)
            }
            // AVG of integers truncates in T-SQL; leave it out.
            _ => Err(Stop::Reject),
        }
    }
}

/// Functions that return NULL for any NULL argument.
fn null_strict(name: &str) -> bool {
    matches!(
        name,
        "UPPER"
            | "LOWER"
            | "LEN"
            | "LENGTH"
            | "SUBSTRING"
            | "SUBSTR"
            | "LEFT"
            | "RIGHT"
            | "REPLACE"
            | "QUOTENAME"
            | "SPLIT_PART"
            | "DATEADD"
            | "DATEDIFF"
            | "DATEPART"
            | "YEAR"
            | "MONTH"
            | "DAY"
            | "ROUND"
            | "FLOOR"
            | "CEILING"
            | "LPAD"
            | "RPAD"
            | "TO_HEX"
            | "POSITION"
            | "STRPOS"
    )
}

fn and3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn or3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// LIKE with `%` and `_` (no character classes); None when the answer could
/// depend on the collation.
fn like(s: &str, p: &str, ci: bool) -> Option<bool> {
    if p.contains('[') || !s.is_ascii() || !p.is_ascii() {
        return None;
    }
    let (s, p): (Vec<char>, Vec<char>) = if ci {
        (
            s.to_lowercase().chars().collect(),
            p.to_lowercase().chars().collect(),
        )
    } else {
        (s.chars().collect(), p.chars().collect())
    };
    fn m(s: &[char], p: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('%') => (0..=s.len()).any(|i| m(&s[i..], &p[1..])),
            Some('_') => !s.is_empty() && m(&s[1..], &p[1..]),
            Some(c) => s.first() == Some(c) && m(&s[1..], &p[1..]),
        }
    }
    let r = m(&s, &p);
    // A case-sensitive mismatch that only differs by case is collation-dependent.
    if !ci && !r {
        let sl: String = s.iter().collect::<String>().to_lowercase();
        let pl: String = p.iter().collect::<String>().to_lowercase();
        let sl: Vec<char> = sl.chars().collect();
        let pl: Vec<char> = pl.chars().collect();
        if m(&sl, &pl) {
            return None;
        }
    }
    Some(r)
}

// ---- queries ---------------------------------------------------------------------

/// Rows a query returns, with output column names.
#[derive(Debug, Clone)]
pub struct Rows {
    pub names: Vec<String>,
    pub rows: Vec<Vec<V>>,
    pub ordered: bool,
}

impl Ctx<'_> {
    /// Rows of one FROM item: each a list of bindings.
    fn from_item(&mut self, f: &From, outer: &[Scope]) -> R<Vec<Scope>> {
        match f {
            From::Table { name, alias, hints } => {
                if let [single] = name.as_slice() {
                    if let Some((level, j)) = self.find_cte(single) {
                        let cte = self.ctes[level][j].clone();
                        // A CTE sees only the CTEs defined before it.
                        let saved = std::mem::take(&mut self.ctes);
                        self.ctes = saved[..level].to_vec();
                        self.ctes.push(saved[level][..j].to_vec());
                        let sub = From::Sub {
                            query: Box::new(cte.query),
                            alias: Some(alias.clone().unwrap_or_else(|| single.clone())),
                            cols: cte.cols,
                        };
                        let r = self.from_item(&sub, &[]);
                        self.ctes = saved;
                        return r;
                    }
                }
                let key = table_key_with(name, hints);
                let base = table_key(name);
                let n = self.rows(&key);
                let mut names = vec![base.clone(), last_part(&base).to_string()];
                if let Some(a) = alias {
                    names = vec![a.to_lowercase()];
                }
                Ok((0..n)
                    .map(|i| {
                        vec![Binding {
                            names: names.clone(),
                            table: Some(key.clone()),
                            src: Src::Base(key.clone(), i),
                        }]
                    })
                    .collect())
            }
            From::Sub { query, alias, cols } => {
                let r = self.query(query, outer, false)?;
                let names: Vec<String> = if cols.is_empty() {
                    r.names.clone()
                } else {
                    cols.iter().map(|c| c.to_lowercase()).collect()
                };
                let alias = alias.as_ref().map(|a| a.to_lowercase()).unwrap_or_default();
                Ok(r.rows
                    .into_iter()
                    .map(|row| {
                        vec![Binding {
                            names: vec![alias.clone()],
                            table: None,
                            src: Src::Derived(names.iter().cloned().zip(row).collect()),
                        }]
                    })
                    .collect())
            }
            From::Values { rows, alias, cols } => {
                let alias = alias.as_ref().map(|a| a.to_lowercase()).unwrap_or_default();
                let mut out = Vec::new();
                for r in rows {
                    let mut vals = Vec::new();
                    for e in r {
                        vals.push(self.expr(e, outer, None)?);
                    }
                    out.push(vec![Binding {
                        names: vec![alias.clone()],
                        table: None,
                        src: Src::Derived(
                            cols.iter().map(|c| c.to_lowercase()).zip(vals).collect(),
                        ),
                    }]);
                }
                Ok(out)
            }
            From::Func {
                name,
                args,
                schema,
                alias,
            } => {
                let key = func_key(name, args);
                if schema.is_empty() {
                    return unsupported(format!("{key} without a WITH schema"));
                }
                // Its rows are free when it reads a parameter (any document can
                // be passed), assumed when it reads a local variable.
                let mut origin = Origin::Free;
                for (i, a) in args.iter().enumerate() {
                    match a {
                        Expr::Var(v) if i == 0 => {
                            let k = var_key(v);
                            if self.read_vars.contains(&k) {
                                return unsupported(format!(
                                    "{v} is read besides a table function"
                                ));
                            }
                            match self.vars.get(&k).map(|f| f.origin) {
                                Some(Origin::Free) => {}
                                Some(Origin::Assumed) => origin = Origin::Assumed,
                                _ => return unsupported(format!("{key} reads a constant")),
                            }
                            self.func_vars.insert(k);
                        }
                        Expr::Str(_) | Expr::Num(_) if i > 0 => {}
                        _ => return unsupported(format!("{key} arguments")),
                    }
                }
                let d = self.d;
                self.func_schema.insert(
                    key.clone(),
                    schema
                        .iter()
                        .map(|(c, t)| (c.to_lowercase(), Ty::from_type(t, d, false)))
                        .collect(),
                );
                self.func_origin.insert(key.clone(), origin);
                let n = self.rows(&key);
                let names = vec![alias
                    .as_ref()
                    .map(|a| a.to_lowercase())
                    .unwrap_or_else(|| name.last().map(|x| x.to_lowercase()).unwrap_or_default())];
                Ok((0..n)
                    .map(|i| {
                        vec![Binding {
                            names: names.clone(),
                            table: Some(key.clone()),
                            src: Src::Base(key.clone(), i),
                        }]
                    })
                    .collect())
            }
            From::DataChange { .. } => unsupported("data change table reference"),
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                let ls = self.from_item(left, outer)?;
                let rs = self.from_item(right, outer)?;
                let mut out = Vec::new();
                let mut right_matched = vec![false; rs.len()];
                for l in &ls {
                    let mut matched = false;
                    for (j, r) in rs.iter().enumerate() {
                        let mut row = l.clone();
                        row.extend(r.iter().cloned());
                        let ok = match on {
                            None => true,
                            Some(c) => {
                                let mut sc = outer.to_vec();
                                sc.push(row.clone());
                                let v = self.expr(c, &sc, None)?;
                                if let V::Err(_) = v {
                                    return Err(Stop::Reject);
                                }
                                truth(&v)? == Some(true)
                            }
                        };
                        if ok {
                            matched = true;
                            right_matched[j] = true;
                            out.push(row);
                        }
                    }
                    if !matched && matches!(kind.as_str(), "LEFT" | "FULL") {
                        let mut row = l.clone();
                        row.extend(nulls_of(&rs, right));
                        out.push(row);
                    }
                }
                if matches!(kind.as_str(), "RIGHT" | "FULL") {
                    for (j, r) in rs.iter().enumerate() {
                        if !right_matched[j] {
                            let mut row = nulls_of(&ls, left);
                            row.extend(r.iter().cloned());
                            out.push(row);
                        }
                    }
                }
                Ok(out)
            }
        }
    }

    fn find_cte(&self, name: &str) -> Option<(usize, usize)> {
        for (level, ctes) in self.ctes.iter().enumerate().rev() {
            if let Some(j) = ctes.iter().position(|c| c.name.eq_ignore_ascii_case(name)) {
                return Some((level, j));
            }
        }
        None
    }

    /// Evaluates a query. `exists_only` skips the select list.
    fn query(&mut self, q: &Select, outer: &[Scope], exists_only: bool) -> R<Rows> {
        if !q.set_ops.is_empty() {
            if q.ctes.is_empty() {
                return self.set_query(q, outer);
            }
            self.ctes.push(q.ctes.clone());
            let r = self.set_query(q, outer);
            self.ctes.pop();
            return r;
        }
        if q.ctes.is_empty() {
            return self.query_body(q, outer, exists_only);
        }
        self.ctes.push(q.ctes.clone());
        let r = self.query_body(q, outer, exists_only);
        self.ctes.pop();
        r
    }

    /// UNION / EXCEPT / INTERSECT, then the ORDER BY and paging of the whole.
    fn set_query(&mut self, q: &Select, outer: &[Scope]) -> R<Rows> {
        let mut head = q.clone();
        head.set_ops.clear();
        head.ctes.clear();
        head.order_by.clear();
        head.offset = None;
        head.limit = None;
        head.for_json = false;
        let first = self.query(&head, outer, false)?;
        let names = first.names;
        let mut rows = first.rows;
        let contains = |set: &[Vec<V>], r: &[V]| -> R<bool> {
            let mut unsure = false;
            for x in set {
                match rows_same(x, r) {
                    Some(true) => return Ok(true),
                    Some(false) => {}
                    None => unsure = true,
                }
            }
            if unsure {
                Err(Stop::Reject)
            } else {
                Ok(false)
            }
        };
        let distinct = |rows: Vec<Vec<V>>| -> R<Vec<Vec<V>>> {
            let mut out: Vec<Vec<V>> = Vec::new();
            for r in rows {
                if !contains(&out, &r)? {
                    out.push(r);
                }
            }
            Ok(out)
        };
        for (op, o) in &q.set_ops {
            let r = self.query(o, outer, false)?;
            if r.names.len() != names.len() {
                return unsupported("set operation operands with different column counts");
            }
            rows = match op.as_str() {
                "UNION ALL" => {
                    rows.extend(r.rows);
                    rows
                }
                "UNION" => {
                    rows.extend(r.rows);
                    distinct(rows)?
                }
                "EXCEPT" | "INTERSECT" => {
                    let keep = op == "INTERSECT";
                    let mut out = Vec::new();
                    for x in distinct(rows)? {
                        if contains(&r.rows, &x)? == keep {
                            out.push(x);
                        }
                    }
                    out
                }
                _ => return unsupported(format!("{op}")),
            };
        }
        let mut ordered = false;
        if !q.order_by.is_empty() {
            let mut keys = Vec::new();
            for o in &q.order_by {
                let i = match &o.expr {
                    Expr::Num(n) => n.parse::<usize>().ok().and_then(|k| k.checked_sub(1)),
                    Expr::Name(p) => p
                        .last()
                        .and_then(|c| names.iter().position(|n| n.eq_ignore_ascii_case(c))),
                    _ => None,
                };
                let Some(i) = i.filter(|i| *i < names.len()) else {
                    return unsupported("ORDER BY of a set operation by an expression");
                };
                let nulls_first = o.nulls_first.unwrap_or(if self.d == Dialect::Postgres {
                    o.desc
                } else {
                    !o.desc
                });
                keys.push((i, o.desc, nulls_first));
            }
            let cmp = |a: &Vec<V>, b: &Vec<V>| -> Option<std::cmp::Ordering> {
                for &(i, desc, nf) in &keys {
                    let ord = match (a[i].is_null(), b[i].is_null()) {
                        (true, true) => std::cmp::Ordering::Equal,
                        (true, false) => {
                            if nf {
                                std::cmp::Ordering::Less
                            } else {
                                std::cmp::Ordering::Greater
                            }
                        }
                        (false, true) => {
                            if nf {
                                std::cmp::Ordering::Greater
                            } else {
                                std::cmp::Ordering::Less
                            }
                        }
                        _ => {
                            let o = cmp_vals(&a[i], &b[i])?;
                            if desc {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                    };
                    if ord != std::cmp::Ordering::Equal {
                        return Some(ord);
                    }
                }
                Some(std::cmp::Ordering::Equal)
            };
            // Insertion sort that gives up on undecidable comparisons.
            let mut sorted: Vec<Vec<V>> = Vec::new();
            ordered = true;
            for r in rows {
                let mut at = sorted.len();
                for (j, x) in sorted.iter().enumerate() {
                    match cmp(&r, x) {
                        None => return Err(Stop::Reject),
                        Some(std::cmp::Ordering::Less) => {
                            at = j;
                            break;
                        }
                        Some(std::cmp::Ordering::Equal) => {
                            if rows_same(&r, x) != Some(true) {
                                ordered = false;
                            }
                        }
                        _ => {}
                    }
                }
                sorted.insert(at, r);
            }
            rows = sorted;
        }
        let num = |c: &mut Self, e: &Option<Expr>| -> R<Option<usize>> {
            match e {
                None => Ok(None),
                Some(e) => match c.expr(e, outer, None)? {
                    V::Int(i) if i >= 0 => Ok(Some(i as usize)),
                    _ => unsupported("non-constant paging"),
                },
            }
        };
        let offset = num(self, &q.offset)?.unwrap_or(0);
        let limit = num(self, &q.limit)?;
        if offset > 0 || limit.is_some_and(|l| l < rows.len()) {
            if !ordered {
                return Err(Stop::Reject);
            }
            rows = rows
                .into_iter()
                .skip(offset)
                .take(limit.unwrap_or(usize::MAX))
                .collect();
        }
        Ok(Rows {
            names,
            rows,
            ordered,
        })
    }

    fn query_body(&mut self, q: &Select, outer: &[Scope], exists_only: bool) -> R<Rows> {
        if let Some(rows) = &q.values {
            let mut out = Vec::new();
            for r in rows {
                let mut vals = Vec::new();
                for e in r {
                    vals.push(self.expr(e, outer, None)?);
                }
                out.push(vals);
            }
            return Ok(Rows {
                names: Vec::new(),
                rows: out,
                ordered: false,
            });
        }
        // FROM: cartesian product of the items.
        let mut product: Vec<Scope> = vec![Vec::new()];
        for f in &q.from {
            let rows = self.from_item(f, outer)?;
            let mut next = Vec::new();
            for p in &product {
                for r in &rows {
                    let mut x = p.clone();
                    x.extend(r.iter().cloned());
                    next.push(x);
                }
            }
            product = next;
        }
        // WHERE.
        let mut kept = Vec::new();
        for row in product {
            let ok = match &q.where_ {
                None => true,
                Some(w) => {
                    let mut sc = outer.to_vec();
                    sc.push(row.clone());
                    let v = self.expr(w, &sc, None)?;
                    if let V::Err(_) = v {
                        return Err(Stop::Reject);
                    }
                    truth(&v)? == Some(true)
                }
            };
            if ok {
                kept.push(row);
            }
        }
        let grouped = !q.group_by.is_empty()
            || q.having.is_some()
            || q.items.iter().any(|i| has_aggregate(&i.expr));
        if exists_only && !grouped {
            return Ok(Rows {
                names: Vec::new(),
                rows: kept.into_iter().map(|_| Vec::new()).collect(),
                ordered: false,
            });
        }
        if q.distinct && exists_only {
            return Ok(Rows {
                names: Vec::new(),
                rows: if kept.is_empty() {
                    Vec::new()
                } else {
                    vec![Vec::new()]
                },
                ordered: false,
            });
        }
        let names: Vec<String> = q
            .items
            .iter()
            .map(|i| match (&i.alias, &i.expr) {
                (Some(a), _) => a.to_lowercase(),
                (None, Expr::Name(p)) => p.last().map(|s| s.to_lowercase()).unwrap_or_default(),
                _ => String::new(),
            })
            .collect();
        // Output rows with their sort keys.
        let mut out: Vec<(Vec<V>, Vec<V>)> = Vec::new();
        if grouped {
            // Groups by key values; an aggregate without GROUP BY makes one group.
            let mut groups: Vec<(Vec<V>, Vec<Scope>)> = Vec::new();
            if q.group_by.is_empty() {
                groups.push((Vec::new(), kept));
            } else {
                for row in kept {
                    let mut sc = outer.to_vec();
                    sc.push(row.clone());
                    let mut key = Vec::new();
                    for e in &q.group_by {
                        key.push(self.expr(e, &sc, None)?);
                    }
                    let mut placed = false;
                    for (k, rows) in groups.iter_mut() {
                        let mut eq = true;
                        for (x, y) in k.iter().zip(&key) {
                            match same(x, y) {
                                Some(true) => {}
                                Some(false) => eq = false,
                                None => return Err(Stop::Reject),
                            }
                        }
                        if eq {
                            rows.push(row.clone());
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        groups.push((key, vec![row]));
                    }
                }
            }
            for (_, rows) in &groups {
                // Non-aggregated columns are read from the group's first row.
                let first = rows.first().cloned().unwrap_or_default();
                let mut sc = outer.to_vec();
                sc.push(if rows.is_empty() { Vec::new() } else { first });
                let g = Group { rows };
                if let Some(h) = &q.having {
                    let v = self.expr(h, &sc, Some(&g))?;
                    if truth(&v)? != Some(true) {
                        continue;
                    }
                }
                if exists_only {
                    out.push((Vec::new(), Vec::new()));
                    continue;
                }
                let mut vals = Vec::new();
                for i in &q.items {
                    if matches!(i.expr, Expr::Star(_)) {
                        return unsupported("* in a grouped query");
                    }
                    vals.push(self.expr(&i.expr, &sc, Some(&g))?);
                }
                let mut keys = Vec::new();
                for o in &q.order_by {
                    keys.push(self.order_key(o, &q.items, &vals, &sc, Some(&g))?);
                }
                out.push((vals, keys));
            }
        } else {
            for row in kept {
                let mut sc = outer.to_vec();
                sc.push(row.clone());
                let mut vals = Vec::new();
                for i in &q.items {
                    if let Expr::Star(_) = i.expr {
                        return unsupported("SELECT *");
                    }
                    vals.push(self.expr(&i.expr, &sc, None)?);
                }
                let mut keys = Vec::new();
                for o in &q.order_by {
                    keys.push(self.order_key(o, &q.items, &vals, &sc, None)?);
                }
                out.push((vals, keys));
            }
        }
        if q.distinct {
            let mut d: Vec<(Vec<V>, Vec<V>)> = Vec::new();
            for r in out {
                let mut dup = false;
                for x in &d {
                    if rows_same(&x.0, &r.0).ok_or(Stop::Reject)? {
                        dup = true;
                    }
                }
                if !dup {
                    d.push(r);
                }
            }
            out = d;
        }
        // ORDER BY: a stable sort is only meaningful without ties between different rows.
        let mut ties = false;
        if !q.order_by.is_empty() {
            let mut err = false;
            let descs: Vec<(bool, Option<bool>)> =
                q.order_by.iter().map(|o| (o.desc, o.nulls_first)).collect();
            let d = self.d;
            out.sort_by(|x, y| {
                for (i, (desc, nf)) in descs.iter().enumerate() {
                    let (a, b) = (&x.1[i], &y.1[i]);
                    let o = match (a, b) {
                        (V::Null, V::Null) => std::cmp::Ordering::Equal,
                        (V::Null, _) | (_, V::Null) => {
                            // T-SQL sorts NULL first ascending; PostgreSQL last.
                            let null_small = match nf {
                                Some(f) => *f != *desc,
                                None => d == Dialect::Tsql,
                            };
                            let o = if a.is_null() == null_small {
                                std::cmp::Ordering::Less
                            } else {
                                std::cmp::Ordering::Greater
                            };
                            if *desc {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        _ => match cmp_vals(a, b) {
                            Some(o) => {
                                if *desc {
                                    o.reverse()
                                } else {
                                    o
                                }
                            }
                            None => {
                                err = true;
                                std::cmp::Ordering::Equal
                            }
                        },
                    };
                    if o != std::cmp::Ordering::Equal {
                        return o;
                    }
                }
                std::cmp::Ordering::Equal
            });
            if err {
                return Err(Stop::Reject);
            }
            for w in out.windows(2) {
                let tie = w[0]
                    .1
                    .iter()
                    .zip(&w[1].1)
                    .all(|(a, b)| same(a, b) != Some(false));
                if tie && rows_same(&w[0].0, &w[1].0) != Some(true) {
                    // Rows the ORDER BY does not tell apart come in either order.
                    ties = true;
                }
            }
        }
        // TOP / LIMIT / OFFSET.
        let mut rows: Vec<Vec<V>> = out.into_iter().map(|r| r.0).collect();
        let offset = match &q.offset {
            Some(e) => match self.expr(e, outer, None)? {
                V::Int(i) if i >= 0 => i as usize,
                _ => return Err(Stop::Reject),
            },
            None => 0,
        };
        let limit = match q.top.as_ref().or(q.limit.as_ref()) {
            Some(e) => match self.expr(e, outer, None)? {
                V::Int(i) if i >= 0 => Some(i as usize),
                V::Null if q.limit.is_some() => None,
                _ => return Err(Stop::Reject),
            },
            None => None,
        };
        if ties && (offset > 0 || limit.is_some_and(|l| rows.len() > l)) {
            return Err(Stop::Reject);
        }
        if (limit.is_some() || offset > 0)
            && q.order_by.is_empty()
            && rows.len() > limit.unwrap_or(0) + offset
        {
            // Which rows are kept is unspecified, unless they all look the same.
            let first = rows.first().cloned();
            if !rows
                .iter()
                .all(|r| first.as_ref().and_then(|f| rows_same(f, r)) == Some(true))
            {
                return Err(Stop::Reject);
            }
        }
        rows = rows.into_iter().skip(offset).collect();
        if let Some(l) = limit {
            rows.truncate(l);
        }
        Ok(Rows {
            names,
            rows,
            ordered: !q.order_by.is_empty() && !ties,
        })
    }

    fn order_key(
        &mut self,
        o: &OrderItem,
        items: &[super::ast::SelItem],
        vals: &[V],
        sc: &[Scope],
        g: Option<&Group>,
    ) -> R<V> {
        // ORDER BY 2: the second output column.
        if let Expr::Num(n) = &o.expr {
            if let Ok(i) = n.parse::<usize>() {
                return vals.get(i.wrapping_sub(1)).cloned().ok_or(Stop::Reject);
            }
        }
        // An output alias.
        if let Expr::Name(p) = &o.expr {
            if p.len() == 1 {
                let n = p[0].to_lowercase();
                if let Some(i) = items
                    .iter()
                    .position(|it| it.alias.as_ref().is_some_and(|a| a.to_lowercase() == n))
                {
                    return Ok(vals[i].clone());
                }
            }
        }
        self.expr(&o.expr, sc, g)
    }
}

fn nulls_of(rows: &[Scope], f: &From) -> Scope {
    // The NULL-extended side keeps its names so its columns read as NULL.
    match rows.first() {
        Some(r) => r
            .iter()
            .map(|b| Binding {
                names: b.names.clone(),
                table: b.table.clone(),
                src: Src::Nulls,
            })
            .collect(),
        None => from_names(f)
            .into_iter()
            .map(|(names, table)| Binding {
                names,
                table,
                src: Src::Nulls,
            })
            .collect(),
    }
}

fn from_names(f: &From) -> Vec<(Vec<String>, Option<String>)> {
    match f {
        From::Table { name, alias, hints } => {
            let base = table_key(name);
            let names = match alias {
                Some(a) => vec![a.to_lowercase()],
                None => vec![base.clone(), last_part(&base).to_string()],
            };
            vec![(names, Some(table_key_with(name, hints)))]
        }
        From::Func {
            name, args, alias, ..
        } => {
            let names = vec![alias
                .as_ref()
                .map(|a| a.to_lowercase())
                .unwrap_or_else(|| name.last().map(|x| x.to_lowercase()).unwrap_or_default())];
            vec![(names, Some(func_key(name, args)))]
        }
        From::Sub { alias, .. } | From::Values { alias, .. } | From::DataChange { alias, .. } => {
            vec![(
                vec![alias.as_ref().map(|a| a.to_lowercase()).unwrap_or_default()],
                None,
            )]
        }
        From::Join { left, right, .. } => {
            let mut v = from_names(left);
            v.extend(from_names(right));
            v
        }
    }
}

fn rows_same(a: &[V], b: &[V]) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    let mut all = true;
    for (x, y) in a.iter().zip(b) {
        match same(x, y) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => all = false,
        }
    }
    if all {
        Some(true)
    } else {
        None
    }
}

/// Whether two observations certainly differ (Some(true)), certainly agree
/// (Some(false)) or cannot be told apart (None).
pub fn differ(a: &Obs, b: &Obs) -> Option<bool> {
    match (a, b) {
        (Obs::Error(_), Obs::Error(_)) => Some(false),
        (Obs::Error(_), _) | (_, Obs::Error(_)) => Some(true),
        (Obs::Value(x), Obs::Value(y)) => same(x, y).map(|s| !s),
        (Obs::Branch(x), Obs::Branch(y)) => Some(x != y),
        (Obs::Assigned(x), Obs::Assigned(y)) => {
            let mut unknown = false;
            for (p, q) in x.iter().zip(y) {
                match (p, q) {
                    (None, None) => {}
                    (Some(p), Some(q)) => match same(p, q) {
                        Some(true) => {}
                        Some(false) => return Some(true),
                        None => unknown = true,
                    },
                    // One side assigns, the other keeps the previous value.
                    _ => return Some(true),
                }
            }
            if unknown {
                None
            } else {
                Some(false)
            }
        }
        (Obs::Writes(x), Obs::Writes(y)) => {
            let xi: Vec<usize> = x.iter().map(|w| w.0).collect();
            let yi: Vec<usize> = y.iter().map(|w| w.0).collect();
            if xi != yi {
                return Some(true);
            }
            let mut unknown = false;
            for (p, q) in x.iter().zip(y) {
                match rows_same(&p.1, &q.1) {
                    Some(true) => {}
                    Some(false) => return Some(true),
                    None => unknown = true,
                }
            }
            if unknown {
                None
            } else {
                Some(false)
            }
        }
        (Obs::Rows(x, ox), Obs::Rows(y, oy)) => {
            if x.len() != y.len() {
                return Some(true);
            }
            if *ox && *oy {
                let mut unknown = false;
                for (p, q) in x.iter().zip(y) {
                    match rows_same(p, q) {
                        Some(true) => {}
                        Some(false) => return Some(true),
                        None => unknown = true,
                    }
                }
                return if unknown { None } else { Some(false) };
            }
            // As multisets: every row of x matched by a distinct row of y.
            let mut used = vec![false; y.len()];
            for p in x {
                let mut found = false;
                for (j, q) in y.iter().enumerate() {
                    if used[j] {
                        continue;
                    }
                    match rows_same(p, q) {
                        Some(true) => {
                            used[j] = true;
                            found = true;
                            break;
                        }
                        Some(false) => {}
                        None => return None,
                    }
                }
                if !found {
                    return Some(true);
                }
            }
            Some(false)
        }
        _ => None,
    }
}

// ---- statements --------------------------------------------------------------------

/// What to observe of a statement.
#[derive(Debug, Clone)]
pub enum Probe {
    /// A data statement (SELECT, INSERT, UPDATE, DELETE, assignment, RETURN, DECLARE).
    Stmt(Stmt),
    /// A chain of conditions (IF / ELSIF / WHILE / CONTINUE WHEN): the index
    /// of the first one that is TRUE.
    Conds(Vec<Expr>),
    /// The values of expressions (arguments of THROW, RAISE, PRINT, EXEC).
    Values(Vec<Expr>),
}

impl Ctx<'_> {
    /// Rejects inputs under which this side never reaches the statement.
    fn check_guards(&mut self) -> R<()> {
        let i = usize::from(self.side == 2);
        let guards = std::mem::take(&mut self.guards[i]);
        let mut r = Ok(());
        for (e, must) in &guards {
            // A guard that cannot be decided is skipped (and reported); one
            // that fails or is decided the wrong way rejects the inputs.
            let t = match self.expr(e, &[], None) {
                Ok(V::Err(_)) => {
                    r = Err(Stop::Reject);
                    break;
                }
                Ok(v) => truth(&v),
                Err(stop) => Err(stop),
            };
            match t {
                Ok(t) if (t == Some(true)) == *must => {}
                Ok(_) => {
                    r = Err(Stop::Reject);
                    break;
                }
                Err(Stop::Need(n)) => {
                    r = Err(Stop::Need(n));
                    break;
                }
                Err(_) => self.guard_skipped = true,
            }
        }
        self.guards[i] = guards;
        r
    }

    pub fn observe(&mut self, p: &Probe) -> R<Obs> {
        self.func_vars.clear();
        self.read_vars.clear();
        self.check_guards()?;
        let r = match p {
            Probe::Conds(cs) => {
                for (i, c) in cs.iter().enumerate() {
                    let v = self.expr(c, &[], None)?;
                    if let V::Err(e) = v {
                        return Ok(Obs::Error(e));
                    }
                    if truth(&v)? == Some(true) {
                        return Ok(Obs::Branch(Some(i)));
                    }
                }
                Ok(Obs::Branch(None))
            }
            Probe::Values(es) => {
                let mut vals = Vec::new();
                for e in es {
                    let v = self.expr(e, &[], None)?;
                    if let V::Err(e) = v {
                        return Ok(Obs::Error(e));
                    }
                    vals.push(v);
                }
                Ok(Obs::Rows(vec![vals], true))
            }
            Probe::Stmt(s) => self.stmt(s),
        }?;
        Ok(r)
    }

    fn stmt(&mut self, s: &Stmt) -> R<Obs> {
        match s {
            Stmt::Select(q) => {
                let assigns: Vec<bool> = q.items.iter().map(|i| i.assign.is_some()).collect();
                let r = self.query(q, &[], false)?;
                if let Some(e) = first_error(&r.rows) {
                    return Ok(Obs::Error(e));
                }
                if assigns.iter().any(|a| *a) || !q.into.is_empty() {
                    let width = r.rows.first().map(Vec::len).unwrap_or(q.items.len());
                    return match r.rows.as_slice() {
                        // T-SQL keeps the variables; PL/pgSQL SELECT INTO sets NULL.
                        [] if self.d == Dialect::Tsql => Ok(Obs::Assigned(vec![None; width])),
                        [] => Ok(Obs::Assigned(vec![Some(V::Null); width])),
                        [row] => Ok(Obs::Assigned(row.iter().cloned().map(Some).collect())),
                        rows => {
                            // Several rows: T-SQL assigns from each (the last one wins),
                            // PL/pgSQL takes the first; only certain when they agree.
                            let first = &rows[0];
                            if rows.iter().all(|r| rows_same(r, first) == Some(true)) || r.ordered {
                                let pick = if self.d == Dialect::Tsql {
                                    rows.last().unwrap()
                                } else {
                                    first
                                };
                                Ok(Obs::Assigned(pick.iter().cloned().map(Some).collect()))
                            } else {
                                Err(Stop::Reject)
                            }
                        }
                    };
                }
                Ok(Obs::Rows(r.rows, r.ordered))
            }
            Stmt::Assign(t, e) => {
                let v = self.expr(e, &[], None)?;
                // A string longer than the variable's declared length is cut or rejected.
                let name = match t {
                    Expr::Var(n) => Some(var_key(n)),
                    Expr::Name(p) if p.len() == 1 => Some(p[0].to_lowercase()),
                    _ => None,
                };
                if let (V::Str(s), Some(ty)) = (&v, name.and_then(|n| self.vars.get(&n)?.ty)) {
                    if ty.is_string() && ty.len.is_some_and(|l| s.chars().count() as u32 > l) {
                        return Err(Stop::Reject);
                    }
                }
                Ok(match v {
                    V::Err(e) => Obs::Error(e),
                    v => Obs::Value(v),
                })
            }
            Stmt::Return(Some(e)) => {
                let v = self.expr(e, &[], None)?;
                Ok(match v {
                    V::Err(e) => Obs::Error(e),
                    v => Obs::Value(v),
                })
            }
            Stmt::Declare { default, ty, .. } => {
                let Some(e) = default else {
                    return Ok(Obs::Value(V::Null));
                };
                let v = self.expr(e, &[], None)?;
                let v = match (ty, v) {
                    (Some(t), V::Str(s)) => {
                        let ty = Ty::from_type(t, self.d, false);
                        match ty.len {
                            Some(l) if ty.is_string() && s.chars().count() as u32 > l => {
                                return Err(Stop::Reject)
                            }
                            _ => V::Str(s),
                        }
                    }
                    (_, v) => v,
                };
                Ok(match v {
                    V::Err(e) => Obs::Error(e),
                    v => Obs::Value(v),
                })
            }
            Stmt::Return(None) => Ok(Obs::Value(V::Null)),
            Stmt::Insert {
                source,
                returning,
                on_conflict,
                ..
            } => {
                if !returning.is_empty() || on_conflict.is_some() {
                    return unsupported("INSERT RETURNING / ON CONFLICT");
                }
                let rows = match source {
                    InsertSource::Values(rows) => {
                        let mut out = Vec::new();
                        for r in rows {
                            let mut vals = Vec::new();
                            for e in r {
                                vals.push(self.expr(e, &[], None)?);
                            }
                            out.push(vals);
                        }
                        out
                    }
                    InsertSource::Query(q) => self.query(q, &[], false)?.rows,
                    InsertSource::Default => vec![Vec::new()],
                };
                if let Some(e) = first_error(&rows) {
                    return Ok(Obs::Error(e));
                }
                Ok(Obs::Rows(rows, false))
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => self.update(target, alias.as_deref(), Some(sets), from, where_.as_ref()),
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => self.update(target, alias.as_deref(), None, from, where_.as_ref()),
            Stmt::Merge { .. } => unsupported("MERGE"),
        }
    }

    /// UPDATE (with `sets`) or DELETE: which target rows are written, and
    /// the new values.
    fn update(
        &mut self,
        target: &[String],
        alias: Option<&str>,
        sets: Option<&Vec<(Vec<String>, super::ast::Expr)>>,
        from: &[From],
        where_: Option<&Expr>,
    ) -> R<Obs> {
        let mut key = table_key(target);
        // T-SQL `UPDATE alias SET ... FROM t AS alias`: the target is the aliased table.
        let mut target_alias: Option<String> = alias.map(|a| a.to_lowercase());
        if !from.is_empty() && self.d == Dialect::Tsql && target.len() == 1 {
            let t = key.clone();
            for (names, table) in from.iter().flat_map(from_names) {
                if names.contains(&t) {
                    if let Some(table) = table {
                        if names.len() == 1 && names[0] != table {
                            key = table;
                            target_alias = Some(t.clone());
                        }
                    }
                }
            }
        }
        let n = self.rows(&key);
        let target_names = match &target_alias {
            Some(a) => vec![a.clone()],
            None => vec![key.clone(), last_part(&key).to_string()],
        };
        let joined_in_from = !from.is_empty()
            && from.iter().flat_map(from_names).any(|(names, table)| {
                table.as_deref() == Some(key.as_str())
                    && names.iter().any(|x| target_names.contains(x))
            });
        let mut writes = Vec::new();
        for i in 0..n {
            let tb = Binding {
                names: target_names.clone(),
                table: Some(key.clone()),
                src: Src::Base(key.clone(), i),
            };
            // FROM rows (T-SQL UPDATE ... FROM, PostgreSQL UPDATE ... FROM / DELETE ... USING).
            let mut product: Vec<Scope> = vec![Vec::new()];
            for f in from {
                let rows = self.from_item(f, &[])?;
                let mut next = Vec::new();
                for p in &product {
                    for r in &rows {
                        let mut x = p.clone();
                        x.extend(r.iter().cloned());
                        next.push(x);
                    }
                }
                product = next;
            }
            let mut matches = Vec::new();
            for p in product {
                let mut row: Scope = if joined_in_from {
                    // The target appears in FROM: keep only rows that join to row i.
                    if !p
                        .iter()
                        .any(|b| matches!(&b.src, Src::Base(t, r) if *t == key && *r == i))
                    {
                        continue;
                    }
                    p
                } else {
                    let mut r = vec![tb.clone()];
                    r.extend(p);
                    r
                };
                if joined_in_from {
                    for b in row.iter_mut() {
                        if matches!(&b.src, Src::Base(t, _) if *t == key) {
                            b.names = target_names.clone();
                        }
                    }
                }
                let ok = match where_ {
                    None => true,
                    Some(w) => {
                        let v = self.expr(w, &[row.clone()], None)?;
                        if let V::Err(e) = v {
                            return Ok(Obs::Error(e));
                        }
                        truth(&v)? == Some(true)
                    }
                };
                if ok {
                    matches.push(row);
                }
            }
            match matches.as_slice() {
                [] => {}
                [row] => {
                    let mut vals = Vec::new();
                    if let Some(sets) = sets {
                        for (cols, e) in sets {
                            if cols.len() != 1 {
                                return unsupported("multi-column SET");
                            }
                            let v = self.expr(e, &[row.clone()], None)?;
                            if let V::Err(e) = v {
                                return Ok(Obs::Error(e));
                            }
                            vals.push(v);
                        }
                    }
                    writes.push((i, vals));
                }
                // Several source rows for one target row: the value is unspecified.
                _ => return Err(Stop::Reject),
            }
        }
        Ok(Obs::Writes(writes))
    }
}

fn first_error(rows: &[Vec<V>]) -> Option<String> {
    rows.iter().flatten().find_map(|v| match v {
        V::Err(e) => Some(e.clone()),
        _ => None,
    })
}

// ---- search -----------------------------------------------------------------------

/// A found witness: input values and both observations.
#[derive(Debug, Clone)]
pub struct Found {
    pub inputs: Vec<(Key, V)>,
    pub a: Obs,
    pub b: Obs,
    /// Some input is a local value whose occurrence is assumed.
    pub assumed: bool,
    pub opaque: Vec<String>,
    pub shape: HashMap<String, usize>,
    /// Some guard on the way to the statement could not be evaluated.
    pub unreached: bool,
}

/// Why no witness was found.
#[derive(Debug, Clone, PartialEq)]
pub enum Miss {
    Unsupported(String),
    /// Every assignment tried agreed (or was ambiguous).
    NoWitness,
}

const BUDGET: usize = 20_000;

/// Searches for inputs under which `a` and `b` observably differ. `make`
/// builds a fresh context (facts, hints, catalog) for each world shape.
pub fn search<'s>(make: &dyn Fn() -> Ctx<'s>, a: &Probe, b: &Probe) -> Result<Found, Miss> {
    // World shapes: one row per table first, then each table empty, then
    // each table with two rows.
    // Table functions and temporal reads must be the same reads on both
    // sides: otherwise their rows would be chosen independently.
    let (ta, tb) = (probe_tables(a), probe_tables(b));
    for t in ta
        .iter()
        .filter(|t| !tb.contains(t))
        .chain(tb.iter().filter(|t| !ta.contains(t)))
    {
        if t.contains('(') || t.contains(" system_time ") {
            return Err(Miss::Unsupported(format!(
                "{t} is read by one version only"
            )));
        }
    }
    let mut tables = Vec::new();
    let guards = make().guards;
    let guard_probe = Probe::Conds(guards.iter().flatten().map(|g| g.0.clone()).collect());
    for p in [a, b, &guard_probe] {
        for t in probe_tables(p) {
            if !tables.contains(&t) {
                tables.push(t);
            }
        }
    }
    let mut shapes: Vec<HashMap<String, usize>> = vec![HashMap::new()];
    for t in &tables {
        shapes.push(HashMap::from([(t.clone(), 0)]));
    }
    for t in &tables {
        shapes.push(HashMap::from([(t.clone(), 2)]));
    }
    if tables.len() >= 2 {
        shapes.push(tables.iter().map(|t| (t.clone(), 2)).collect());
    }
    let mut budget = BUDGET;
    let mut last_miss = Miss::NoWitness;
    for shape in shapes {
        let mut c = make();
        c.shape = shape.clone();
        match dfs(&mut c, a, b, &mut budget) {
            Ok(Some(mut f)) => {
                f.shape = shape;
                return Ok(f);
            }
            Ok(None) => {}
            Err(m) => {
                last_miss = m;
                if matches!(last_miss, Miss::Unsupported(_)) {
                    return Err(last_miss);
                }
            }
        }
        if budget == 0 {
            break;
        }
    }
    Err(last_miss)
}

fn eval_side(c: &mut Ctx, p: &Probe, side: u8) -> R<Obs> {
    c.side = side;
    c.observe(p)
}

fn dfs(c: &mut Ctx, a: &Probe, b: &Probe, budget: &mut usize) -> Result<Option<Found>, Miss> {
    if *budget == 0 {
        return Ok(None);
    }
    *budget -= 1;
    c.guard_skipped = false;
    let ra = eval_side(c, a, 1);
    let rb = match &ra {
        Ok(_) => eval_side(c, b, 2),
        Err(e) => Err(e.clone()),
    };
    if std::env::var_os("SQLDIFF_DEBUG_SEARCH").is_some() {
        eprintln!(
            "  shape {:?} values {:?}\n    A {:?}\n    B {:?}",
            c.shape, c.values, ra, rb
        );
    }
    let need = match (&ra, &rb) {
        (Ok(x), Ok(y)) => {
            return Ok(match differ(x, y) {
                Some(true) => Some(Found {
                    inputs: c
                        .inputs
                        .iter()
                        .zip(&c.values)
                        .filter_map(|(i, v)| v.clone().map(|v| (i.key.clone(), v)))
                        .collect(),
                    a: x.clone(),
                    b: y.clone(),
                    assumed: c
                        .inputs
                        .iter()
                        .zip(&c.values)
                        .any(|(i, v)| v.is_some() && i.origin == Origin::Assumed),
                    opaque: c.opaque.clone(),
                    shape: HashMap::new(),
                    unreached: c.guard_skipped,
                }),
                _ => None,
            })
        }
        (Err(Stop::Need(i)), _) | (_, Err(Stop::Need(i))) => *i,
        (Err(Stop::Reject), _) | (_, Err(Stop::Reject)) => return Ok(None),
        (Err(Stop::Unsupported(m)), _) | (_, Err(Stop::Unsupported(m))) => {
            return Err(Miss::Unsupported(m.clone()))
        }
    };
    let cands = c.inputs[need].cands.clone();
    for v in cands {
        c.values[need] = Some(v);
        let n_inputs = c.inputs.len();
        if let Some(f) = dfs(c, a, b, budget)? {
            return Ok(Some(f));
        }
        // Forget inputs created below this choice.
        for j in n_inputs..c.inputs.len() {
            let k = c.inputs[j].key.clone();
            c.index.remove(&k);
        }
        c.inputs.truncate(n_inputs);
        c.values.truncate(n_inputs);
        if *budget == 0 {
            break;
        }
    }
    c.values[need] = None;
    Ok(None)
}

/// Base tables a probe reads or writes.
pub fn probe_tables(p: &Probe) -> Vec<String> {
    let mut out = Vec::new();
    match p {
        Probe::Stmt(s) => stmt_tables(s, &mut out),
        Probe::Conds(es) | Probe::Values(es) => {
            for e in es {
                expr_tables(e, &mut out);
            }
        }
    }
    out
}

fn push_table(out: &mut Vec<String>, t: String) {
    if !out.contains(&t) {
        out.push(t);
    }
}

fn stmt_tables(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Select(q) => query_tables(q, out),
        Stmt::Insert { source, .. } => match source {
            InsertSource::Values(rows) => {
                for e in rows.iter().flatten() {
                    expr_tables(e, out);
                }
            }
            InsertSource::Query(q) => query_tables(q, out),
            InsertSource::Default => {}
        },
        Stmt::Update {
            target,
            sets,
            from,
            where_,
            ..
        } => {
            push_table(out, table_key(target));
            for f in from {
                from_tables(f, out);
            }
            for (_, e) in sets {
                expr_tables(e, out);
            }
            if let Some(w) = where_ {
                expr_tables(w, out);
            }
        }
        Stmt::Delete {
            target,
            from,
            where_,
            ..
        } => {
            push_table(out, table_key(target));
            for f in from {
                from_tables(f, out);
            }
            if let Some(w) = where_ {
                expr_tables(w, out);
            }
        }
        Stmt::Assign(_, e) | Stmt::Return(Some(e)) => expr_tables(e, out),
        Stmt::Declare {
            default: Some(e), ..
        } => expr_tables(e, out),
        _ => {}
    }
}

fn query_tables(q: &Select, out: &mut Vec<String>) {
    let mut inner = Vec::new();
    for c in &q.ctes {
        query_tables(&c.query, &mut inner);
    }
    for f in &q.from {
        from_tables(f, &mut inner);
    }
    for i in &q.items {
        expr_tables(&i.expr, &mut inner);
    }
    for e in q.where_.iter().chain(q.having.iter()) {
        expr_tables(e, &mut inner);
    }
    for (_, o) in &q.set_ops {
        query_tables(o, &mut inner);
    }
    for t in inner {
        if !q.ctes.iter().any(|c| c.name.eq_ignore_ascii_case(&t)) {
            push_table(out, t);
        }
    }
}

fn from_tables(f: &From, out: &mut Vec<String>) {
    match f {
        From::Table { name, hints, .. } => push_table(out, table_key_with(name, hints)),
        From::Func { name, args, .. } => push_table(out, func_key(name, args)),
        From::Sub { query, .. } => query_tables(query, out),
        From::Join {
            left, right, on, ..
        } => {
            from_tables(left, out);
            from_tables(right, out);
            if let Some(o) = on {
                expr_tables(o, out);
            }
        }
        _ => {}
    }
}

fn expr_tables(e: &Expr, out: &mut Vec<String>) {
    walk_expr(e, &mut |x| match x {
        Expr::Exists(q) | Expr::Subquery(q) | Expr::InQuery(_, q, _) => query_tables(q, out),
        _ => {}
    });
}
