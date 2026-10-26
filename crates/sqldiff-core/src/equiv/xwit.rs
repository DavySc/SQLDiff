//! Witnesses for statement pairs across dialects, found on the canonical
//! terms (`canon`).
//!
//! Two statements whose terms differ only where no rule in `compare`
//! explains the difference (a changed comparison operator, a dropped or
//! changed WHERE, a different parameter, literal or column) are run on a
//! small database: one table of one or two rows plus the parameter values.
//! A witness is an assignment under which both statements finish and their
//! observable results differ: the rows a DELETE or UPDATE leaves, the row an
//! INSERT adds, the rows a query returns or a returned value.
//!
//! The terms are dialect-neutral: every place where the dialects behave
//! differently is an explicit part of the term. The evaluator only models
//! terms whose meaning is the same in every dialect for the values it uses
//! (integers, NULL, short ASCII strings without blanks, opaque values that
//! are only copied), and gives up on anything else, so the search never
//! claims more than it can show. Values are chosen within the parameter and
//! catalog types; what it cannot know without a catalog (column types,
//! constraints, triggers) is stated as an assumption.

use std::collections::{BTreeMap, BTreeSet};

use super::lower::Unit;
use super::schema::{Fam, Schema, Ty};
use super::term::Term;
use crate::model::Assumption;

pub fn witness_data() -> Assumption {
    Assumption::default(
        "ASM-WITNESS-DATA",
        "The tables can hold the witness rows: the columns it sets are ordinary columns that hold the numbers, strings or NULLs it gives them, and no constraint or trigger rejects or replaces the write",
    )
}

/// One side of the comparison.
pub struct Side<'u> {
    pub unit: &'u Unit,
    pub schema: &'u Schema,
    pub name: String,
}

pub struct Found {
    pub witness: String,
    pub assumptions: Vec<Assumption>,
}

type R<T> = Result<T, String>;

// ---- values ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum V {
    Null,
    Int(i64),
    Str(String),
    /// A value of a type the witness does not need to know; only copied.
    Tok(u8),
}

impl V {
    fn show(&self) -> String {
        match self {
            V::Null => "NULL".into(),
            V::Int(i) => i.to_string(),
            V::Str(s) => format!("'{s}'"),
            V::Tok(k) => format!("<value {k}>"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T3 {
    T,
    F,
    U,
}

impl T3 {
    fn not(self) -> T3 {
        match self {
            T3::T => T3::F,
            T3::F => T3::T,
            T3::U => T3::U,
        }
    }
}

/// Why an evaluation stopped.
enum Stop {
    /// The world makes a statement fail (division by zero, overflow):
    /// try another one.
    Fail,
    /// The term is outside what the evaluator models.
    Unsupported(String),
}

type E<T> = Result<T, Stop>;

fn unsupported<T>(s: impl Into<String>) -> E<T> {
    Err(Stop::Unsupported(s.into()))
}

/// Integers beyond this range might not fit the column or parameter type.
const LIMIT: i64 = 1 << 30;

// ---- slots and kinds ----------------------------------------------------

/// An input: a parameter (by position, from 1) or a column of the table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Slot {
    Param(usize),
    Col(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Any,
    Num,
    Str,
}

/// What the statements need from their inputs.
#[derive(Default)]
struct Need {
    table: Option<String>,
    /// The statement reads no table of its own, so a subquery's columns
    /// are the only ones in scope.
    scalar_ok: bool,
    kinds: BTreeMap<Slot, Kind>,
    /// Slots that must hold the same kind (a column set from an expression).
    links: Vec<(Slot, Slot)>,
    ints: BTreeSet<i64>,
    strs: BTreeSet<String>,
}

impl Need {
    fn unify(&mut self, s: Slot, k: Kind) -> R<()> {
        let cur = self.kinds.entry(s.clone()).or_insert(Kind::Any);
        match (*cur, k) {
            (_, Kind::Any) => Ok(()),
            (Kind::Any, k) => {
                *cur = k;
                Ok(())
            }
            (c, k) if c == k => Ok(()),
            _ => Err(format!("{s:?} is used both as a number and as a string")),
        }
    }

    fn slot(&self, t: &Term) -> R<Option<Slot>> {
        if t.is("sym") {
            let id = t.kid(0).and_then(Term::atom).unwrap_or("");
            return match id.strip_prefix('p').and_then(|x| x.parse::<usize>().ok()) {
                Some(i) if i > 0 => Ok(Some(Slot::Param(i))),
                _ => Err(format!("variable `{id}` is not a parameter")),
            };
        }
        if t.is("col") {
            let tab = t.kid(0).and_then(Term::atom).unwrap_or("?");
            let col = t.kid(1).and_then(Term::atom).unwrap_or("");
            if Some(tab) != self.table.as_deref() {
                return Err(format!("column of another table ({tab})"));
            }
            return Ok(Some(Slot::Col(col.to_string())));
        }
        Ok(None)
    }

    /// A value expression used as kind `k`.
    fn value(&mut self, t: &Term, k: Kind) -> R<()> {
        if let Some(s) = self.slot(t)? {
            return self.unify(s, k);
        }
        match (t.head(), t) {
            ("null", Term::A(_)) => Ok(()),
            ("num", _) => {
                if k == Kind::Str {
                    return Err("number used as a string".into());
                }
                let v = num(t).ok_or("non-integer number")?;
                self.ints.insert(v);
                Ok(())
            }
            ("str", _) => {
                if k == Kind::Num {
                    return Err("string used as a number".into());
                }
                let s = t.kid(0).and_then(Term::atom).unwrap_or("");
                if !plain_str(s) {
                    return Err("string literal with blanks or non-ASCII characters".into());
                }
                self.strs.insert(s.to_string());
                Ok(())
            }
            ("add" | "sub" | "mul", Term::N(_, kids)) if kids.len() == 2 && k != Kind::Str => {
                for x in kids {
                    self.value(x, Kind::Num)?;
                }
                Ok(())
            }
            ("neg", Term::N(_, kids)) if kids.len() == 1 && k != Kind::Str => {
                self.value(&kids[0], Kind::Num)
            }
            ("conv", Term::N(_, kids)) if kids.len() == 2 && k != Kind::Str => {
                conv_max(&kids[0]).ok_or("conversion to a non-numeric type")?;
                self.value(&kids[1], Kind::Num)
            }
            // A subquery value, only where no other table is in scope.
            ("scalar", Term::N(_, kids)) if kids.len() == 3 && self.scalar_ok => {
                if !matches!(kids[2].atom(), Some("null" | "error" | "unchanged")) {
                    return Err("subquery without rows".into());
                }
                let q = query(&kids[0], false)?;
                if q.items.len() != 1 {
                    return Err("subquery with several columns".into());
                }
                match &self.table {
                    Some(t) if *t != q.table => return Err("subqueries on two tables".into()),
                    _ => self.table = Some(q.table.clone()),
                }
                self.pred(q.filter)?;
                self.value(q.items[0], k)
            }
            (h, _) => Err(format!("`{h}` is not evaluated")),
        }
    }

    /// The kind an assigned value gives its column.
    fn assigned(&mut self, col: &str, v: &Term) -> R<()> {
        let c = Slot::Col(col.to_string());
        self.unify(c.clone(), Kind::Any)?;
        if let Some(s) = self.slot(v)? {
            self.unify(s.clone(), Kind::Any)?;
            self.links.push((c, s));
            return Ok(());
        }
        let k = match v.head() {
            "num" | "add" | "sub" | "mul" | "neg" => Kind::Num,
            "str" => Kind::Str,
            _ => Kind::Any,
        };
        self.unify(c, k)?;
        self.value(v, k)
    }

    fn pred(&mut self, t: &Term) -> R<()> {
        let kids = t.kids();
        match t.head() {
            "-" if t.atom().is_some() => Ok(()),
            "cmp" if kids.len() == 3 => {
                op(&kids[0])?;
                self.value(&kids[1], Kind::Num)?;
                self.value(&kids[2], Kind::Num)
            }
            "strcmp" if kids.len() == 5 => {
                let o = kids[0].atom().unwrap_or("");
                if o != "=" && o != "<>" {
                    return Err("string ordering comparison".into());
                }
                if !matches!(kids[1].atom(), Some("ci" | "cs")) {
                    return Err("string comparison collation".into());
                }
                if !matches!(kids[2].atom(), Some("pad" | "nopad" | "pad-mixed")) {
                    return Err("string comparison padding".into());
                }
                self.value(&kids[3], Kind::Str)?;
                self.value(&kids[4], Kind::Str)
            }
            "and" | "or" if !kids.is_empty() => kids.iter().try_for_each(|k| self.pred(k)),
            "not" if kids.len() == 1 => self.pred(&kids[0]),
            "isnull" if kids.len() == 2 => {
                flag(&kids[1])?;
                self.value(&kids[0], Kind::Any)
            }
            // Without a dialect tag: membership of numbers.
            "in" if kids.len() == 3 && kids[1].is("list") => {
                flag(&kids[2])?;
                self.value(&kids[0], Kind::Num)?;
                kids[1]
                    .kids()
                    .iter()
                    .try_for_each(|x| self.value(x, Kind::Num))
            }
            "between" if kids.len() == 4 => {
                flag(&kids[3])?;
                for x in &kids[..3] {
                    self.value(x, Kind::Num)?;
                }
                Ok(())
            }
            h => Err(format!("condition `{h}` is not evaluated")),
        }
    }

    fn propagate(&mut self) -> R<()> {
        loop {
            let mut changed = false;
            for (x, y) in self.links.clone() {
                let (kx, ky) = (self.kinds[&x], self.kinds[&y]);
                if kx != ky {
                    if kx == Kind::Any {
                        self.unify(x, ky)?;
                    } else {
                        self.unify(y, kx)?;
                    }
                    changed = true;
                }
            }
            if !changed {
                return Ok(());
            }
        }
    }
}

fn num(t: &Term) -> Option<i64> {
    let v: i64 = t.kid(0)?.atom()?.parse().ok()?;
    (v.abs() < LIMIT).then_some(v)
}

/// Short ASCII words with a letter: they compare the same under every
/// collation and padding rule, and are not dates or numbers in disguise.
fn plain_str(s: &str) -> bool {
    s.len() <= 30
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn op(t: &Term) -> R<&str> {
    match t.atom() {
        Some(o @ ("=" | "<>" | "<" | ">" | "<=" | ">=")) => Ok(o),
        _ => Err("comparison operator".into()),
    }
}

fn flag(t: &Term) -> R<bool> {
    match t.atom() {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err("flag".into()),
    }
}

// ---- statements ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum What {
    Delete,
    Update,
    Insert,
    Query,
    Return,
    Assign,
}

/// The parts of a statement the evaluator runs.
struct Stmt<'t> {
    what: What,
    table: Option<String>,
    filter: Option<&'t Term>,
    /// UPDATE SET / INSERT values, by column.
    sets: Vec<(String, &'t Term)>,
    /// Query items or the returned value.
    items: Vec<&'t Term>,
    distinct: bool,
    /// TOP / LIMIT: at most this many rows (the world keeps to fewer
    /// qualifying rows, so which ones are kept never matters).
    limit: Option<usize>,
    /// Assigned variable.
    target: Option<String>,
}

/// A query over one table: `(q distinct items from where group having order page lock)`.
struct Query<'t> {
    table: String,
    filter: &'t Term,
    items: Vec<&'t Term>,
    distinct: bool,
    limit: Option<usize>,
}

fn query(t: &Term, named_items: bool) -> R<Query<'_>> {
    let q = t.kids();
    if !t.is("q") || q.len() != 9 {
        return Err("query shape".into());
    }
    let distinct = match &q[0] {
        Term::A(s) if s == "all" => false,
        Term::A(s) if s == "distinct" => true,
        _ => return Err("DISTINCT under a collation".into()),
    };
    let from = q[2].kids();
    if !q[2].is("from") || from.len() != 1 || !from[0].is("t") {
        return Err("query over other than one table".into());
    }
    let none = |x: &Term| x.atom() == Some("-");
    if !q[4].is("group") || !q[4].kids().is_empty() || !none(&q[5]) {
        return Err("grouping".into());
    }
    let limit = if none(&q[7]) {
        None
    } else {
        let p = q[7].kids();
        let zero = p.first().is_some_and(|o| o.is("num") && num(o) == Some(0));
        match p.get(1) {
            Some(l) if zero && l.is("num") => match num(l) {
                Some(n) if n >= 1 => Some(n as usize),
                _ => return Err("paging".into()),
            },
            _ => return Err("paging".into()),
        }
    };
    let mut items = Vec::new();
    for it in q[1].kids() {
        if named_items {
            if !it.is("item") || it.kids().len() != 2 {
                return Err("select item".into());
            }
            items.push(&it.kids()[0]);
        } else {
            items.push(it);
        }
    }
    Ok(Query {
        table: table_atom(from[0].kid(0))?,
        filter: &q[3],
        items,
        distinct,
        limit,
    })
}

/// Largest value of a numeric conversion target `(ty Fam len scale)`.
fn conv_max(t: &Term) -> Option<i64> {
    if !t.is("ty") {
        return None;
    }
    let k = t.kids();
    let fam = match k.first()?.atom()? {
        "Int" => Fam::Int,
        "Exact" => Fam::Exact,
        "Float" => Fam::Float,
        _ => return None,
    };
    let n = |i: usize| k.get(i).and_then(Term::atom).and_then(|x| x.parse().ok());
    int_max(&Ty {
        fam,
        len: n(1),
        scale: n(2),
    })
}

fn table_atom(t: Option<&Term>) -> R<String> {
    let s = t.and_then(Term::atom).ok_or("table")?;
    if s.contains(':') || s.contains('#') || s.contains('@') || s == "?" {
        return Err(format!("table `{s}` is not a plain table"));
    }
    Ok(s.to_string())
}

fn kv_list(set: Option<&Term>) -> R<Vec<(String, &Term)>> {
    let set = set.filter(|s| s.is("set")).ok_or("SET list")?;
    set.kids()
        .iter()
        .map(|kv| {
            if !kv.is("kv") || kv.kids().len() != 2 {
                return Err("assignment of several columns".to_string());
            }
            Ok((
                kv.kids()[0].atom().ok_or("column")?.to_string(),
                &kv.kids()[1],
            ))
        })
        .collect()
}

fn statement(t: &Term) -> R<Stmt<'_>> {
    let k = t.kids();
    let base = |what| Stmt {
        what,
        table: None,
        filter: None,
        sets: Vec::new(),
        items: Vec::new(),
        distinct: false,
        limit: None,
        target: None,
    };
    match t.head() {
        "delete" if k.len() == 2 => Ok(Stmt {
            table: Some(table_atom(k.first())?),
            filter: Some(&k[1]),
            ..base(What::Delete)
        }),
        "update" if k.len() == 3 => Ok(Stmt {
            table: Some(table_atom(k.first())?),
            sets: kv_list(k.get(1))?,
            filter: Some(&k[2]),
            ..base(What::Update)
        }),
        "insert1" if k.len() == 2 => Ok(Stmt {
            table: Some(table_atom(k.first())?),
            sets: kv_list(k.get(1))?,
            ..base(What::Insert)
        }),
        "return" if k.len() == 1 => Ok(Stmt {
            items: vec![&k[0]],
            ..base(What::Return)
        }),
        "resultset" if k.len() == 1 => {
            let q = query(&k[0], true)?;
            Ok(Stmt {
                table: Some(q.table),
                filter: Some(q.filter),
                items: q.items,
                distinct: q.distinct,
                limit: q.limit,
                ..base(What::Query)
            })
        }
        // Only an OUT parameter's value is seen by the caller (`search`
        // checks that nothing later reads or sets it).
        "assign" if k.len() == 2 && k[0].is("sym") => Ok(Stmt {
            target: k[0].kid(0).and_then(Term::atom).map(str::to_string),
            items: vec![&k[1]],
            ..base(What::Assign)
        }),
        h => Err(format!("`{h}` statements are not evaluated")),
    }
}

// ---- evaluation ---------------------------------------------------------

struct World<'w> {
    params: &'w BTreeMap<usize, V>,
    rows: &'w [BTreeMap<String, V>],
}

impl World<'_> {
    fn value(&self, t: &Term, row: &BTreeMap<String, V>) -> E<V> {
        if t.is("sym") {
            let i: usize = t.kids()[0].atom().unwrap_or("")[1..].parse().unwrap_or(0);
            return self
                .params
                .get(&i)
                .cloned()
                .map_or_else(|| unsupported("param"), Ok);
        }
        if t.is("col") {
            let c = t.kids()[1].atom().unwrap_or("");
            return row
                .get(c)
                .cloned()
                .map_or_else(|| unsupported("column"), Ok);
        }
        match t.head() {
            "null" => Ok(V::Null),
            "num" => num(t).map(V::Int).map_or_else(|| unsupported("number"), Ok),
            "str" => Ok(V::Str(t.kids()[0].atom().unwrap_or("").to_string())),
            "neg" => match self.value(&t.kids()[0], row)? {
                V::Int(x) => Ok(V::Int(-x)),
                V::Null => Ok(V::Null),
                _ => unsupported("negation"),
            },
            "conv" => match self.value(&t.kids()[1], row)? {
                V::Int(x) if x.abs() <= conv_max(&t.kids()[0]).unwrap_or(0) => Ok(V::Int(x)),
                V::Int(_) => Err(Stop::Fail),
                V::Null => Ok(V::Null),
                _ => unsupported("conversion"),
            },
            "scalar" => {
                let q = query(&t.kids()[0], false).map_err(Stop::Unsupported)?;
                let mut hits = Vec::new();
                for r in self.rows {
                    if self.pred(q.filter, r)? == T3::T {
                        hits.push(self.value(q.items[0], r)?);
                    }
                }
                match (hits.len(), t.kids()[2].atom()) {
                    (1, _) => Ok(hits.pop().unwrap_or(V::Null)),
                    (0, Some("null")) => Ok(V::Null),
                    // An error, the variable's earlier value, or one of
                    // several rows: not compared.
                    _ => Err(Stop::Fail),
                }
            }
            h @ ("add" | "sub" | "mul") => {
                let (x, y) = (
                    self.value(&t.kids()[0], row)?,
                    self.value(&t.kids()[1], row)?,
                );
                match (x, y) {
                    (V::Int(x), V::Int(y)) => {
                        let r = match h {
                            "add" => x.checked_add(y),
                            "sub" => x.checked_sub(y),
                            _ => x.checked_mul(y),
                        };
                        match r {
                            Some(r) if r.abs() < LIMIT => Ok(V::Int(r)),
                            _ => Err(Stop::Fail),
                        }
                    }
                    (V::Null, _) | (_, V::Null) => Ok(V::Null),
                    _ => unsupported("arithmetic"),
                }
            }
            h => unsupported(format!("`{h}`")),
        }
    }

    fn cmp(o: &str, x: &V, y: &V) -> E<T3> {
        let ord = match (x, y) {
            (V::Null, _) | (_, V::Null) => return Ok(T3::U),
            (V::Int(a), V::Int(b)) => a.cmp(b),
            _ => return unsupported("comparison of non-numbers"),
        };
        let r = match o {
            "=" => ord.is_eq(),
            "<>" => ord.is_ne(),
            "<" => ord.is_lt(),
            ">" => ord.is_gt(),
            "<=" => ord.is_le(),
            _ => ord.is_ge(),
        };
        Ok(if r { T3::T } else { T3::F })
    }

    fn pred(&self, t: &Term, row: &BTreeMap<String, V>) -> E<T3> {
        let k = t.kids();
        match t.head() {
            "-" => Ok(T3::T),
            "cmp" => {
                let (x, y) = (self.value(&k[1], row)?, self.value(&k[2], row)?);
                Self::cmp(k[0].atom().unwrap_or(""), &x, &y)
            }
            "strcmp" => {
                let (x, y) = (self.value(&k[3], row)?, self.value(&k[4], row)?);
                let eq = match (&x, &y) {
                    (V::Null, _) | (_, V::Null) => return Ok(T3::U),
                    // No blanks in the values, so padding does not matter.
                    (V::Str(a), V::Str(b)) => match k[1].atom() {
                        Some("ci") => a.eq_ignore_ascii_case(b),
                        _ => a == b,
                    },
                    _ => return unsupported("string comparison of non-strings"),
                };
                let r = if k[0].atom() == Some("=") { eq } else { !eq };
                Ok(if r { T3::T } else { T3::F })
            }
            "and" => {
                let mut r = T3::T;
                for x in k {
                    match self.pred(x, row)? {
                        T3::F => return Ok(T3::F),
                        T3::U => r = T3::U,
                        T3::T => {}
                    }
                }
                Ok(r)
            }
            "or" => {
                let mut r = T3::F;
                for x in k {
                    match self.pred(x, row)? {
                        T3::T => return Ok(T3::T),
                        T3::U => r = T3::U,
                        T3::F => {}
                    }
                }
                Ok(r)
            }
            "not" => Ok(self.pred(&k[0], row)?.not()),
            "isnull" => {
                let null = self.value(&k[0], row)? == V::Null;
                let neg = k[1].atom() == Some("true");
                Ok(if null != neg { T3::T } else { T3::F })
            }
            "in" => {
                let x = self.value(&k[0], row)?;
                let mut r = T3::F;
                for it in k[1].kids() {
                    match Self::cmp("=", &x, &self.value(it, row)?)? {
                        T3::T => {
                            r = T3::T;
                            break;
                        }
                        T3::U => r = T3::U,
                        T3::F => {}
                    }
                }
                Ok(if k[2].atom() == Some("true") {
                    r.not()
                } else {
                    r
                })
            }
            "between" => {
                let x = self.value(&k[0], row)?;
                let lo = Self::cmp(">=", &x, &self.value(&k[1], row)?)?;
                let hi = Self::cmp("<=", &x, &self.value(&k[2], row)?)?;
                let r = match (lo, hi) {
                    (T3::F, _) | (_, T3::F) => T3::F,
                    (T3::T, T3::T) => T3::T,
                    _ => T3::U,
                };
                Ok(if k[3].atom() == Some("true") {
                    r.not()
                } else {
                    r
                })
            }
            h => unsupported(format!("condition `{h}`")),
        }
    }

    fn run(&self, s: &Stmt) -> E<Obs> {
        let rows = self.rows;
        let keep = |row: &BTreeMap<String, V>| -> E<bool> {
            Ok(match s.filter {
                Some(f) => self.pred(f, row)? == T3::T,
                None => true,
            })
        };
        match s.what {
            What::Delete => {
                let mut left = Vec::new();
                for r in rows {
                    if !keep(r)? {
                        left.push(tuple(r));
                    }
                }
                left.sort();
                Ok(Obs::Rows(left))
            }
            What::Update => {
                let mut out = Vec::new();
                for r in rows {
                    let mut n = r.clone();
                    if keep(r)? {
                        for (c, v) in &s.sets {
                            n.insert(c.clone(), self.value(v, r)?);
                        }
                    }
                    out.push(tuple(&n));
                }
                out.sort();
                Ok(Obs::Rows(out))
            }
            What::Insert => {
                let empty = BTreeMap::new();
                let mut row = Vec::new();
                for (c, v) in &s.sets {
                    row.push((c.clone(), self.value(v, &empty)?));
                }
                row.sort();
                Ok(Obs::Row(row))
            }
            What::Query => {
                let mut out = Vec::new();
                for r in rows {
                    if keep(r)? {
                        let mut t = Vec::new();
                        for it in &s.items {
                            t.push(self.value(it, r)?);
                        }
                        out.push(t);
                    }
                }
                out.sort();
                if s.distinct {
                    out.dedup();
                }
                if s.limit.is_some_and(|l| out.len() > l) {
                    return Err(Stop::Fail);
                }
                Ok(Obs::Rows(out))
            }
            What::Return | What::Assign => {
                Ok(Obs::Value(self.value(s.items[0], &BTreeMap::new())?))
            }
        }
    }
}

fn tuple(r: &BTreeMap<String, V>) -> Vec<V> {
    r.values().cloned().collect()
}

#[derive(Debug, PartialEq)]
enum Obs {
    /// Rows left in the table, or rows returned (sorted).
    Rows(Vec<Vec<V>>),
    /// The inserted row, by column.
    Row(Vec<(String, V)>),
    Value(V),
}

// ---- domains ------------------------------------------------------------

/// Largest integer a type surely holds, or None when the type holds no
/// integers the witness can use.
fn int_max(t: &Ty) -> Option<i64> {
    match t.fam {
        Fam::Int => Some(match t.len {
            Some(1) => 1,
            Some(b) if b <= 8 => 127,
            Some(b) if b <= 16 => 32767,
            _ => LIMIT - 1,
        }),
        Fam::Exact => match (t.len, t.scale) {
            (None, _) => Some(LIMIT - 1),
            (Some(p), s) => {
                let digits = p.saturating_sub(s.unwrap_or(0)).min(9);
                if digits == 0 {
                    None
                } else {
                    Some(10i64.pow(digits) - 1)
                }
            }
        },
        Fam::Float => Some(LIMIT - 1),
        _ => None,
    }
}

/// The types an input has on the two sides; None where unknown.
fn types(sides: [&Side; 2], s: &Slot, table: Option<&str>) -> R<[Option<Ty>; 2]> {
    let mut out = [None, None];
    for (i, side) in sides.iter().enumerate() {
        out[i] = match s {
            Slot::Param(p) => Some(
                side.unit
                    .params
                    .get(p - 1)
                    .ok_or("parameter missing on one side")?
                    .ty
                    .ok_or("parameter of unknown type")?,
            ),
            Slot::Col(c) => table
                .map(|t| t.rsplit_once('.').map_or(t, |x| x.1))
                .and_then(|t| side.schema.table(t))
                .and_then(|t| t.column(c))
                .map(|c| c.ty),
        };
    }
    Ok(out)
}

fn not_null(sides: [&Side; 2], c: &str, table: Option<&str>) -> bool {
    let Some(t) = table.map(|t| t.rsplit_once('.').map_or(t, |x| x.1)) else {
        return false;
    };
    sides.iter().any(|s| {
        s.schema
            .table(t)
            .and_then(|t| t.column(c))
            .is_some_and(|c| c.not_null)
    })
}

/// Candidate values of one input, most useful first; NULL last.
fn domain(need: &Need, s: &Slot, kind: Kind, tys: &[Option<Ty>; 2], nullable: bool) -> R<Vec<V>> {
    let mut v = Vec::new();
    match kind {
        Kind::Num => {
            let mut max = LIMIT - 1;
            for t in tys.iter().flatten() {
                max = max.min(int_max(t).ok_or_else(|| format!("{s:?} is not numeric"))?);
            }
            let mut c: Vec<i64> = vec![0, 1, 2];
            for &k in &need.ints {
                c.extend([k, k + 1, k - 1]);
            }
            for x in c {
                let min = if need.ints.contains(&x) { -max } else { 0 };
                if (min..=max).contains(&x) && !v.contains(&V::Int(x)) {
                    v.push(V::Int(x));
                }
            }
            v.truncate(8);
        }
        Kind::Str => {
            let mut max = usize::MAX;
            for t in tys.iter().flatten() {
                if !t.is_string() {
                    return Err(format!("{s:?} is not a string"));
                }
                if let Some(l) = t.len {
                    max = max.min(l as usize);
                }
            }
            let mut c: Vec<String> = vec!["a".into(), "b".into()];
            c.extend(need.strs.iter().cloned());
            for x in c {
                // A CHAR column pads shorter values; keep to values that fit.
                if x.len() <= max && !v.contains(&V::Str(x.clone())) {
                    v.push(V::Str(x));
                }
            }
            v.truncate(6);
        }
        Kind::Any => v.extend([V::Tok(1), V::Tok(2)]),
    }
    if nullable {
        v.push(V::Null);
    }
    Ok(v)
}

/// The witness's world is the state at the statement and its result is the
/// routine's: no other statement touches its table (writes before it would
/// make the chosen rows unreachable, writes after it could undo or repeat
/// the difference) or assigns a parameter it reads, and every other
/// statement was lowered.
fn isolated(need: &Need, others: [&[&Term]; 2]) -> R<()> {
    let table = need.table.as_deref();
    let params: Vec<String> = need
        .kinds
        .keys()
        .filter_map(|s| match s {
            Slot::Param(i) => Some(format!("p{i}")),
            Slot::Col(_) => None,
        })
        .collect();
    for t in others.iter().flat_map(|l| l.iter()) {
        if t.is("raw") {
            return Err("another statement was not lowered".into());
        }
        if let Some(tab) = table {
            let prefix = format!("{tab}#");
            if t.any(&|x| x.atom().is_some_and(|s| s == tab || s.starts_with(&prefix))) {
                return Err(format!("another statement uses {tab}"));
            }
        }
        if t.any(&|x| {
            x.is("assign")
                && x.kid(0).is_some_and(|v| {
                    v.is("sym")
                        && v.kid(0)
                            .and_then(Term::atom)
                            .is_some_and(|id| params.iter().any(|p| p == id))
                })
        }) {
            return Err("another statement assigns a parameter the witness reads".into());
        }
    }
    Ok(())
}

// ---- search -------------------------------------------------------------

/// Candidate values per column.
type Domains = Vec<(String, Vec<V>)>;

const MAX_WORLDS: usize = 400_000;

/// Looks for inputs under which the two statements' results differ.
/// `others` holds every other statement of each routine; the caller only
/// searches when all of them are proven equal to their counterparts.
pub fn search(x: &Term, y: &Term, sa: &Side, sb: &Side, others: [&[&Term]; 2]) -> R<Found> {
    let a = statement(x)?;
    let b = statement(y)?;
    if a.what != b.what {
        return Err("different kinds of statement".into());
    }
    if a.what == What::Assign {
        let t = a.target.clone().unwrap_or_default();
        if b.target.as_deref() != Some(t.as_str()) {
            return Err("different variables assigned".into());
        }
        let out = |s: &Side| {
            t.strip_prefix('p')
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| s.unit.params.get(i.wrapping_sub(1)))
                .is_some_and(|p| p.out)
        };
        if !out(sa) || !out(sb) {
            return Err("assignment to other than an OUT parameter".into());
        }
        let sym = super::term::n("sym", vec![super::term::a(t.clone())]);
        if others
            .iter()
            .flat_map(|l| l.iter())
            .any(|x| x.any(&|y| *y == sym))
        {
            return Err(format!("{t} is used by another statement"));
        }
    }
    if a.table != b.table {
        return Err("different tables".into());
    }
    if a.what == What::Insert {
        let ca: Vec<&String> = a.sets.iter().map(|x| &x.0).collect();
        let cb: Vec<&String> = b.sets.iter().map(|x| &x.0).collect();
        if ca != cb {
            return Err("different inserted columns".into());
        }
    }
    if a.items.len() != b.items.len() {
        return Err("different number of result columns".into());
    }
    let mut need = Need {
        table: a.table.clone(),
        scalar_ok: a.table.is_none(),
        ..Need::default()
    };
    for s in [&a, &b] {
        if let Some(f) = s.filter {
            need.pred(f)?;
        }
        for (c, v) in &s.sets {
            match s.what {
                What::Update => need.assigned(c, v)?,
                // Inserted values: compared as they are.
                _ => {
                    let k = if v.is("str") { Kind::Str } else { Kind::Any };
                    need.value(v, k)?
                }
            }
        }
        for it in &s.items {
            let k = if it.is("str") { Kind::Str } else { Kind::Any };
            need.value(it, k)?;
        }
    }
    need.propagate()?;
    isolated(&need, others)?;
    let sides = [sa, sb];
    let table = need.table.clone();
    let mut params: Vec<(usize, Vec<V>)> = Vec::new();
    let mut cols: Vec<(String, Vec<V>)> = Vec::new();
    for (s, k) in &need.kinds {
        let tys = types(sides, s, table.as_deref())?;
        if let [Some(p), Some(q)] = tys {
            if p.is_string() != q.is_string() {
                return Err(format!("{s:?} has different types on the two sides"));
            }
        }
        match s {
            Slot::Param(i) => params.push((*i, domain(&need, s, *k, &tys, false)?)),
            Slot::Col(c) => {
                let nullable = !not_null(sides, c, table.as_deref());
                cols.push((c.clone(), domain(&need, s, *k, &tys, nullable)?));
            }
        }
    }
    let reads_rows = need.table.is_some() && a.what != What::Insert;
    let a = Stmt {
        table: need.table.clone(),
        ..a
    };
    let row_counts: &[usize] = if reads_rows { &[1, 2] } else { &[0] };
    // Worlds without NULL columns first, so a witness only rests on NULL
    // semantics when it has to.
    let non_null: Vec<(String, Vec<V>)> = cols
        .iter()
        .map(|(c, d)| {
            (
                c.clone(),
                d.iter().filter(|v| **v != V::Null).cloned().collect(),
            )
        })
        .collect();
    let with_nulls = cols.iter().any(|(_, d)| d.contains(&V::Null));
    let passes: Vec<(usize, &Domains)> = row_counts
        .iter()
        .map(|&n| (n, &non_null))
        .chain(
            row_counts
                .iter()
                .filter(|_| with_nulls)
                .map(|&n| (n, &cols)),
        )
        .collect();
    for (nrows, cols) in passes {
        let mut doms: Vec<&Vec<V>> = params.iter().map(|p| &p.1).collect();
        for _ in 0..nrows {
            doms.extend(cols.iter().map(|c| &c.1));
        }
        let total = doms
            .iter()
            .try_fold(1usize, |acc, d| acc.checked_mul(d.len()))
            .unwrap_or(usize::MAX);
        if total > MAX_WORLDS {
            if nrows <= 1 {
                return Err("too many inputs to search".into());
            }
            continue;
        }
        let mut idx = vec![0usize; doms.len()];
        loop {
            if let Some(f) = try_world(&a, &b, &params, cols, nrows, &doms, &idx, sa, sb)? {
                let mut assumptions = Vec::new();
                if reads_rows || a.what == What::Insert {
                    assumptions.push(witness_data());
                }
                return Ok(Found {
                    witness: f,
                    assumptions,
                });
            }
            // Next assignment.
            let mut k = 0;
            loop {
                if k == idx.len() {
                    break;
                }
                idx[k] += 1;
                if idx[k] < doms[k].len() {
                    break;
                }
                idx[k] = 0;
                k += 1;
            }
            if k == idx.len() {
                break;
            }
        }
    }
    Err("no witness found".into())
}

#[allow(clippy::too_many_arguments)]
fn try_world(
    a: &Stmt,
    b: &Stmt,
    params: &[(usize, Vec<V>)],
    cols: &[(String, Vec<V>)],
    nrows: usize,
    doms: &[&Vec<V>],
    idx: &[usize],
    sa: &Side,
    sb: &Side,
) -> R<Option<String>> {
    let pick = |k: usize| doms[k][idx[k]].clone();
    let pv: BTreeMap<usize, V> = params
        .iter()
        .enumerate()
        .map(|(k, (i, _))| (*i, pick(k)))
        .collect();
    let mut rows: Vec<BTreeMap<String, V>> = Vec::new();
    for r in 0..nrows {
        let base = params.len() + r * cols.len();
        rows.push(
            cols.iter()
                .enumerate()
                .map(|(k, (c, _))| (c.clone(), pick(base + k)))
                .collect(),
        );
    }
    // Two rows differ in every column and hold no NULL in the same column,
    // so no unique key over these columns is violated.
    if nrows == 2
        && cols
            .iter()
            .any(|(c, _)| rows[0][c] == rows[1][c] || rows[0][c] == V::Null)
    {
        return Ok(None);
    }
    let w = World {
        params: &pv,
        rows: &rows,
    };
    let (oa, ob) = match (w.run(a), w.run(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(Stop::Unsupported(e)), _) | (_, Err(Stop::Unsupported(e))) => return Err(e),
        _ => return Ok(None),
    };
    if oa == ob {
        return Ok(None);
    }
    Ok(Some(describe(a, &pv, cols, &rows, &oa, &ob, sa, sb)))
}

// ---- the witness in words ---------------------------------------------------

fn show_row(r: &BTreeMap<String, V>) -> String {
    let parts: Vec<String> = r
        .iter()
        .map(|(c, v)| format!("{c} = {}", v.show()))
        .collect();
    format!("({})", parts.join(", "))
}

fn show_tuple(t: &[V]) -> String {
    let parts: Vec<String> = t.iter().map(V::show).collect();
    format!("({})", parts.join(", "))
}

fn show_rows(rows: &[Vec<V>]) -> String {
    match rows.len() {
        0 => "no rows".into(),
        _ => rows
            .iter()
            .map(|r| show_tuple(r))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

#[allow(clippy::too_many_arguments)]
fn describe(
    s: &Stmt,
    params: &BTreeMap<usize, V>,
    cols: &[(String, Vec<V>)],
    rows: &[BTreeMap<String, V>],
    oa: &Obs,
    ob: &Obs,
    sa: &Side,
    sb: &Side,
) -> String {
    let mut inputs = Vec::new();
    for (i, v) in params {
        let name = |s: &Side| {
            s.unit
                .params
                .get(i - 1)
                .map(|p| p.name.clone())
                .unwrap_or_default()
        };
        let (na, nb) = (name(sa), name(sb));
        let names = if na == nb { na } else { format!("{na} / {nb}") };
        inputs.push(format!("parameter {i} ({names}) is {}", v.show()));
    }
    let table = s.table.clone().unwrap_or_default();
    if !rows.is_empty() {
        let shown: Vec<String> = rows.iter().map(show_row).collect();
        inputs.push(format!(
            "{table} holds {} {}",
            if rows.len() == 1 {
                "the row"
            } else {
                "the rows"
            },
            shown.join(" and ")
        ));
    }
    let when = if inputs.is_empty() {
        String::new()
    } else {
        format!("When {}, ", inputs.join(" and "))
    };
    let col_names: Vec<&str> = cols.iter().map(|c| c.0.as_str()).collect();
    let effect = match (oa, ob) {
        (Obs::Rows(x), Obs::Rows(y)) if s.what == What::Delete => {
            let gone = |left: &Vec<Vec<V>>| rows.len() - left.len();
            let (ga, gb) = (gone(x), gone(y));
            if ga != gb {
                format!(
                    "{} deletes {} and {} deletes {}",
                    sa.name,
                    count(ga),
                    sb.name,
                    count(gb)
                )
            } else {
                format!(
                    "the two sides delete different rows: {} keeps {} and {} keeps {} (columns {})",
                    sa.name,
                    show_rows(x),
                    sb.name,
                    show_rows(y),
                    col_names.join(", ")
                )
            }
        }
        (Obs::Rows(x), Obs::Rows(y)) if s.what == What::Update => format!(
            "afterwards {table} holds {} on {} and {} on {} (columns {})",
            show_rows(x),
            sa.name,
            show_rows(y),
            sb.name,
            col_names.join(", ")
        ),
        (Obs::Rows(x), Obs::Rows(y)) => format!(
            "{} returns {} and {} returns {}",
            sa.name,
            show_rows(x),
            sb.name,
            show_rows(y)
        ),
        (Obs::Row(x), Obs::Row(y)) => {
            let show = |r: &Vec<(String, V)>| {
                let p: Vec<String> = r
                    .iter()
                    .map(|(c, v)| format!("{c} = {}", v.show()))
                    .collect();
                format!("({})", p.join(", "))
            };
            format!(
                "{} inserts {} into {table} and {} inserts {}",
                sa.name,
                show(x),
                sb.name,
                show(y)
            )
        }
        (Obs::Value(x), Obs::Value(y)) => format!(
            "{} returns {} and {} returns {}",
            sa.name,
            x.show(),
            sb.name,
            y.show()
        ),
        _ => "the results differ".into(),
    };
    let mut out = format!("{when}{effect}");
    if let Some(c) = out.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    out
}

fn count(n: usize) -> String {
    match n {
        0 => "no row".into(),
        1 => "1 row".into(),
        n => format!("{n} rows"),
    }
}
