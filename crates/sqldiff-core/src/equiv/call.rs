//! Procedure calls and object comments compared across dialects: T-SQL
//! `EXEC` against PostgreSQL `CALL`, and T-SQL `sp_addextendedproperty`
//! against PostgreSQL `COMMENT ON`.
//!
//! Each statement is read completely or not at all: dynamic SQL, return
//! codes, `WITH` options, expressions T-SQL does not accept as arguments and
//! comment targets the model does not know leave the call not compared.
//!
//! A call is equal when the called procedure is found in both sides'
//! catalogs, takes as many parameters with the same modes, and every
//! parameter, taken by position, receives the same value on both sides:
//! arguments bound by position or by name, omitted ones replaced by the
//! parameter's default. Values are compared after the conversion to the
//! parameter type each side applies. The called procedure itself is the
//! conversion of the original, compared in the file that creates it
//! (`ASM-CALLEE-CONVERTED`).
//!
//! Facts used (checked on SQL Server 2022 and PostgreSQL 18, 2026-10-26;
//! `docs/cross-dialect-proofs.md`, "Calls and comments"):
//! - SQL Server converts an argument to the declared parameter type:
//!   `nvarchar(5)` truncates 'abcdefgh' to 'abcde', `decimal(5,2)` rounds
//!   1.005 to 1.01, `char(5)` pads with blanks, `int` truncates 1.7 to 1, and
//!   a bigint beyond the int range fails.
//! - PostgreSQL ignores a parameter type's length, precision and scale
//!   (`CREATE PROCEDURE`), so the same arguments arrive unchanged; it finds
//!   no procedure for a bigint argument to an int parameter, nor for an
//!   integer argument to a boolean one.
//! - A T-SQL OUTPUT parameter receives the argument's value and copies the
//!   result back, truncating it to the variable's type. A PostgreSQL INOUT
//!   parameter receives the value; an OUT parameter does not (it starts
//!   NULL); the copy back into a shorter varchar variable fails.
//! - After the first `@name = value` argument T-SQL accepts only named
//!   ones; an unknown name fails the call.

use std::collections::HashMap;

use sqldiff_syntax::{ast as cst, Dialect};

use super::ast::{self, string_value, Tok, Type, K};
use super::canon::asm;
use super::lower::Unit;
use super::schema::Schema;
use super::table::{self, Name, Ty};
use super::term::{a, n, Term};
use crate::catalog::{self, CatalogView, Ident, Lookup, ParamMode, Routine, RoutineKind};
use crate::model::Assumption;

type R<T> = Result<T, String>;

pub mod asms {
    pub const CALLEE: (&str, &str) = (
        "ASM-CALLEE-CONVERTED",
        "The procedure a call names on side B is the conversion of the one it names on side A; that procedure's own conversion (signature, body, result sets) is compared in the file that creates it",
    );
    pub const PARAM_TYPMOD: (&str, &str) = (
        "ASM-PG-PARAMETER-TYPMOD",
        "Arguments and the values a procedure assigns to its OUTPUT parameters already have the parameter's declared length, precision and scale: PostgreSQL ignores them in a parameter's type (nothing is truncated, rounded or padded), T-SQL converts to them",
    );
    pub const EXTPROP_COMMENT: (&str, &str) = (
        "ASM-EXTPROP-COMMENT",
        "Whoever reads the T-SQL extended property reads the PostgreSQL comment instead: the property name is not kept, and an object has one comment",
    );
}

/// One argument as written.
#[derive(Debug, Clone)]
pub struct Arg {
    /// Parameter name: T-SQL with its `@`, lower case; PostgreSQL folded.
    pub name: Option<String>,
    /// The value's tokens; empty for T-SQL `DEFAULT`.
    pub value: Vec<Tok>,
    /// T-SQL `OUTPUT` / `OUT`.
    pub output: bool,
    pub off: usize,
}

#[derive(Debug, Clone)]
pub struct Call {
    pub name: Vec<Name>,
    pub args: Vec<Arg>,
}

/// What an object comment is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Schema(Name),
    /// TABLE, VIEW, PROCEDURE or FUNCTION.
    Object(&'static str, Name, Name),
    Column(&'static str, Name, Name, Name),
    /// Schema, table (T-SQL only: PostgreSQL names the index alone) and index.
    Index(Name, Option<Name>, Name),
    /// Schema, table and constraint.
    Constraint(Name, Name, Name),
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub target: Target,
    /// The comment text; `None` for `IS NULL`.
    pub text: Option<String>,
    /// A T-SQL literal without the N prefix (read through the code page).
    pub narrow: bool,
    /// The PostgreSQL literal holds a backslash (read differently without
    /// standard_conforming_strings).
    pub backslash: bool,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Call(Call),
    Comment(Comment),
}

fn name_of(t: &Tok) -> Option<Name> {
    match t.kind {
        K::Word => Some(Name {
            text: t.text.clone(),
            quoted: false,
        }),
        K::Quoted => Some(Name {
            text: cst::unquote(&t.text),
            quoted: true,
        }),
        _ => None,
    }
}

/// A dotted name from `i`: its parts and the index after it.
fn dotted(ts: &[Tok], mut i: usize) -> R<(Vec<Name>, usize)> {
    let mut parts = Vec::new();
    loop {
        let p = ts
            .get(i)
            .and_then(name_of)
            .ok_or_else(|| "a name was expected".to_string())?;
        parts.push(p);
        i += 1;
        if ts.get(i).is_some_and(|t| t.kind == K::Dot) {
            i += 1;
        } else {
            return Ok((parts, i));
        }
    }
}

/// Splits on top-level commas.
fn split(ts: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, t) in ts.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Comma if depth == 0 => {
                out.push(&ts[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&ts[start..]);
    out
}

fn word(t: Option<&Tok>, w: &str) -> bool {
    t.is_some_and(|t| t.kind == K::Word && t.up == w)
}

/// The key a name part resolves to within its dialect (see `table::key`):
/// T-SQL ignores case (`ASM-IDENT-FOLD`), PostgreSQL folds unquoted names to
/// lower case and keeps a quoted name as written.
fn key(n: &Name, d: Dialect) -> R<String> {
    match d {
        Dialect::Postgres if n.quoted => Ok(n.text.clone()),
        _ => Ok(n.text.to_lowercase()),
    }
}

/// Schema (`None` for the default one) and name, each as `key` resolves it.
fn object_key(parts: &[Name], d: Dialect) -> R<(Option<String>, String)> {
    let default = match d {
        Dialect::Tsql => "dbo",
        Dialect::Postgres => "public",
        Dialect::Db2i => return Err("Db2 for i is not compared".into()),
    };
    match parts {
        [o] => Ok((None, key(o, d)?)),
        [s, o] => {
            let s = key(s, d)?;
            Ok(((s != default).then_some(s), key(o, d)?))
        }
        _ => Err("a name with more than two parts".into()),
    }
}

/// The names a PostgreSQL statement uses, checked against its catalog: a
/// T-SQL name matches the PostgreSQL name that differs from it only in
/// letter case (`ASM-IDENT-FOLD`), which says nothing when the catalog holds
/// two names that differ only in letter case (see `Schema::case_check`).
fn case_check<'n>(names: impl IntoIterator<Item = &'n Name>, s: &Side) -> R<()> {
    if s.d != Dialect::Postgres {
        return Ok(());
    }
    for n in names {
        s.schema
            .case_check(&key(n, s.d)?, true)
            .map_err(|e| format!("{}: {e}", s.name))?;
    }
    Ok(())
}

impl Target {
    fn names(&self) -> Vec<&Name> {
        match self {
            Target::Schema(s) => vec![s],
            Target::Object(_, s, o) => vec![s, o],
            Target::Column(_, s, o, c) => vec![s, o, c],
            Target::Index(s, t, x) => [Some(s), t.as_ref(), Some(x)]
                .into_iter()
                .flatten()
                .collect(),
            Target::Constraint(s, t, c) => vec![s, t, c],
        }
    }
}

/// Reads one call or comment statement.
pub fn parse(ts: &[Tok], d: Dialect) -> R<Stmt> {
    match d {
        Dialect::Tsql => tsql(ts),
        Dialect::Postgres => match ts.first().map(|t| t.up.as_str()) {
            Some("CALL") => pg_call(ts).map(Stmt::Call),
            Some("COMMENT") => pg_comment(ts).map(Stmt::Comment),
            _ => Err("not a CALL or COMMENT ON".into()),
        },
        Dialect::Db2i => Err("Db2 for i calls and comments are not compared yet".into()),
    }
}

fn tsql(ts: &[Tok]) -> R<Stmt> {
    if !matches!(ts.first().map(|t| t.up.as_str()), Some("EXEC" | "EXECUTE")) {
        return Err("not an EXEC".into());
    }
    if ts.get(1).is_some_and(|t| t.kind == K::Var) {
        return Err(if ts.get(2).is_some_and(|t| t.text == "=") {
            "the return code (`EXEC @rc = …`) is not compared".into()
        } else {
            "a procedure named by a variable".into()
        });
    }
    let (name, i) = dotted(ts, 1)?;
    let rest = &ts[i..];
    if rest
        .iter()
        .any(|t| word(Some(t), "WITH") || word(Some(t), "AT"))
    {
        return Err("EXEC with WITH or AT options".into());
    }
    let last = name
        .last()
        .map(|n| n.text.to_lowercase())
        .unwrap_or_default();
    if last == "sp_addextendedproperty" {
        if name.len() == 2 && !name[0].text.eq_ignore_ascii_case("sys") {
            return Err(format!("{}.sp_addextendedproperty", name[0].text));
        }
        return extended_property(rest).map(Stmt::Comment);
    }
    if last.starts_with("sp_") || last.starts_with("xp_") {
        return Err("a system procedure".into());
    }
    let mut args = Vec::new();
    if !rest.is_empty() {
        for (k, part) in split(rest).into_iter().enumerate() {
            args.push(tsql_arg(part, k)?);
        }
    }
    Ok(Stmt::Call(Call { name, args }))
}

fn tsql_arg(part: &[Tok], k: usize) -> R<Arg> {
    let off = part.first().map_or(0, |t| t.off);
    let mut v = part;
    let output = v
        .last()
        .is_some_and(|t| t.kind == K::Word && matches!(t.up.as_str(), "OUTPUT" | "OUT"));
    if output {
        v = &v[..v.len() - 1];
    }
    let mut name = None;
    if v.len() >= 2 && v[0].kind == K::Var && v[1].kind == K::Op && v[1].text == "=" {
        name = Some(v[0].text.to_lowercase());
        v = &v[2..];
    }
    // SQL Server accepts a constant, a variable or DEFAULT; an unquoted word
    // would be read as a string.
    let ok = match v {
        [t] => {
            matches!(t.kind, K::Num | K::Str | K::Var)
                || (t.kind == K::Word && matches!(t.up.as_str(), "NULL" | "DEFAULT"))
        }
        [s, t] => s.kind == K::Op && matches!(s.text.as_str(), "-" | "+") && t.kind == K::Num,
        _ => false,
    };
    if !ok {
        return Err(format!(
            "argument {}: only constants, variables and DEFAULT are compared",
            k + 1
        ));
    }
    let value = if word(v.first(), "DEFAULT") {
        Vec::new()
    } else {
        v.to_vec()
    };
    Ok(Arg {
        name,
        value,
        output,
        off,
    })
}

fn pg_call(ts: &[Tok]) -> R<Call> {
    let (name, i) = dotted(ts, 1)?;
    if ts.get(i).map(|t| t.kind) != Some(K::LParen) || ts.last().map(|t| t.kind) != Some(K::RParen)
    {
        return Err("CALL without an argument list".into());
    }
    let inner = &ts[i + 1..ts.len() - 1];
    if inner.iter().filter(|t| t.kind == K::LParen).count()
        != inner.iter().filter(|t| t.kind == K::RParen).count()
    {
        return Err("unbalanced parentheses".into());
    }
    let mut args = Vec::new();
    if !inner.is_empty() {
        for (k, part) in split(inner).into_iter().enumerate() {
            let off = part.first().map_or(0, |t| t.off);
            let (name, v) = match part {
                [p, op, rest @ ..] if op.kind == K::Op && op.text == "=>" => {
                    let p = name_of(p).ok_or("a parameter name was expected")?;
                    (Some(key(&p, Dialect::Postgres)?), rest)
                }
                [_, op, ..] if op.kind == K::Op && op.text == ":=" => {
                    return Err("the `name := value` argument form".into())
                }
                _ => (None, part),
            };
            if v.is_empty() {
                return Err(format!("argument {} is empty", k + 1));
            }
            args.push(Arg {
                name,
                value: v.to_vec(),
                output: false,
                off,
            });
        }
    }
    Ok(Call { name, args })
}

fn pg_comment(ts: &[Tok]) -> R<Comment> {
    if !word(ts.get(1), "ON") {
        return Err("COMMENT without ON".into());
    }
    let kind = ts.get(2).map(|t| t.up.as_str()).unwrap_or("");
    let (target, i) = match kind {
        "SCHEMA" => {
            let (p, i) = dotted(ts, 3)?;
            match <[Name; 1]>::try_from(p) {
                Ok([s]) => (Target::Schema(s), i),
                _ => return Err("a schema name with several parts".into()),
            }
        }
        "TABLE" | "VIEW" | "PROCEDURE" | "FUNCTION" => {
            let (p, i) = dotted(ts, 3)?;
            if ts.get(i).is_some_and(|t| t.kind == K::LParen) {
                return Err(format!("COMMENT ON {kind} with an argument list"));
            }
            let k = match kind {
                "TABLE" => "TABLE",
                "VIEW" => "VIEW",
                "PROCEDURE" => "PROCEDURE",
                _ => "FUNCTION",
            };
            match <[Name; 2]>::try_from(p) {
                Ok([s, o]) => (Target::Object(k, s, o), i),
                _ => return Err(format!("COMMENT ON {kind} without a schema name")),
            }
        }
        "COLUMN" => {
            let (p, i) = dotted(ts, 3)?;
            match <[Name; 3]>::try_from(p) {
                // TABLE or VIEW: PostgreSQL does not say which.
                Ok([s, o, c]) => (Target::Column("", s, o, c), i),
                _ => return Err("COMMENT ON COLUMN without schema, table and column".into()),
            }
        }
        "INDEX" => {
            let (p, i) = dotted(ts, 3)?;
            match <[Name; 2]>::try_from(p) {
                Ok([s, x]) => (Target::Index(s, None, x), i),
                _ => return Err("COMMENT ON INDEX without a schema name".into()),
            }
        }
        "CONSTRAINT" => {
            let c = ts.get(3).and_then(name_of).ok_or("a constraint name")?;
            if !word(ts.get(4), "ON") || word(ts.get(5), "DOMAIN") {
                return Err("COMMENT ON CONSTRAINT not on a table".into());
            }
            let (p, i) = dotted(ts, 5)?;
            match <[Name; 2]>::try_from(p) {
                Ok([s, t]) => (Target::Constraint(s, t, c), i),
                _ => return Err("COMMENT ON CONSTRAINT without a schema name".into()),
            }
        }
        other => return Err(format!("COMMENT ON {other}")),
    };
    if !word(ts.get(i), "IS") || ts.len() != i + 2 {
        return Err("COMMENT ON without one value after IS".into());
    }
    let v = &ts[i + 1];
    let (text, backslash) = match v.kind {
        K::Word if v.up == "NULL" => (None, false),
        K::Str if v.text.starts_with('\'') => (Some(string_value(&v.text)?), v.text.contains('\\')),
        _ => return Err("a comment that is not a plain string literal".into()),
    };
    Ok(Comment {
        target,
        text,
        narrow: false,
        backslash,
    })
}

/// `sp_addextendedproperty` with string literal arguments.
fn extended_property(rest: &[Tok]) -> R<Comment> {
    const NAMES: [&str; 8] = [
        "@name",
        "@value",
        "@level0type",
        "@level0name",
        "@level1type",
        "@level1name",
        "@level2type",
        "@level2name",
    ];
    let mut val: HashMap<&str, (String, bool)> = HashMap::new();
    let mut named = false;
    for (k, part) in split(rest).into_iter().enumerate() {
        let (key, v) = match part {
            [p, eq, v @ ..] if p.kind == K::Var && eq.kind == K::Op && eq.text == "=" => {
                named = true;
                let key = p.text.to_lowercase();
                let key = NAMES
                    .iter()
                    .find(|n| **n == key)
                    .ok_or_else(|| format!("sp_addextendedproperty has no parameter {}", p.text))?;
                (*key, v)
            }
            _ if named => return Err("a positional argument after a named one".into()),
            _ => (
                *NAMES
                    .get(k)
                    .ok_or("sp_addextendedproperty: too many arguments")?,
                part,
            ),
        };
        if val.contains_key(key) {
            return Err(format!("{key} is given twice"));
        }
        match v {
            [t] if t.kind == K::Str => {
                let narrow = !t.text.starts_with(['N', 'n']);
                val.insert(key, (string_value(&t.text)?, narrow));
            }
            [t] if word(Some(t), "NULL") || word(Some(t), "DEFAULT") => {}
            _ => return Err(format!("{key} is not a string literal")),
        }
    }
    let get = |k: &str| val.get(k).map(|v| v.0.clone());
    let nm = |k: &str| {
        get(k).map(|t| Name {
            text: t,
            quoted: true,
        })
    };
    let kind = |k: &str| get(k).map(|t| t.to_ascii_uppercase());
    let (text, narrow) = val
        .get("@value")
        .cloned()
        .ok_or("sp_addextendedproperty without a value")?;
    if get("@name").is_none() {
        return Err("sp_addextendedproperty without a name".into());
    }
    let need = |k: &str| nm(k).ok_or_else(|| format!("sp_addextendedproperty without {k}"));
    let target = match (
        kind("@level0type").as_deref(),
        kind("@level1type").as_deref(),
        kind("@level2type").as_deref(),
    ) {
        (Some("SCHEMA"), None, None) => Target::Schema(need("@level0name")?),
        (Some("SCHEMA"), Some(o @ ("TABLE" | "VIEW" | "PROCEDURE" | "FUNCTION")), None) => {
            let o = match o {
                "TABLE" => "TABLE",
                "VIEW" => "VIEW",
                "PROCEDURE" => "PROCEDURE",
                _ => "FUNCTION",
            };
            Target::Object(o, need("@level0name")?, need("@level1name")?)
        }
        (Some("SCHEMA"), Some(o @ ("TABLE" | "VIEW")), Some("COLUMN")) => Target::Column(
            if o == "TABLE" { "TABLE" } else { "VIEW" },
            need("@level0name")?,
            need("@level1name")?,
            need("@level2name")?,
        ),
        (Some("SCHEMA"), Some("TABLE"), Some("INDEX")) => Target::Index(
            need("@level0name")?,
            Some(need("@level1name")?),
            need("@level2name")?,
        ),
        (Some("SCHEMA"), Some("TABLE"), Some("CONSTRAINT")) => Target::Constraint(
            need("@level0name")?,
            need("@level1name")?,
            need("@level2name")?,
        ),
        (l0, l1, l2) => {
            return Err(format!(
                "an extended property on {}",
                [l0, l1, l2]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
        }
    };
    // The level names are names, not identifiers: compared like unquoted
    // T-SQL names (case-insensitive catalog collation).
    let unq = |n: Name| Name {
        text: n.text,
        quoted: false,
    };
    let target = match target {
        Target::Schema(s) => Target::Schema(unq(s)),
        Target::Object(k, s, o) => Target::Object(k, unq(s), unq(o)),
        Target::Column(k, s, o, c) => Target::Column(k, unq(s), unq(o), unq(c)),
        Target::Index(s, t, x) => Target::Index(unq(s), t.map(unq), unq(x)),
        Target::Constraint(s, t, c) => Target::Constraint(unq(s), unq(t), unq(c)),
    };
    Ok(Comment {
        target,
        text: Some(text),
        narrow,
        backslash: false,
    })
}

/// What pairs a statement with its counterpart: the call's procedure, or
/// the comment's object.
pub fn pair_key(s: &Stmt, d: Dialect) -> R<String> {
    // Names pair up to letter case (`ASM-IDENT-FOLD`); `compare` checks that
    // the PostgreSQL catalog has no other name differing only in case.
    let k = |n: &Name| -> R<String> { Ok(key(n, d)?.to_lowercase()) };
    let schema = |n: &Name| -> R<String> {
        let default = if d == Dialect::Tsql { "dbo" } else { "public" };
        Ok(if key(n, d)? == default {
            String::new()
        } else {
            k(n)?
        })
    };
    Ok(match s {
        Stmt::Call(c) => {
            let (s, o) = object_key(&c.name, d)?;
            format!(
                "call {}.{}",
                s.unwrap_or_default().to_lowercase(),
                o.to_lowercase()
            )
        }
        Stmt::Comment(c) => match &c.target {
            Target::Schema(s) => format!("comment schema {}", k(s)?),
            Target::Object(kind, s, o) => format!("comment {kind} {}.{}", schema(s)?, k(o)?),
            Target::Column(_, s, o, c) => {
                format!("comment column {}.{}.{}", schema(s)?, k(o)?, k(c)?)
            }
            Target::Index(s, _, x) => format!("comment index {}.{}", schema(s)?, k(x)?),
            Target::Constraint(s, t, c) => {
                format!("comment constraint {}.{}.{}", schema(s)?, k(t)?, k(c)?)
            }
        },
    })
}

/// Pairs two key sequences in order (longest common subsequence).
pub fn align(x: &[Option<String>], y: &[Option<String>]) -> Vec<(usize, usize)> {
    let (m, n) = (x.len(), y.len());
    let mut l = vec![vec![0u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            l[i][j] = if x[i].is_some() && x[i] == y[j] {
                l[i + 1][j + 1] + 1
            } else {
                l[i + 1][j].max(l[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < m && j < n {
        if x[i].is_some() && x[i] == y[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if l[i + 1][j] >= l[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The outcome of comparing one pair.
pub enum Verdict {
    Equal(Vec<Assumption>),
    /// A difference: rule, witness and the offsets (A, B) it points at.
    Diff(&'static str, String, (usize, usize)),
    Unknown(String),
}

/// One side of a pair.
pub struct Side<'s> {
    pub d: Dialect,
    pub unit: &'s Unit,
    pub schema: &'s Schema,
    pub name: &'s str,
    /// The statement sits in a PostgreSQL DO block (PL/pgSQL), even when
    /// the unit is a script.
    pub in_do: bool,
}

/// Compares a T-SQL statement `t` with a PostgreSQL statement `p`.
/// Offsets in the verdict are (T-SQL, PostgreSQL).
pub fn compare(t: (&Stmt, &Side), p: (&Stmt, &Side)) -> Verdict {
    match (t.0, p.0) {
        (Stmt::Comment(x), Stmt::Comment(y)) => comments(x, t.1, y, p.1),
        (Stmt::Call(x), Stmt::Call(y)) => match calls(x, t.1, y, p.1) {
            Ok(v) => v,
            Err(e) => Verdict::Unknown(e),
        },
        _ => Verdict::Unknown("a call against a comment".into()),
    }
}

// ---------------------------------------------------------------------
// Comments

fn ident(n: &Name) -> Ident {
    Ident::new(n.text.clone(), n.quoted)
}

/// Whether the object a comment names exists in a side's catalog, and why not.
fn exists(target: &Target, view: &CatalogView, side: &str) -> R<()> {
    let d = view.dialect;
    let missing = |what: String| Err(format!("{side}'s catalog has no {what}"));
    let table = |s: &Name, t: &Name| -> Option<&catalog::Table> {
        match view.table(&[ident(s), ident(t)]) {
            Lookup::Found(t) => Some(t),
            _ => None,
        }
    };
    let is_view = |t: &catalog::Table| t.kind == catalog::TableKind::View;
    let is_table = |t: &catalog::Table| t.kind == catalog::TableKind::Table;
    match target {
        Target::Schema(s) => {
            let k = ident(s).key(d);
            let known = view.catalog.schemas.iter().any(|x| catalog::key(d, x) == k)
                || view
                    .catalog
                    .tables
                    .iter()
                    .filter_map(|t| t.schema.as_deref())
                    .chain(
                        view.catalog
                            .routines
                            .iter()
                            .filter_map(|r| r.schema.as_deref()),
                    )
                    .any(|x| catalog::key(d, x) == k);
            if !known {
                return missing(format!("schema {}", s.text));
            }
        }
        Target::Object(k @ ("TABLE" | "VIEW"), s, o) => match table(s, o) {
            Some(t) if (*k == "VIEW") == is_view(t) && (is_table(t) || is_view(t)) => {}
            Some(_) => return Err(format!("{side}'s {}.{} is not a {k}", s.text, o.text)),
            None => return missing(format!("{} {}.{}", k.to_lowercase(), s.text, o.text)),
        },
        Target::Object(k, s, o) => {
            let kind = if *k == "PROCEDURE" {
                RoutineKind::Procedure
            } else {
                RoutineKind::Function
            };
            let found = match view.routines(&[ident(s), ident(o)]) {
                Lookup::Found(rs) => rs.into_iter().filter(|r| r.kind == kind).count(),
                _ => 0,
            };
            match found {
                1 => {}
                0 => return missing(format!("{} {}.{}", k.to_lowercase(), s.text, o.text)),
                _ => {
                    return Err(format!(
                        "{side} has several {} named {}.{}",
                        k.to_lowercase(),
                        s.text,
                        o.text
                    ))
                }
            }
        }
        Target::Column(k, s, o, c) => match table(s, o) {
            Some(t) if (k.is_empty() || (*k == "VIEW") == is_view(t)) => {
                if t.column(d, &ident(c)).is_none() {
                    return missing(format!("column {}.{}.{}", s.text, o.text, c.text));
                }
            }
            Some(_) => return Err(format!("{side}'s {}.{} is not a {k}", s.text, o.text)),
            None => return missing(format!("table {}.{}", s.text, o.text)),
        },
        Target::Index(s, t, x) => {
            let k = ident(x).key(d);
            let sk = ident(s).key(d);
            let hit = view.catalog.tables.iter().any(|tb| {
                let in_schema = match &tb.schema {
                    Some(ts) => catalog::key(d, ts) == sk,
                    None => catalog::default_schema(d)
                        .is_some_and(|def| catalog::key(d, &Ident::plain(def).stored(d)) == sk),
                };
                let on_table = t
                    .as_ref()
                    .map_or(true, |t| ident(t).key(d) == catalog::key(d, &tb.name));
                in_schema && on_table && tb.indexes.iter().any(|i| catalog::key(d, &i.name) == k)
            });
            if !hit {
                return missing(format!("index {}.{}", s.text, x.text));
            }
        }
        Target::Constraint(s, t, c) => match table(s, t) {
            Some(tb) => {
                let k = ident(c).key(d);
                if !tb
                    .keys
                    .iter()
                    .filter_map(|x| x.name.as_deref())
                    .any(|n| catalog::key(d, n) == k)
                {
                    return missing(format!(
                        "key constraint {} on {}.{} (only primary keys and unique constraints are recorded)",
                        c.text, s.text, t.text
                    ));
                }
            }
            None => return missing(format!("table {}.{}", s.text, t.text)),
        },
    }
    Ok(())
}

fn comments(x: &Comment, t: &Side, y: &Comment, p: &Side) -> Verdict {
    let mut asms = vec![asm::make(asms::EXTPROP_COMMENT), asm::make(asm::IDENT_FOLD)];
    let schema_name = |tg: &Target| match tg {
        Target::Schema(s)
        | Target::Object(_, s, _)
        | Target::Column(_, s, _, _)
        | Target::Index(s, _, _)
        | Target::Constraint(s, _, _) => s.text.to_lowercase(),
    };
    if matches!(&x.target, Target::Schema(_)) {
    } else if schema_name(&x.target) == "dbo" && schema_name(&y.target) == "public" {
        asms.push(asm::make(asm::DEFAULT_SCHEMA));
    }
    // Kinds the pairing key leaves out.
    if let (Target::Column(kt, ..), Target::Column(..)) = (&x.target, &y.target) {
        if kt.is_empty() {
            return Verdict::Unknown("the T-SQL column's object kind is unknown".into());
        }
    }
    for (c, s) in [(x, t), (y, p)] {
        let Some(view) = s.schema.catalog() else {
            return Verdict::Unknown(format!(
                "{} has no catalog, so the commented object is not known to exist",
                s.name
            ));
        };
        // PostgreSQL does not say whether a column belongs to a table or a
        // view: the T-SQL side's kind decides.
        let target = match (&c.target, &x.target) {
            (Target::Column(_, s1, o, col), Target::Column(k, ..)) => {
                Target::Column(k, s1.clone(), o.clone(), col.clone())
            }
            (tg, _) => tg.clone(),
        };
        if let Err(e) = exists(&target, view, s.name).and_then(|_| case_check(target.names(), s)) {
            return Verdict::Unknown(e);
        }
    }
    if y.backslash {
        return Verdict::Unknown(
            "the comment holds a backslash, read differently without standard_conforming_strings"
                .into(),
        );
    }
    match (&x.text, &y.text) {
        (Some(a), Some(b)) if a == b => {
            if x.narrow && !a.is_ascii() {
                asms.push(asm::make(table::asm::TSQL_CODE_PAGE));
            }
            Verdict::Equal(asms)
        }
        (Some(a), Some(b)) => Verdict::Diff(
            "equiv.comment-text",
            format!(
                "{} gives the object the text '{a}' and {} the comment '{b}'",
                t.name, p.name
            ),
            (0, 0),
        ),
        _ => Verdict::Unknown("a comment set to NULL".into()),
    }
}

// ---------------------------------------------------------------------
// Calls

/// The one procedure a call names in a side's catalog.
fn callee<'v>(c: &Call, s: &'v Side) -> R<&'v Routine> {
    let view = s.schema.catalog().ok_or_else(|| {
        format!(
            "{} has no catalog, so the called procedure is not known",
            s.name
        )
    })?;
    let parts: Vec<Ident> = c.name.iter().map(ident).collect();
    let shown = c
        .name
        .iter()
        .map(|n| n.text.clone())
        .collect::<Vec<_>>()
        .join(".");
    let found: Vec<&Routine> = match view.routines(&parts) {
        Lookup::Found(rs) => rs
            .into_iter()
            .filter(|r| r.kind == RoutineKind::Procedure)
            .collect(),
        _ => Vec::new(),
    };
    match found.as_slice() {
        [r] => Ok(r),
        [] => Err(format!("{}'s catalog has no procedure {shown}", s.name)),
        _ => Err(format!("{} has several procedures named {shown}", s.name)),
    }
}

/// The argument each parameter receives, by parameter position; `None`
/// where the call leaves the parameter to its default.
fn bind<'c>(c: &'c Call, r: &Routine, s: &Side) -> R<Vec<Option<&'c Arg>>> {
    let mut out: Vec<Option<&Arg>> = vec![None; r.params.len()];
    let mut named = false;
    for (k, arg) in c.args.iter().enumerate() {
        let at = match &arg.name {
            Some(n) => {
                named = true;
                r.params
                    .iter()
                    .position(|p| {
                        if s.d == Dialect::Tsql {
                            p.name.to_lowercase() == *n
                        } else {
                            p.name == *n
                        }
                    })
                    .ok_or_else(|| format!("{}: {} has no parameter {n}", s.name, r.display()))?
            }
            None if named => {
                return Err(format!(
                    "{}: argument {} is positional after a named one",
                    s.name,
                    k + 1
                ))
            }
            None if k < r.params.len() => k,
            None => {
                return Err(format!(
                    "{}: {} takes {} parameter(s) and the call passes more",
                    s.name,
                    r.display(),
                    r.params.len()
                ))
            }
        };
        if out[at].is_some() {
            return Err(format!("{}: parameter {} is given twice", s.name, at + 1));
        }
        out[at] = Some(arg);
    }
    Ok(out)
}

/// Tokens of a default expression written in the catalog.
fn default_toks(text: &str, d: Dialect) -> Vec<Tok> {
    let root = sqldiff_syntax::parse(&format!("SELECT {text}"), d).syntax();
    let mut ts = ast::tokens(&root);
    if !ts.is_empty() {
        ts.remove(0);
    }
    ts
}

/// A value as passed: what the side converts to the parameter type.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Var(String),
    /// String literal value; `national`: T-SQL N'…'.
    Str(String, bool),
    Num(String),
    Bool(bool),
}

fn value(ts: &[Tok], d: Dialect) -> R<Val> {
    let txt = || {
        ts.iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    match ts {
        [t] if t.kind == K::Word && t.up == "NULL" => Ok(Val::Null),
        [t] if t.kind == K::Var && d == Dialect::Tsql => {
            Ok(Val::Var(t.text.trim_start_matches('@').to_lowercase()))
        }
        [t] if t.kind == K::Word
            && d == Dialect::Postgres
            && matches!(t.up.as_str(), "TRUE" | "FALSE") =>
        {
            Ok(Val::Bool(t.up == "TRUE"))
        }
        [t] if matches!(t.kind, K::Word | K::Quoted) && d == Dialect::Postgres => {
            let n = name_of(t).unwrap();
            if n.quoted && n.text != n.text.to_lowercase() {
                return Err(format!("the quoted variable name \"{}\"", n.text));
            }
            Ok(Val::Var(key(&n, d)?))
        }
        [t] if t.kind == K::Str => {
            if d == Dialect::Postgres && !t.text.starts_with('\'') {
                return Err(format!("the literal {}", t.text));
            }
            Ok(Val::Str(
                string_value(&t.text)?,
                t.text.starts_with(['N', 'n']),
            ))
        }
        [t] if t.kind == K::Num => Ok(Val::Num(t.text.clone())),
        [s, t] if s.kind == K::Op && s.text == "-" && t.kind == K::Num => {
            Ok(Val::Num(format!("-{}", t.text)))
        }
        [s, t] if s.kind == K::Op && s.text == "+" && t.kind == K::Num => {
            Ok(Val::Num(t.text.clone()))
        }
        _ => Err(format!("the value `{}` is not compared", txt())),
    }
}

/// A variable of the calling routine as the statement engine sees it.
fn var_term(name: &str, s: &Side) -> R<Term> {
    let tok = if s.d == Dialect::Tsql {
        super::synth(K::Var, &format!("@{name}"))
    } else {
        super::synth(K::Word, name)
    };
    let toks = [tok];
    let e = ast::Parser::new(&toks, s.d).expr()?;
    let mut c = super::canon::Canon::new(s.unit, s.schema);
    Ok(c.expr(&e)?.0)
}

/// The declared type of a variable or parameter of the calling routine.
fn var_type(u: &Unit, name: &str) -> Option<Type> {
    u.locals
        .iter()
        .rev()
        .find(|l| l.name == name)
        .and_then(|l| l.decl.clone())
        .or_else(|| {
            u.params
                .iter()
                .find(|p| p.name == name)
                .and_then(|p| p.decl.clone())
        })
}

fn param_type(p: &catalog::Param) -> Type {
    let mut args: Vec<String> = p.data_type.args.iter().map(u32::to_string).collect();
    if p.data_type.max {
        args.insert(0, "MAX".into());
    }
    Type {
        name: p.data_type.base.clone(),
        args,
    }
}

fn read_ty(t: &Type, d: Dialect) -> R<Ty> {
    match d {
        Dialect::Tsql => table::tsql_type(t),
        Dialect::Postgres => table::pg_type(t),
        Dialect::Db2i => Err("Db2 for i types are not compared".into()),
    }
}

/// Integer digits a value of the type can have.
fn int_digits(t: &Ty) -> Option<u32> {
    match t {
        Ty::Bit => Some(1),
        Ty::TinyInt => Some(3),
        Ty::Int(16) => Some(5),
        Ty::Int(32) => Some(10),
        Ty::Int(64) => Some(19),
        Ty::Dec(Some((p, s))) => Some(p - s),
        _ => None,
    }
}

/// A T-SQL value of type `v` converted to type `p` keeps its value: no
/// truncation, rounding, padding or overflow.
fn fits_tsql(v: &Ty, p: &Ty) -> bool {
    if v == p {
        return !matches!(
            v,
            Ty::Str { fixed: true, .. } | Ty::Binary { fixed: true, .. }
        );
    }
    match (v, p) {
        (Ty::Int(a), Ty::Int(b)) => a <= b,
        (Ty::Bit | Ty::TinyInt, Ty::Int(b)) => *b >= 16,
        (Ty::Bit | Ty::TinyInt | Ty::Int(_), Ty::Dec(Some((pp, ps)))) => {
            int_digits(v).is_some_and(|x| x <= pp - ps)
        }
        (Ty::Dec(Some((p1, s1))), Ty::Dec(Some((p2, s2)))) => s1 <= s2 && p1 - s1 <= p2 - s2,
        (
            Ty::Str {
                fixed: false,
                national: n1,
                len: l1,
            },
            Ty::Str {
                fixed: false,
                national: n2,
                len: l2,
            },
        ) => {
            (*n2 || !*n1)
                && match (l1, l2) {
                    (_, None) => true,
                    (Some(a), Some(b)) => a <= b,
                    (None, Some(_)) => false,
                }
        }
        (Ty::Time(a), Ty::Time(b)) | (Ty::DateTime2(a), Ty::DateTime2(b)) => a <= b,
        _ => false,
    }
}

/// A PostgreSQL value of type `v` passes to a parameter of type `p`
/// unchanged: an implicit cast that keeps the value (the parameter's own
/// length, precision and scale are ignored).
fn fits_pg(v: &Ty, p: &Ty) -> bool {
    match (v, p) {
        (Ty::Int(a), Ty::Int(b)) => a <= b,
        (Ty::Int(_) | Ty::Dec(_), Ty::Dec(_)) => true,
        (Ty::Bool, Ty::Bool) | (Ty::Date, Ty::Date) | (Ty::Uuid, Ty::Uuid) => true,
        (Ty::Str { fixed: false, .. }, Ty::Str { fixed: false, .. }) => true,
        (Ty::Timestamp(a), Ty::Timestamp(b)) | (Ty::Time(a), Ty::Time(b)) => a <= b,
        (Ty::Float(a), Ty::Float(b)) => !*a || *b,
        _ => false,
    }
}

/// A PostgreSQL type name without its aliases.
fn pg_base(name: &str) -> String {
    let n = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let n = n
        .rsplit('.')
        .next()
        .unwrap_or(&n)
        .trim_matches('"')
        .to_string();
    match n.as_str() {
        "int" | "integer" => "int4",
        "bigint" => "int8",
        "smallint" => "int2",
        "character varying" => "varchar",
        "character" | "char" => "bpchar",
        "decimal" => "numeric",
        "boolean" => "bool",
        "double precision" | "float" => "float8",
        "real" => "float4",
        "timestamp without time zone" => "timestamp",
        "timestamp with time zone" => "timestamptz",
        "time without time zone" => "time",
        "time with time zone" => "timetz",
        _ => return n,
    }
    .to_string()
}

/// Whether PostgreSQL ignores part of the parameter's declared type.
fn has_typmod(p: &Type) -> bool {
    !p.args.is_empty()
}

/// The value a literal becomes in a parameter of type `ty`, as a term;
/// None when that is not modelled for certain.
fn literal_term(v: &Val, ty: &Type, d: Dialect, asms: &mut Vec<Assumption>) -> Option<Term> {
    use super::literal::{convert, Target};
    let rt = read_ty(ty, d).ok()?;
    if let Ty::Str {
        fixed,
        national,
        len,
    } = rt
    {
        let Val::Str(s, lit_national) = v else {
            return None;
        };
        if d == Dialect::Tsql {
            // SQL Server truncates to the parameter's length and pads char.
            let units = if national {
                s.encode_utf16().count()
            } else {
                s.chars().count()
            };
            if len.is_some_and(|n| units > n as usize) || (fixed && len != Some(units as u32)) {
                return None;
            }
            if !s.is_ascii() && (!national || !lit_national) {
                asms.push(asm::make(table::asm::TSQL_CODE_PAGE));
            }
        } else if fixed {
            return None;
        }
        return Some(n("str", vec![a(s.clone())]));
    }
    let target = Target::of_type(ty, d)?;
    match (v, target) {
        (Val::Bool(b), Target::Bool) => Some(n("bool", vec![a(if *b { "TRUE" } else { "FALSE" })])),
        (Val::Bool(_), _) => None,
        // An integer literal is no boolean in PostgreSQL.
        (Val::Num(_), Target::Bool) => None,
        (Val::Num(x), Target::Int { .. } | Target::Exact { .. } | Target::Bit) => {
            let (t, a) = convert(x, target, d, d == Dialect::Tsql)?;
            if let Some(a) = a {
                asms.push(asm::make(a));
            }
            Some(t)
        }
        (Val::Num(_), _) => None,
        (Val::Str(s, _), _) => {
            let (t, a) = convert(s, target, d, d == Dialect::Tsql)?;
            if let Some(a) = a {
                asms.push(asm::make(a));
            }
            Some(t)
        }
        _ => None,
    }
}

/// bit 0/1 and boolean FALSE/TRUE are the same value.
fn same_value(x: &Term, y: &Term) -> bool {
    if x == y {
        return true;
    }
    let bit = |t: &Term| {
        t.is("num")
            .then(|| t.kid(0).and_then(Term::atom).map(str::to_string))
            .flatten()
    };
    let b = |t: &Term| {
        t.is("bool")
            .then(|| t.kid(0).and_then(Term::atom).map(str::to_string))
            .flatten()
    };
    matches!(
        (bit(x).as_deref(), b(y).as_deref()),
        (Some("0"), Some("FALSE")) | (Some("1"), Some("TRUE"))
    )
}

/// Two values of the same kind, so unequal terms are unequal values.
fn comparable(x: &Term, y: &Term) -> bool {
    // bit against boolean.
    if x.is("num") && y.is("bool") {
        return true;
    }
    x.head() == y.head() && (!x.is("tval") || x.kid(0) == y.kid(0))
}

fn calls(x: &Call, t: &Side, y: &Call, p: &Side) -> R<Verdict> {
    let mut asms = vec![asm::make(asms::CALLEE), asm::make(asm::IDENT_FOLD)];
    let (rt, rp) = (callee(x, t)?, callee(y, p)?);
    case_check(&y.name, p)?;
    if object_key(&x.name, t.d)?.0.is_none() && object_key(&y.name, p.d)?.0.is_none() {
        asms.push(asm::make(asm::DEFAULT_SCHEMA));
    }
    if rt.params.len() != rp.params.len() {
        return Err(format!(
            "{} takes {} parameter(s) on {} and {} on {}",
            rt.display(),
            rt.params.len(),
            t.name,
            rp.params.len(),
            p.name
        ));
    }
    let (bt, bp) = (bind(x, rt, t)?, bind(y, rp, p)?);
    for (i, (pt, pp)) in rt.params.iter().zip(&rp.params).enumerate() {
        let what = format!("parameter {} ({} / {})", i + 1, pt.name, pp.name);
        let out = match (pt.mode, pp.mode) {
            (ParamMode::In, ParamMode::In) => false,
            (ParamMode::InOut, ParamMode::InOut) => true,
            (ParamMode::InOut, ParamMode::Out) => {
                return Err(format!(
                "{what} is OUTPUT on {}, which passes the value in, and OUT on {}, which does not",
                t.name, p.name
            ))
            }
            (a, b) => {
                return Err(format!(
                    "{what} is {a:?} on {} and {b:?} on {}",
                    t.name, p.name
                ))
            }
        };
        let (tt, tp) = (param_type(pt), param_type(pp));
        let (ty_t, ty_p) = (read_ty(&tt, t.d)?, read_ty(&tp, p.d)?);
        // The value each side passes: the argument, or the default.
        let pick = |arg: Option<&Arg>, prm: &catalog::Param, s: &Side| -> R<Vec<Tok>> {
            match arg {
                Some(a) if !a.value.is_empty() => Ok(a.value.clone()),
                _ => match (&prm.default, prm.has_default) {
                    (Some(d), _) => Ok(default_toks(d, s.d)),
                    (None, true) => Err(format!("{}: the default of {what} is not known", s.name)),
                    (None, false) => Err(format!(
                        "{}: {what} has no default and the call does not pass it",
                        s.name
                    )),
                },
            }
        };
        let (at, ap) = (bt[i], bp[i]);
        if out {
            // T-SQL copies back only into an argument marked OUTPUT;
            // PostgreSQL always copies an INOUT parameter back.
            if !at.is_some_and(|a| a.output) || ap.is_none() {
                return Err(format!(
                    "{what}: the output is not passed to a variable on both sides"
                ));
            }
            if p.unit.kind == super::lower::UnitKind::Script && !p.in_do {
                return Err(format!(
                    "{what}: a CALL outside PL/pgSQL returns INOUT values as a row"
                ));
            }
        } else if at.is_some_and(|a| a.output) {
            return Err(format!("{what}: OUTPUT is passed for an input parameter"));
        }
        let (vt, vp) = (pick(at, pt, t)?, pick(ap, pp, p)?);
        let (xv, yv) = (value(&vt, t.d)?, value(&vp, p.d)?);
        match (&xv, &yv) {
            (Val::Null, Val::Null) => {}
            (Val::Var(a), Val::Var(b)) => {
                let (vx, vy) = (var_term(a, t)?, var_term(b, p)?);
                let c = super::compare::compare(&vx, &vy);
                if !c.mismatches.is_empty() {
                    return Err(format!(
                        "{what}: the variables {a} ({}) and {b} ({}) were not shown the same",
                        t.name, p.name
                    ));
                }
                asms.extend(c.assumptions);
                let decl = |u: &Unit, nm: &str, s: &Side| -> R<Ty> {
                    let ty = var_type(u, nm)
                        .ok_or_else(|| format!("{}: the type of {nm} is not known", s.name))?;
                    read_ty(&ty, s.d)
                };
                let (vty_t, vty_p) = (decl(t.unit, a, t)?, decl(p.unit, b, p)?);
                let c = table::types(&vty_t, &vty_p);
                // A wider PostgreSQL variable is the assignments' business,
                // compared where they are; the values passed are the same.
                if c.diff.is_some() {
                    return Err(format!(
                        "{what}: the variables' types differ ({} on {}, {} on {})",
                        show(&vty_t),
                        t.name,
                        show(&vty_p),
                        p.name
                    ));
                }
                asms.extend(c.asms.into_iter().map(asm::make));
                if !fits_tsql(&vty_t, &ty_t) || !fits_pg(&vty_p, &ty_p) {
                    return Err(format!(
                        "{what}: the variable's type does not pass unchanged to the parameter's"
                    ));
                }
                if out && (!fits_tsql(&ty_t, &vty_t) || !fits_pg(&ty_p, &vty_p)) {
                    return Err(format!(
                        "{what}: the OUTPUT value does not copy back unchanged into the variable"
                    ));
                }
                // PostgreSQL fails the call ("argument is not writable") when an
                // INOUT argument needs any cast, even varchar to text.
                if out {
                    let vp = var_type(p.unit, b)
                        .ok_or_else(|| format!("{}: the type of {b} is not known", p.name))?;
                    if pg_base(&vp.name) != pg_base(&tp.name) {
                        return Err(format!(
                            "{what}: PostgreSQL passes an INOUT variable only of the parameter's own type ({} against {})",
                            vp.name, tp.name
                        ));
                    }
                }
                if has_typmod(&tp) {
                    asms.push(asm::make(asms::PARAM_TYPMOD));
                }
            }
            (Val::Var(_), _) | (_, Val::Var(_)) => {
                return Err(format!(
                    "{what}: a variable on one side and a value on the other"
                ))
            }
            _ => {
                let lt = literal_term(&xv, &tt, t.d, &mut asms);
                let lp = literal_term(&yv, &tp, p.d, &mut asms);
                match (lt, lp) {
                    (Some(a), Some(b)) if same_value(&a, &b) => {
                        // A decimal with fewer digits than the scale: SQL
                        // Server pads it, PostgreSQL keeps it as written.
                        if matches!(ty_t, Ty::Dec(_)) && has_typmod(&tp) {
                            asms.push(asm::make(asms::PARAM_TYPMOD));
                        }
                    }
                    // Values of other kinds: the parameter types differ,
                    // which the callee's own comparison reports.
                    (Some(a), Some(b)) if !comparable(&a, &b) => {
                        return Err(format!(
                            "{what}: the parameter types differ ({} on {}, {} on {})",
                            show(&ty_t),
                            t.name,
                            show(&ty_p),
                            p.name
                        ))
                    }
                    (Some(a), Some(b)) => {
                        let off = (at.map_or(0, |x| x.off), ap.map_or(0, |x| x.off));
                        return Ok(Verdict::Diff(
                            "equiv.call-argument",
                            format!(
                                "{} passes {} to {what} of {}, {} passes {}",
                                t.name,
                                super::compare::render(&a),
                                rt.display(),
                                p.name,
                                super::compare::render(&b)
                            ),
                            off,
                        ));
                    }
                    _ => {
                        return Err(format!(
                            "{what}: the value's conversion to {} / {} is not modelled",
                            show(&ty_t),
                            show(&ty_p)
                        ))
                    }
                }
            }
        }
    }
    let mut seen = Vec::new();
    asms.retain(|x| {
        let new = !seen.contains(&x.id);
        seen.push(x.id.clone());
        new
    });
    Ok(Verdict::Equal(asms))
}

fn show(t: &Ty) -> String {
    format!("{t:?}").to_lowercase()
}

/// The values T-SQL `sp_executesql` passes to the parameters of its text
/// (`params`, with their declared types) against the values of PL/pgSQL
/// `EXECUTE … USING`. Only a variable of the calling routine on each side,
/// the same variable, of exactly the parameter's type, is compared: T-SQL
/// converts a value to the parameter type as it passes it (2.7 to an int
/// parameter gives 2) while PostgreSQL types `$n` by the value itself, so
/// any conversion would differ. Returns the assumptions and the type each
/// `$n` takes on PostgreSQL.
pub fn dynamic_args(
    params: &[(String, Type)],
    targs: &[Vec<Tok>],
    pargs: &[Vec<Tok>],
    t: &Side,
    p: &Side,
) -> R<(Vec<Assumption>, Vec<Type>)> {
    let mut asms = Vec::new();
    let mut types = Vec::new();
    for (i, ((pname, pty), (vt, vp))) in params.iter().zip(targs.iter().zip(pargs)).enumerate() {
        let what = format!("the value of @{pname} (USING ${})", i + 1);
        let (Val::Var(a), Val::Var(b)) = (value(vt, t.d)?, value(vp, p.d)?) else {
            return Err(format!(
                "{what}: only a variable passed on both sides is compared"
            ));
        };
        let (x, y) = (var_term(&a, t)?, var_term(&b, p)?);
        let c = super::compare::compare(&x, &y);
        if !c.mismatches.is_empty() {
            return Err(format!(
                "{what}: the variables {a} and {b} were not shown the same"
            ));
        }
        asms.extend(c.assumptions);
        let decl_t = var_type(t.unit, &a)
            .ok_or_else(|| format!("{}: the type of {a} is not known", t.name))?;
        let decl_p = var_type(p.unit, &b)
            .ok_or_else(|| format!("{}: the type of {b} is not known", p.name))?;
        let (vty_t, vty_p, ty_t) = (
            read_ty(&decl_t, t.d)?,
            read_ty(&decl_p, p.d)?,
            read_ty(pty, Dialect::Tsql)?,
        );
        if !fits_tsql(&vty_t, &ty_t) || !fits_tsql(&ty_t, &vty_t) {
            return Err(format!(
                "{what}: the variable's type ({}) is not the parameter's ({}), so T-SQL converts it",
                show(&vty_t),
                show(&ty_t)
            ));
        }
        let c = table::types(&vty_t, &vty_p);
        if c.diff.is_some() {
            return Err(format!(
                "{what}: the variables' types differ ({} on {}, {} on {})",
                show(&vty_t),
                t.name,
                show(&vty_p),
                p.name
            ));
        }
        asms.extend(c.asms.into_iter().map(asm::make));
        types.push(decl_p);
    }
    Ok((asms, types))
}
