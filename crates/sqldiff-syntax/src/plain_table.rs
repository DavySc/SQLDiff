//! Plain tables: the `CREATE TABLE` statements whose every fact the proof
//! checker's C3 slices can take as given (`Sqldiff.Mirror.CatTable` in
//! formal/lean). A table is read only when its definition is nothing but
//! `int` columns and (C3c) `varchar(n)`/`nvarchar(n)` columns, their
//! nullability, a primary key on `int` columns, and (C3c) constant
//! defaults; anything else (an expression or function default, identity,
//! computed column, other constraint, collation, table option, temporary
//! table, `IF NOT EXISTS`, another type, a string key) is refused, never
//! skipped.
//!
//! T-SQL makes a column without `NULL` or `NOT NULL` nullable or not
//! depending on the session (`ANSI_NULL_DFLT_ON`, "SET ANSI_NULL_DFLT_ON
//! (Transact-SQL)"), so a T-SQL column outside the key must say which.
//!
//! (C3c) Plain indexes ([`index`]): a non-unique `CREATE INDEX` on plain
//! columns, without a filter or options. It changes no result of the
//! slice as long as no entry can exceed the engine's size limit, which the
//! caller checks against the table ([`PlainIndex::fits`]; eval/tables T7,
//! T8).

use crate::abstract_syntax::{string_value, tokens, Parser, Tok, K};
use crate::{Dialect, SyntaxKind, SyntaxNode};

/// A plain table as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainTable {
    /// The name's parts as written (a quoted part as the abstract syntax
    /// keeps it: `"Name` when its case matters in PostgreSQL).
    pub name: Vec<String>,
    /// In declaration order.
    pub cols: Vec<PlainCol>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainCol {
    pub name: String,
    pub not_null: bool,
    /// A column of the primary key (NOT NULL in any case).
    pub key: bool,
    /// (C3c) The column's type.
    pub ty: ColType,
    /// (C3c) A constant default; `DEFAULT NULL` reads as none (an `INSERT`
    /// that leaves the column out stores NULL either way).
    pub default: Option<ColDefault>,
}

/// (C3c) A plain column type. PostgreSQL has no `nvarchar`: its
/// `varchar(n)` and `character varying(n)` are [`ColType::Varchar`].
/// (C2c) The types the checker's slice holds beyond `int` and strings,
/// named for both dialects: T-SQL `bit` and PostgreSQL `boolean` are
/// [`ColType::Bool`], T-SQL `datetime2(p)` and PostgreSQL `timestamp(p)`
/// are [`ColType::Timestamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    /// T-SQL `int`/`integer`, PostgreSQL `integer`/`int`/`int4`.
    Int,
    /// `bigint` (PostgreSQL also `int8`).
    BigInt,
    /// `smallint` (PostgreSQL also `int2`).
    SmallInt,
    /// `varchar(n)` (also `character varying(n)`, `char varying(n)`),
    /// `1 <= n <= 4000`.
    Varchar(u32),
    /// T-SQL `nvarchar(n)`, `1 <= n <= 4000`.
    NVarchar(u32),
    /// T-SQL `bit`, PostgreSQL `boolean`/`bool`.
    Bool,
    /// `date`.
    Date,
    /// T-SQL `datetime2(p)`, PostgreSQL `timestamp(p)` (also `timestamp`,
    /// which is `timestamp(6)`, and `without time zone`), `p <= 6`.
    Timestamp(u32),
}

/// (C3c) A constant default: an integer in the 32-bit range for an integer
/// column, or a string literal of printable ASCII without a backslash that
/// fits a string column (`'..'`; T-SQL also `N'..'`), in any number of
/// parentheses. (C2c) For a `bit` column T-SQL `0`/`1` ([`ColDefault::Int`])
/// and PostgreSQL `FALSE`/`TRUE` ([`ColDefault::Bool`]); for a `date` or
/// `datetime2(p)` column a string literal in the ISO 8601 form the checker
/// reads (`'YYYY-MM-DD'`, `'YYYY-MM-DDThh:mm:ss[.f]'` with at most `p`
/// fractional digits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColDefault {
    Int(i32),
    Str(String),
    Bool(bool),
}

/// (C2c) `s` is `YYYY-MM-DD`, a date of the Gregorian calendar within
/// 0001-01-01 and 9999-12-31 (`Sqldiff.Mirror.dateLit?`).
pub fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        s.get(r)
            .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|t| t.parse().ok())
    };
    let (Some(y), Some(m), Some(d)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=9999).contains(&y) && (1..=days).contains(&d)
}

/// (C2c) `s` is `YYYY-MM-DDThh:mm:ss[.f]` with at most `p` fractional
/// digits (`Sqldiff.Mirror.tsLit?`).
pub fn is_iso_timestamp(s: &str, p: u32) -> bool {
    let Some((date, time)) = s.split_once('T') else {
        return false;
    };
    if !is_iso_date(date) {
        return false;
    }
    let (hms, frac) = match time.split_once('.') {
        Some((hms, frac)) => (hms, Some(frac)),
        None => (time, None),
    };
    let b = hms.as_bytes();
    let two = |i: usize| -> Option<u32> {
        hms.get(i..i + 2)
            .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|t| t.parse().ok())
    };
    if b.len() != 8 || b[2] != b':' || b[5] != b':' {
        return false;
    }
    let (Some(h), Some(mi), Some(se)) = (two(0), two(3), two(6)) else {
        return false;
    };
    if h >= 24 || mi >= 60 || se >= 60 {
        return false;
    }
    match frac {
        None => true,
        Some(f) => !f.is_empty() && f.len() <= p as usize && f.bytes().all(|c| c.is_ascii_digit()),
    }
}

/// The largest string length the slice takes (`Sqldiff.Mirror.tsqlMaxLen`).
pub const MAX_LEN: u32 = 4000;

/// The name as the dialect compares it: T-SQL ignores case (for ASCII,
/// under the collations the slice admits), PostgreSQL folds unquoted names.
fn same(d: Dialect, a: &str, b: &str) -> bool {
    match d {
        Dialect::Tsql => a.eq_ignore_ascii_case(b),
        _ => fold(a) == fold(b),
    }
}

fn fold(n: &str) -> String {
    if n.starts_with('"') {
        n.to_string()
    } else {
        n.to_lowercase()
    }
}

/// `PRIMARY KEY [CLUSTERED | NONCLUSTERED]`, after `[CONSTRAINT name]`.
fn primary_key(p: &mut Parser, d: Dialect) -> Result<bool, String> {
    let at = p.i;
    if p.eat_kw("CONSTRAINT") {
        p.ident()?;
    }
    if p.eat_kw("PRIMARY") {
        p.expect_kw("KEY")?;
        if d == Dialect::Tsql && !p.eat_kw("CLUSTERED") {
            p.eat_kw("NONCLUSTERED");
        }
        Ok(true)
    } else {
        p.i = at;
        Ok(false)
    }
}

/// A length in parentheses: `(n)`, `1 <= n <= 4000` (not `max`).
fn length(p: &mut Parser) -> Result<u32, String> {
    p.expect_kind(K::LParen)?;
    let n = match p.peek() {
        Some(t) if t.kind == K::Num && t.text.bytes().all(|b| b.is_ascii_digit()) => {
            t.text.parse::<u32>().ok()
        }
        _ => None,
    };
    let n = n
        .filter(|n| (1..=MAX_LEN).contains(n))
        .ok_or_else(|| p.err("a string length of 1 to 4000"))?;
    p.i += 1;
    p.expect_kind(K::RParen)?;
    Ok(n)
}

/// (C2c) A precision in parentheses, `(p)` with `p <= 6`; without, `dflt`
/// (`None`: a precision is required).
fn precision(p: &mut Parser, dflt: Option<u32>) -> Result<u32, String> {
    if !p.eat_kind(K::LParen) {
        return dflt.ok_or_else(|| p.err("a precision of 0 to 6"));
    }
    let n = match p.peek() {
        Some(t) if t.kind == K::Num && t.text.bytes().all(|b| b.is_ascii_digit()) => {
            t.text.parse::<u32>().ok()
        }
        _ => None,
    };
    let n = n
        .filter(|n| *n <= 6)
        .ok_or_else(|| p.err("a precision of 0 to 6"))?;
    p.i += 1;
    p.expect_kind(K::RParen)?;
    Ok(n)
}

/// (C3c) The column type at the cursor.
fn col_type(p: &mut Parser, d: Dialect) -> Result<ColType, String> {
    let up = match p.peek() {
        Some(x) if x.kind == K::Word => x.up.clone(),
        _ => return Err(p.err("only int, varchar(n) and the typed-value columns are plain")),
    };
    p.i += 1;
    match up.as_str() {
        "INT" | "INTEGER" => Ok(ColType::Int),
        "INT4" if d == Dialect::Postgres => Ok(ColType::Int),
        "BIGINT" => Ok(ColType::BigInt),
        "INT8" if d == Dialect::Postgres => Ok(ColType::BigInt),
        "SMALLINT" => Ok(ColType::SmallInt),
        "INT2" if d == Dialect::Postgres => Ok(ColType::SmallInt),
        "VARCHAR" => Ok(ColType::Varchar(length(p)?)),
        "NVARCHAR" if d == Dialect::Tsql => Ok(ColType::NVarchar(length(p)?)),
        "CHARACTER" | "CHAR" if p.eat_kw("VARYING") => Ok(ColType::Varchar(length(p)?)),
        "BIT" if d == Dialect::Tsql => Ok(ColType::Bool),
        "BOOLEAN" | "BOOL" if d == Dialect::Postgres => Ok(ColType::Bool),
        "DATE" => Ok(ColType::Date),
        // `datetime2` without a precision is `datetime2(7)`: not plain.
        "DATETIME2" if d == Dialect::Tsql => Ok(ColType::Timestamp(precision(p, None)?)),
        "TIMESTAMP" if d == Dialect::Postgres => {
            let n = precision(p, Some(6))?;
            if p.eat_kw("WITHOUT") {
                p.expect_kw("TIME")?;
                p.expect_kw("ZONE")?;
            }
            Ok(ColType::Timestamp(n))
        }
        _ => {
            p.i -= 1;
            Err(p.err("only int, varchar(n) and the typed-value columns are plain"))
        }
    }
}

/// (C3c) A constant after `DEFAULT`: an integer, a string, or NULL (none),
/// in any number of parentheses.
fn default_value(p: &mut Parser, d: Dialect) -> Result<Option<ColDefault>, String> {
    let mut depth = 0;
    while p.eat_kind(K::LParen) {
        depth += 1;
    }
    let neg = p.eat_op("-");
    let v = match p.peek() {
        Some(t) if t.kind == K::Word && t.up == "NULL" && !neg => None,
        // C2c: PostgreSQL `TRUE`/`FALSE` for a boolean column.
        Some(t)
            if t.kind == K::Word
                && (t.up == "TRUE" || t.up == "FALSE")
                && !neg
                && d == Dialect::Postgres =>
        {
            Some(ColDefault::Bool(t.up == "TRUE"))
        }
        Some(t) if t.kind == K::Num => {
            let (minus, digits) = match t.text.strip_prefix('-') {
                Some(rest) => (!neg, rest),
                None => (neg, t.text.as_str()),
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(p.err("only an integer, a string or NULL is a plain default"));
            }
            let v: i64 = digits
                .parse::<i64>()
                .ok()
                .map(|v| if minus { -v } else { v })
                .filter(|v| i32::try_from(*v).is_ok())
                .ok_or_else(|| p.err("an integer default outside the 32-bit range"))?;
            Some(ColDefault::Int(v as i32))
        }
        // PostgreSQL `N'..'` is a `character` value (blank-padded): only a
        // plain literal there.
        Some(t) if t.kind == K::Str && !neg && (d == Dialect::Tsql || t.text.starts_with('\'')) => {
            Some(ColDefault::Str(string_value(&t.text)?))
        }
        _ => return Err(p.err("only an integer, a string or NULL is a plain default")),
    };
    p.i += 1;
    for _ in 0..depth {
        p.expect_kind(K::RParen)?;
    }
    Ok(v)
}

/// `[CONSTRAINT name] DEFAULT value`, or nothing (the cursor kept).
fn default_clause(p: &mut Parser, d: Dialect) -> Result<Option<Option<ColDefault>>, String> {
    let at = p.i;
    if p.eat_kw("CONSTRAINT") {
        p.ident()?;
    }
    if p.eat_kw("DEFAULT") {
        Ok(Some(default_value(p, d)?))
    } else {
        p.i = at;
        Ok(None)
    }
}

/// (C3c) A default the column can take as it is: an integer for an `int`
/// column; printable ASCII without a backslash, within the length, for a
/// string column (`Sqldiff.Mirror.CatCol.ok`).
fn default_fits(d: Dialect, ty: ColType, v: &ColDefault) -> bool {
    match (ty, v) {
        (ColType::Int | ColType::BigInt, ColDefault::Int(_)) => true,
        (ColType::SmallInt, ColDefault::Int(i)) => i16::try_from(*i).is_ok(),
        (ColType::Varchar(n) | ColType::NVarchar(n), ColDefault::Str(s)) => {
            s.bytes().all(|b| (b' '..=b'~').contains(&b) && b != b'\\') && s.len() <= n as usize
        }
        // C2c: a bit from T-SQL `0`/`1` or PostgreSQL `FALSE`/`TRUE`; dates
        // and timestamps from the ISO literals the checker reads.
        (ColType::Bool, ColDefault::Int(i)) => d == Dialect::Tsql && (*i == 0 || *i == 1),
        (ColType::Bool, ColDefault::Bool(_)) => true,
        (ColType::Date, ColDefault::Str(s)) => is_iso_date(s),
        (ColType::Timestamp(p), ColDefault::Str(s)) => is_iso_timestamp(s, p),
        _ => false,
    }
}

/// The plain table a statement defines, or why it is not one.
pub fn read(n: &SyntaxNode, d: Dialect) -> Result<PlainTable, String> {
    if n.kind() != SyntaxKind::STATEMENT {
        return Err("not a statement".into());
    }
    let t = tokens(n);
    let mut p = Parser::new(&t, d);
    p.expect_kw("CREATE")?;
    p.expect_kw("TABLE")?;
    if p.is_kw("IF") {
        return Err("CREATE TABLE IF NOT EXISTS may keep another definition".into());
    }
    let name = p.qname()?;
    if name.iter().any(|x| x.starts_with(['#', '@'])) {
        return Err("a temporary table".into());
    }
    p.expect_kind(K::LParen)?;
    let mut cols: Vec<PlainCol> = Vec::new();
    let mut key: Option<Vec<String>> = None;
    loop {
        if primary_key(&mut p, d)? {
            if key.is_some() {
                return Err("two primary keys".into());
            }
            p.expect_kind(K::LParen)?;
            let mut k = vec![p.ident()?];
            while p.eat_kind(K::Comma) {
                k.push(p.ident()?);
            }
            p.expect_kind(K::RParen)?;
            key = Some(k);
        } else {
            let col = p.ident()?;
            let ty = col_type(&mut p, d)?;
            let (mut null, mut not_null, mut pk) = (false, false, false);
            let mut default: Option<Option<ColDefault>> = None;
            loop {
                if p.eat_kw("NOT") {
                    p.expect_kw("NULL")?;
                    if not_null {
                        return Err(p.err("NOT NULL twice"));
                    }
                    not_null = true;
                } else if p.eat_kw("NULL") {
                    if null {
                        return Err(p.err("NULL twice"));
                    }
                    null = true;
                } else if primary_key(&mut p, d)? {
                    if pk || key.is_some() {
                        return Err("two primary keys".into());
                    }
                    pk = true;
                } else if let Some(v) = default_clause(&mut p, d)? {
                    if default.is_some() {
                        return Err(format!("column {col} has two defaults"));
                    }
                    default = Some(v);
                } else {
                    break;
                }
            }
            if null && (not_null || pk) {
                return Err(format!("column {col} is both NULL and NOT NULL"));
            }
            if d == Dialect::Tsql && !(null || not_null || pk) {
                return Err(format!(
                    "column {col} does not say NULL or NOT NULL (ANSI_NULL_DFLT_ON decides)"
                ));
            }
            let default = default.flatten();
            if default.as_ref().is_some_and(|v| !default_fits(d, ty, v)) {
                return Err(format!(
                    "column {col} has a default it cannot take as it is"
                ));
            }
            if pk {
                key = Some(vec![col.clone()]);
            }
            if cols.iter().any(|c| same(d, &c.name, &col)) {
                return Err(format!("column {col} twice"));
            }
            cols.push(PlainCol {
                name: col,
                not_null: not_null || pk,
                key: false,
                ty,
                default,
            });
        }
        if !p.eat_kind(K::Comma) {
            break;
        }
    }
    p.expect_kind(K::RParen)?;
    // A T-SQL filegroup does not change what a write does.
    if d == Dialect::Tsql && p.eat_kw("ON") {
        p.ident()?;
    }
    if !p.at_end() {
        return Err(p.err("a table option"));
    }
    for k in key.unwrap_or_default() {
        let c = cols
            .iter_mut()
            .find(|c| same(d, &c.name, &k))
            .ok_or_else(|| format!("key column {k} is not a column"))?;
        if c.key {
            return Err(format!("key column {k} twice"));
        }
        // SQL Server compares string keys as `=` does, padding trailing
        // spaces (eval/tables T6); PostgreSQL compares them exactly. A bit
        // is no key column of the slice (C2c).
        if !matches!(
            c.ty,
            ColType::Int
                | ColType::BigInt
                | ColType::SmallInt
                | ColType::Date
                | ColType::Timestamp(_)
        ) {
            return Err(format!("key column {k} is a string or a bit"));
        }
        c.key = true;
        c.not_null = true;
    }
    Ok(PlainTable { name, cols })
}

/// (C3c) A plain index: non-unique, on plain columns, without a filter,
/// options or (PostgreSQL) an access method other than `btree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainIndex {
    /// The table's name as written.
    pub table: Vec<String>,
    /// The key columns.
    pub cols: Vec<String>,
    /// The `INCLUDE` columns.
    pub include: Vec<String>,
    /// T-SQL `CLUSTERED` (a 900-byte key limit instead of 1700).
    pub clustered: bool,
}

/// SQL Server's index key limits, in bytes (eval/tables T8: 1946 above).
pub const TSQL_CLUSTERED_KEY: u32 = 900;
pub const TSQL_NONCLUSTERED_KEY: u32 = 1700;
/// A bound on a PostgreSQL btree entry, below the 2704 bytes of the limit
/// (eval/tables T8: 54000 above), leaving room for the entry's header.
pub const PG_INDEX_ENTRY: u32 = 2600;

/// `(c [ASC|DESC], ...)`.
fn index_cols(p: &mut Parser, order: bool) -> Result<Vec<String>, String> {
    p.expect_kind(K::LParen)?;
    let mut cs = Vec::new();
    loop {
        cs.push(p.ident()?);
        if order && !p.eat_kw("ASC") {
            p.eat_kw("DESC");
        }
        if !p.eat_kind(K::Comma) {
            break;
        }
    }
    p.expect_kind(K::RParen)?;
    Ok(cs)
}

/// The plain index a statement creates, or why it is not one.
pub fn index(n: &SyntaxNode, d: Dialect) -> Result<PlainIndex, String> {
    if n.kind() != SyntaxKind::STATEMENT {
        return Err("not a statement".into());
    }
    let t = tokens(n);
    let mut p = Parser::new(&t, d);
    p.expect_kw("CREATE")?;
    let clustered = d == Dialect::Tsql && p.eat_kw("CLUSTERED");
    if d == Dialect::Tsql && !clustered {
        p.eat_kw("NONCLUSTERED");
    }
    p.expect_kw("INDEX")?;
    if p.is_kw("CONCURRENTLY") || p.is_kw("IF") || p.is_kw("ON") {
        return Err(p.err("not a plain index"));
    }
    p.ident()?;
    p.expect_kw("ON")?;
    if p.is_kw("ONLY") {
        return Err(p.err("not a plain index"));
    }
    let table = p.qname()?;
    if d == Dialect::Postgres && p.eat_kw("USING") && !p.eat_kw("BTREE") {
        return Err(p.err("an index access method other than btree"));
    }
    let cols = index_cols(&mut p, true)?;
    let include = if p.eat_kw("INCLUDE") {
        index_cols(&mut p, false)?
    } else {
        Vec::new()
    };
    // A T-SQL filegroup does not change what a write does.
    if d == Dialect::Tsql && p.eat_kw("ON") {
        p.ident()?;
    }
    if !p.at_end() {
        return Err(p.err("an index option or filter"));
    }
    Ok(PlainIndex {
        table,
        cols,
        include,
        clustered,
    })
}

impl PlainIndex {
    /// The index's columns are columns of `t`, and no entry of a row `t`
    /// can hold exceeds the engine's limit, counted from declared lengths:
    /// SQL Server, `int` 4, `varchar(n)` `n`, `nvarchar(n)` `2n` bytes over
    /// the key columns; PostgreSQL, `integer` 8 and `varchar(n)` `4n + 8`
    /// bytes (four bytes per UTF-8 character, header and alignment) over the
    /// key and `INCLUDE` columns.
    pub fn fits(&self, t: &PlainTable, d: Dialect) -> Result<(), String> {
        let ty = |c: &str| {
            t.cols
                .iter()
                .find(|x| same(d, &x.name, c))
                .map(|x| x.ty)
                .ok_or_else(|| format!("index column {c} is not a column"))
        };
        let mut size: u32 = 0;
        for c in &self.cols {
            size += match (d, ty(c)?) {
                (Dialect::Tsql, ColType::Varchar(n)) => n,
                (Dialect::Tsql, ColType::NVarchar(n)) => 2 * n,
                // C2c: the fixed-size types, at most 8 bytes each.
                (Dialect::Tsql, _) => 8,
                (_, ColType::Varchar(n) | ColType::NVarchar(n)) => 4 * n + 8,
                (_, _) => 8,
            };
        }
        for c in &self.include {
            let s = ty(c)?;
            if d != Dialect::Tsql {
                size += match s {
                    ColType::Varchar(n) | ColType::NVarchar(n) => 4 * n + 8,
                    _ => 8,
                };
            }
        }
        let limit = match d {
            Dialect::Tsql if self.clustered => TSQL_CLUSTERED_KEY,
            Dialect::Tsql => TSQL_NONCLUSTERED_KEY,
            _ => PG_INDEX_ENTRY,
        };
        if size > limit {
            return Err(format!(
                "an index entry of up to {size} bytes (limit {limit})"
            ));
        }
        Ok(())
    }
}

/// (C3d part 2) A table the checker may read without its routines writing
/// it (`Sqldiff.Mirror.CatTable.readOnly`): any permanent `CREATE TABLE`
/// whose definition cannot change what a plain `SELECT` returns. Each
/// column is either of the slice ([`ColType`], with its nullability) or
/// opaque (`ty` `None`): a type outside the slice, or a column whose
/// values a read could see otherwise than as stored, which the checker
/// takes as a name only. Constraints, defaults, identity and the like are
/// read past: no read sees them (eval/queries Q16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadTable {
    /// The name's parts as written, as for [`PlainTable::name`].
    pub name: Vec<String>,
    /// In declaration order.
    pub cols: Vec<ReadCol>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCol {
    pub name: String,
    /// The column's type when it is in the slice, `None` when opaque.
    pub ty: Option<ColType>,
    /// `NOT NULL` or a key column. A T-SQL column that says neither `NULL`
    /// nor `NOT NULL` reads as nullable, the weaker claim.
    pub not_null: bool,
    /// A column of the primary key the `CREATE TABLE` declares; none when
    /// the statement says `DEFERRABLE` or `INITIALLY` anywhere (a PostgreSQL
    /// deferred key may hold two equal keys until the transaction commits).
    pub key: bool,
}

/// The tokens of `t[from..]` up to the parenthesis that closes the one
/// before `from`, split at the commas at depth 0: each element's range,
/// and the position after the closing parenthesis.
fn elements(t: &[Tok], from: usize) -> Result<(Vec<std::ops::Range<usize>>, usize), String> {
    let (mut depth, mut start, mut out) = (0usize, from, Vec::new());
    for (i, x) in t.iter().enumerate().skip(from) {
        match x.kind {
            K::LParen => depth += 1,
            K::RParen if depth == 0 => {
                out.push(start..i);
                return Ok((out, i + 1));
            }
            K::RParen => depth -= 1,
            K::Comma if depth == 0 => {
                out.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
    }
    Err("an unclosed parenthesis".into())
}

/// The words of `t` at parenthesis depth 0, upper case, with their
/// positions.
fn top_words(t: &[Tok]) -> Vec<(usize, &str)> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (i, x) in t.iter().enumerate() {
        match x.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            K::Word if depth == 0 => out.push((i, x.up.as_str())),
            _ => {}
        }
    }
    out
}

/// What one column definition says, read past its constraints: `PRIMARY
/// KEY`, `NOT NULL`, an explicit `NULL` (not the one of `DEFAULT NULL` or
/// `ON DELETE SET NULL`), and whether something in it makes the column
/// opaque even when its type is of the slice: a collation (the slice's
/// string comparisons take the database's), dynamic data masking or
/// Always Encrypted (a read gives other values than those stored), a
/// generated or computed value, a period column, `SPARSE` or `FILESTREAM`.
struct ColFacts {
    pk: bool,
    not_null: bool,
    null: bool,
    opaque: bool,
}

fn col_facts(t: &[Tok]) -> ColFacts {
    // The words of a `CASE ... END` in a default (`DEFAULT CASE WHEN 1 IS
    // NOT NULL THEN 1 END`) say nothing of the column: left out.
    let mut w = Vec::new();
    let mut case = 0usize;
    for x in top_words(t) {
        match x.1 {
            "CASE" => case += 1,
            "END" if case > 0 => case -= 1,
            _ if case == 0 => w.push(x),
            _ => {}
        }
    }
    let word = |k: usize| w.get(k).map(|x| x.1);
    let mut f = ColFacts {
        pk: false,
        not_null: false,
        null: false,
        opaque: false,
    };
    if case > 0 {
        // An unclosed `CASE`: nothing after it was read.
        f.opaque = true;
    }
    for (k, &(_, x)) in w.iter().enumerate() {
        match x {
            "PRIMARY" if word(k + 1) == Some("KEY") => f.pk = true,
            "NULL" if k > 0 && word(k - 1) == Some("NOT") => f.not_null = true,
            "NULL" if !(k > 0 && matches!(word(k - 1), Some("DEFAULT" | "SET"))) => f.null = true,
            "COLLATE" | "MASKED" | "ENCRYPTED" | "SPARSE" | "FILESTREAM" | "ROWGUIDCOL"
            | "HIDDEN" | "PERSISTED" | "STORED" | "VIRTUAL" => f.opaque = true,
            // Exactly `GENERATED {ALWAYS | BY DEFAULT} AS IDENTITY` is an
            // identity column, whose values are ordinary integers once
            // stored; any other `GENERATED` (a PostgreSQL generated column,
            // also one followed by a constraint named `identity`, a T-SQL
            // period column) is opaque.
            "GENERATED" => {
                let next: Vec<&str> = w[k + 1..].iter().take(4).map(|&(_, y)| y).collect();
                let ident = matches!(
                    next.as_slice(),
                    ["ALWAYS", "AS", "IDENTITY", ..] | ["BY", "DEFAULT", "AS", "IDENTITY", ..]
                );
                if !ident {
                    f.opaque = true;
                }
            }
            _ => {}
        }
    }
    f
}

/// (C3d part 2) The element (of `CREATE TABLE`, or what `ALTER TABLE ...
/// ADD` adds) starting at `e` is a constraint, an inline index or a
/// period, not a column. `named`: it follows `CONSTRAINT name`, after which
/// only a constraint may stand. Words reserved in both dialects (`UNIQUE`,
/// `CHECK`, `FOREIGN`, `PRIMARY KEY`) always start one; the others only in
/// their dialect and form, since there they may be a column's name
/// (PostgreSQL `ALTER TABLE t ADD exclude integer` adds a column):
/// PostgreSQL `EXCLUDE` followed by `USING` or `(`; T-SQL `PERIOD FOR`
/// (`PERIOD FOR SYSTEM_TIME`) and T-SQL `INDEX` (an inline index, a
/// reserved word there).
fn constraint_head(e: &[Tok], d: Dialect, named: bool) -> bool {
    let up = |i: usize| {
        e.get(i)
            .filter(|x| x.kind == K::Word)
            .map(|x| x.up.as_str())
    };
    match up(0) {
        Some("UNIQUE" | "CHECK" | "FOREIGN") => true,
        Some("PRIMARY") => up(1) == Some("KEY"),
        Some("EXCLUDE") => {
            d == Dialect::Postgres
                && (up(1) == Some("USING") || e.get(1).is_some_and(|x| x.kind == K::LParen))
        }
        Some("PERIOD") => d == Dialect::Tsql && !named && up(1) == Some("FOR"),
        Some("INDEX") => d == Dialect::Tsql && !named,
        _ => false,
    }
}

/// After a column's type: the words that may start what follows it in a
/// column definition. Anything else (`[]`, `WITH TIME ZONE`, `ARRAY`, a
/// second type word) means the type was not read whole: opaque.
const AFTER_TYPE: [&str; 16] = [
    "NULL",
    "NOT",
    "CONSTRAINT",
    "PRIMARY",
    "UNIQUE",
    "CHECK",
    "REFERENCES",
    "FOREIGN",
    "DEFAULT",
    "IDENTITY",
    "GENERATED",
    "COLLATE",
    "INDEX",
    "ROWGUIDCOL",
    "SPARSE",
    "MASKED",
];

/// (C3d part 2) The read-only table a statement defines, or why it is not
/// one: refused are a temporary or unlogged-in-name-only table, `IF NOT
/// EXISTS`, PostgreSQL `INHERITS`, `PARTITION BY`, `PARTITION OF`, `OF
/// type`, `LIKE`, `AS SELECT`, a table access method, T-SQL table options
/// other than system versioning and data compression (memory-optimized,
/// ledger, graph and partition-scheme tables), and any element it cannot
/// read; never skipped silently.
pub fn read_only(n: &SyntaxNode, d: Dialect) -> Result<ReadTable, String> {
    if n.kind() != SyntaxKind::STATEMENT {
        return Err("not a statement".into());
    }
    let t = tokens(n);
    let mut p = Parser::new(&t, d);
    p.expect_kw("CREATE")?;
    if d == Dialect::Postgres {
        if p.is_kw("TEMP") || p.is_kw("TEMPORARY") || p.is_kw("GLOBAL") || p.is_kw("LOCAL") {
            return Err("a temporary table".into());
        }
        // An unlogged table loses its rows in a crash, not on a read.
        p.eat_kw("UNLOGGED");
    }
    p.expect_kw("TABLE")?;
    if p.is_kw("IF") {
        return Err("CREATE TABLE IF NOT EXISTS may keep another definition".into());
    }
    let name = p.qname()?;
    if name.iter().any(|x| x.starts_with(['#', '@'])) {
        return Err("a temporary table".into());
    }
    if !p.is_kind(K::LParen) {
        return Err(p.err("a table defined other than by its columns"));
    }
    let (elems, after) = elements(&t, p.i + 1)?;
    let mut cols: Vec<ReadCol> = Vec::new();
    let mut key: Option<Vec<String>> = None;
    for r in elems {
        let e = &t[r];
        let mut q = Parser::new(e, d);
        if q.at_end() {
            return Err("an empty table element".into());
        }
        if q.eat_kw("CONSTRAINT") {
            q.ident()?;
        }
        if q.eat_kws(&["PRIMARY", "KEY"]) {
            if key.is_some() || cols.iter().any(|c| c.key) {
                return Err("two primary keys".into());
            }
            if d == Dialect::Tsql && !q.eat_kw("CLUSTERED") {
                q.eat_kw("NONCLUSTERED");
            }
            key = Some(index_cols(&mut q, d == Dialect::Tsql)?);
            continue;
        }
        let up = q.peek().map(|x| x.up.clone()).unwrap_or_default();
        // A constraint, an inline index or the period of a system-versioned
        // table: no read sees it. `UNIQUE`, `CHECK` and `FOREIGN` are
        // reserved in both dialects; `EXCLUDE`, `PERIOD` and (PostgreSQL)
        // `INDEX` are not, so `exclude integer` is a column: each counts
        // only in its dialect and its form (`constraint_head`).
        let named = q.pos() != 0;
        if q.peek().is_some_and(|x| x.kind == K::Word) && constraint_head(&e[q.pos()..], d, named) {
            continue;
        }
        if up == "LIKE" && q.peek().is_some_and(|x| x.kind == K::Word) {
            return Err("CREATE TABLE ... LIKE".into());
        }
        if q.pos() != 0 {
            return Err(q.err("a constraint of a kind not read"));
        }
        let col = q.ident()?;
        if cols.iter().any(|c| same(d, &c.name, &col)) {
            return Err(format!("column {col} twice"));
        }
        let rest = &e[q.pos()..];
        let f = col_facts(rest);
        if f.null && (f.not_null || f.pk) {
            return Err(format!("column {col} is both NULL and NOT NULL"));
        }
        if f.pk && (key.is_some() || cols.iter().any(|c| c.key)) {
            return Err("two primary keys".into());
        }
        // PostgreSQL `serial` types: an integer column with a sequence
        // default, read as the integer it holds.
        let serial = match q.peek() {
            Some(x)
                if d == Dialect::Postgres
                    && x.kind == K::Word
                    && q.peek_at(1).map_or(true, |y| {
                        y.kind == K::Word && AFTER_TYPE.contains(&y.up.as_str())
                    }) =>
            {
                match x.up.as_str() {
                    "SERIAL" | "SERIAL4" => Some(ColType::Int),
                    "BIGSERIAL" | "SERIAL8" => Some(ColType::BigInt),
                    "SMALLSERIAL" | "SERIAL2" => Some(ColType::SmallInt),
                    _ => None,
                }
            }
            _ => None,
        };
        // A T-SQL computed column (`c AS expr`) has no type of its own.
        let computed = d == Dialect::Tsql && q.is_kw("AS");
        let ty = if computed || f.opaque {
            None
        } else if serial.is_some() {
            serial
        } else {
            match col_type(&mut q, d) {
                Ok(ty)
                    if q.at_end()
                        || q.peek().is_some_and(|x| {
                            x.kind == K::Word && AFTER_TYPE.contains(&x.up.as_str())
                        }) =>
                {
                    Some(ty)
                }
                _ => None,
            }
        };
        cols.push(ReadCol {
            name: col,
            ty,
            not_null: f.not_null || f.pk,
            key: f.pk,
        });
    }
    if cols.is_empty() {
        return Err("a table without columns".into());
    }
    // Table options: a T-SQL filegroup, system versioning (a plain read
    // gives the current rows, eval/queries Q17) and data compression;
    // PostgreSQL storage parameters and a tablespace.
    let mut q = Parser::new(&t[after..], d);
    while !q.at_end() {
        if d == Dialect::Tsql && (q.eat_kw("ON") || q.eat_kw("TEXTIMAGE_ON")) {
            q.ident()?;
            if q.is_kind(K::LParen) {
                return Err(q.err("a partition scheme"));
            }
        } else if q.eat_kw("WITH") {
            if d == Dialect::Postgres && q.is_kw("OIDS") {
                return Err(q.err("a table with OIDs"));
            }
            q.expect_kind(K::LParen)?;
            let (opts, end) = elements(&t[after..], q.pos())?;
            for o in opts {
                let first = t[after..][o].first().map(|x| x.up.as_str());
                let ok = match d {
                    Dialect::Tsql => {
                        matches!(first, Some("SYSTEM_VERSIONING" | "DATA_COMPRESSION"))
                    }
                    // Storage parameters (`fillfactor`, autovacuum settings).
                    _ => true,
                };
                if !ok {
                    return Err(format!("the table option {}", first.unwrap_or("")));
                }
            }
            q.i = end;
        } else if d == Dialect::Postgres && q.eat_kw("TABLESPACE") {
            q.ident()?;
        } else {
            return Err(q.err("a table option"));
        }
    }
    // Any `DEFERRABLE` (not `NOT DEFERRABLE`) or `INITIALLY` in the
    // statement, on the key or not: the table then has no key here.
    // `INITIALLY DEFERRED` alone makes a PostgreSQL key deferrable
    // (`condeferrable`), and a deferred key may hold two equal keys inside
    // a caller's transaction; `NOT DEFERRABLE INITIALLY IMMEDIATE` is
    // refused with it, the weaker reading.
    let words: Vec<&str> = t
        .iter()
        .filter(|x| x.kind == K::Word)
        .map(|x| x.up.as_str())
        .collect();
    let deferrable = words.iter().enumerate().any(|(k, x)| {
        *x == "INITIALLY" || (*x == "DEFERRABLE" && (k == 0 || words[k - 1] != "NOT"))
    });
    if let Some(k) = key {
        for c in k {
            let col = cols
                .iter_mut()
                .find(|x| same(d, &x.name, &c))
                .ok_or_else(|| format!("key column {c} is not a column"))?;
            if col.key {
                return Err(format!("key column {c} twice"));
            }
            col.key = true;
            col.not_null = true;
        }
    }
    if deferrable {
        for c in &mut cols {
            c.key = false;
        }
    }
    Ok(ReadTable { name, cols })
}

/// (C3d part 2) The name of the table a `CREATE TABLE` defines when the
/// statement cannot change another table: no PostgreSQL `INHERITS` or
/// `PARTITION OF` (the one makes the new table a child whose rows a read of
/// the parent returns, the other a partition of it). A table defined
/// `LIKE` another, `AS SELECT` from another, or with a foreign key to
/// another leaves the other's reads as they are; only its own name counts
/// then (`cert.rs`).
pub fn creates_other_table(n: &SyntaxNode, d: Dialect) -> Option<Vec<String>> {
    if n.kind() != SyntaxKind::STATEMENT {
        return None;
    }
    let t = tokens(n);
    if t.iter()
        .any(|x| x.kind == K::Word && (x.up == "INHERITS" || x.up == "PARTITION"))
    {
        return None;
    }
    let mut p = Parser::new(&t, d);
    p.expect_kw("CREATE").ok()?;
    while p.eat_kw("TEMP")
        || p.eat_kw("TEMPORARY")
        || p.eat_kw("GLOBAL")
        || p.eat_kw("LOCAL")
        || p.eat_kw("UNLOGGED")
    {}
    p.expect_kw("TABLE").ok()?;
    p.eat_kws(&["IF", "NOT", "EXISTS"]);
    p.qname().ok()
}

/// (C3d part 2) A statement that names a table but cannot change what a
/// read of it returns, so it does not keep a read-only table out
/// (`cert.rs`, `Tables::read`): an index of any kind (not T-SQL
/// `DROP_EXISTING`, which may rebuild the key's index); `ALTER TABLE` that
/// adds one constraint (`CHECK`, `UNIQUE`, `FOREIGN KEY`, `PRIMARY KEY`,
/// T-SQL `DEFAULT ... FOR`, PostgreSQL `EXCLUDE USING` or `EXCLUDE (`, as
/// `constraint_head` tells them from a column), validates or turns on or
/// off a constraint check (`[NO]CHECK CONSTRAINT`, `VALIDATE CONSTRAINT`),
/// or enables or disables triggers; T-SQL `ENABLE`/`DISABLE TRIGGER`;
/// PostgreSQL `CREATE TRIGGER` and `CREATE VIEW` (T-SQL triggers and views
/// are routines, read in `cert.rs`); `COMMENT ON`; T-SQL
/// `sp_addextendedproperty`, `sp_updateextendedproperty`,
/// `sp_dropextendedproperty`. Anything else naming the table keeps it
/// out: row-level security, rules, column changes, renames, drops,
/// grants, data written at the top level, ...
pub fn leaves_reads(n: &SyntaxNode, d: Dialect) -> bool {
    if n.kind() != SyntaxKind::STATEMENT {
        return false;
    }
    let t = tokens(n);
    let up = |i: usize| {
        t.get(i)
            .filter(|x| x.kind == K::Word)
            .map(|x| x.up.as_str())
    };
    let any = |w: &str| t.iter().any(|x| x.kind == K::Word && x.up == w);
    match (up(0), up(1)) {
        (Some("CREATE"), _) => {
            let mut i = 1;
            if up(1) == Some("OR") && up(2) == Some("REPLACE") {
                i = 3;
            }
            // An index of any kind.
            let mut j = i;
            while matches!(
                up(j),
                Some(
                    "UNIQUE"
                        | "CLUSTERED"
                        | "NONCLUSTERED"
                        | "COLUMNSTORE"
                        | "XML"
                        | "PRIMARY"
                        | "SPATIAL"
                        | "FULLTEXT"
                )
            ) {
                j += 1;
            }
            if up(j) == Some("INDEX") {
                return !any("DROP_EXISTING");
            }
            d == Dialect::Postgres
                && match up(i) {
                    Some("TRIGGER" | "VIEW" | "RECURSIVE") => true,
                    Some("CONSTRAINT" | "EVENT") => up(i + 1) == Some("TRIGGER"),
                    Some("MATERIALIZED") => up(i + 1) == Some("VIEW"),
                    Some("TEMP" | "TEMPORARY") => up(i + 1) == Some("VIEW"),
                    _ => false,
                }
        }
        (Some("ALTER"), Some("TABLE")) => alter_leaves_reads(&t, d).unwrap_or(false),
        (Some("ENABLE" | "DISABLE"), Some("TRIGGER")) => d == Dialect::Tsql,
        (Some("COMMENT"), Some("ON")) => d == Dialect::Postgres,
        (Some("EXEC" | "EXECUTE"), _) if d == Dialect::Tsql => {
            let mut p = Parser::new(&t, d);
            p.i = 1;
            let Ok(name) = p.qname() else { return false };
            let last = name
                .last()
                .map(|x| x.to_ascii_uppercase())
                .unwrap_or_default();
            (name.len() == 1 || (name.len() == 2 && name[0].eq_ignore_ascii_case("sys")))
                && matches!(
                    last.as_str(),
                    "SP_ADDEXTENDEDPROPERTY"
                        | "SP_UPDATEEXTENDEDPROPERTY"
                        | "SP_DROPEXTENDEDPROPERTY"
                )
        }
        _ => false,
    }
}

/// `ALTER TABLE` of one of the forms [`leaves_reads`] lists.
fn alter_leaves_reads(t: &[Tok], d: Dialect) -> Result<bool, String> {
    let mut p = Parser::new(t, d);
    p.expect_kw("ALTER")?;
    p.expect_kw("TABLE")?;
    if d == Dialect::Postgres {
        p.eat_kws(&["IF", "EXISTS"]);
        p.eat_kw("ONLY");
    }
    p.qname()?;
    if d == Dialect::Postgres {
        p.eat_op("*");
    }
    if d == Dialect::Tsql && p.eat_kw("WITH") && !p.eat_kw("CHECK") {
        p.expect_kw("NOCHECK")?;
    }
    let rest = &t[p.pos()..];
    let top_comma = {
        let mut depth = 0usize;
        rest.iter().any(|x| match x.kind {
            K::LParen => {
                depth += 1;
                false
            }
            K::RParen => {
                depth = depth.saturating_sub(1);
                false
            }
            K::Comma => depth == 0,
            _ => false,
        })
    };
    if p.eat_kw("ADD") {
        if top_comma {
            return Ok(false);
        }
        let named = p.eat_kw("CONSTRAINT");
        if named {
            p.ident()?;
        }
        // T-SQL `ADD [CONSTRAINT n] DEFAULT v FOR c` (`DEFAULT` is reserved);
        // no period or index is added here.
        let ok = (d == Dialect::Tsql && p.is_kw("DEFAULT"))
            || (p.peek().is_some_and(|x| x.kind == K::Word)
                && !matches!(p.peek().map(|x| x.up.as_str()), Some("PERIOD" | "INDEX"))
                && constraint_head(&t[p.pos()..], d, named));
        return Ok(ok);
    }
    // `[NO]CHECK CONSTRAINT {ALL | name, ...}`, `{ENABLE | DISABLE} TRIGGER
    // {ALL | name, ...}`, `VALIDATE CONSTRAINT name`: names only after.
    let names_only = |from: usize| {
        t[from..]
            .iter()
            .all(|x| matches!(x.kind, K::Word | K::Quoted | K::Comma))
    };
    if d == Dialect::Tsql && (p.eat_kw("CHECK") || p.eat_kw("NOCHECK")) {
        return Ok(p.eat_kw("CONSTRAINT") && names_only(p.pos()));
    }
    if d == Dialect::Postgres && p.eat_kw("VALIDATE") {
        return Ok(p.eat_kw("CONSTRAINT") && names_only(p.pos()) && !top_comma);
    }
    if p.eat_kw("ENABLE") || p.eat_kw("DISABLE") {
        if d == Dialect::Postgres && !p.eat_kw("ALWAYS") {
            p.eat_kw("REPLICA");
        }
        return Ok(p.eat_kw("TRIGGER") && names_only(p.pos()));
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stmt(sql: &str, d: Dialect) -> SyntaxNode {
        let root = crate::parse(sql, d).syntax();
        root.descendants()
            .find(|n| n.kind() == SyntaxKind::STATEMENT)
            .unwrap()
    }

    fn table(sql: &str, d: Dialect) -> Result<PlainTable, String> {
        read(&stmt(sql, d), d)
    }

    #[test]
    fn plain_tables_read_and_everything_else_is_refused() {
        let t = table(
            "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, Total int NOT NULL, \
             CONSTRAINT PK_Acct PRIMARY KEY CLUSTERED (Id)) ON [PRIMARY];",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(t.name, ["dbo", "Acct"]);
        let flags: Vec<(&str, bool, bool)> = t
            .cols
            .iter()
            .map(|c| (c.name.as_str(), c.not_null, c.key))
            .collect();
        assert_eq!(
            flags,
            [
                ("Id", true, true),
                ("Qty", false, false),
                ("Total", true, false)
            ]
        );
        let pg = table(
            "CREATE TABLE acct (id integer PRIMARY KEY, qty integer, total integer NOT NULL);",
            Dialect::Postgres,
        )
        .unwrap();
        assert_eq!(
            pg.cols[0],
            PlainCol {
                name: "id".into(),
                not_null: true,
                key: true,
                ty: ColType::Int,
                default: None,
            }
        );
        assert!(!pg.cols[1].not_null);
        for (sql, d) in [
            ("CREATE TABLE t (a int);", Dialect::Tsql),
            ("CREATE TABLE t (a int IDENTITY(1,1) NOT NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a int NULL CHECK (a > 0));", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL, b AS (a + 1));", Dialect::Tsql),
            ("CREATE TABLE #t (a int NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a decimal(10, 2) NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL, UNIQUE (a));", Dialect::Tsql),
            (
                "CREATE TABLE t (a int NOT NULL, CONSTRAINT pk PRIMARY KEY (a) WITH (IGNORE_DUP_KEY = ON));",
                Dialect::Tsql,
            ),
            ("CREATE TABLE t (a int NULL, PRIMARY KEY (b));", Dialect::Tsql),
            ("CREATE TABLE t (a int, a int);", Dialect::Postgres),
            ("CREATE TABLE IF NOT EXISTS t (a int);", Dialect::Postgres),
            ("CREATE TEMP TABLE t (a int);", Dialect::Postgres),
            ("CREATE UNLOGGED TABLE t (a int);", Dialect::Postgres),
            ("CREATE TABLE t (a int REFERENCES u (a));", Dialect::Postgres),
            ("CREATE TABLE t (a int) INHERITS (u);", Dialect::Postgres),
            ("CREATE TABLE t (a int PRIMARY KEY DEFERRABLE INITIALLY DEFERRED);", Dialect::Postgres),
            ("CREATE TABLE t (a int GENERATED ALWAYS AS IDENTITY);", Dialect::Postgres),
            ("CREATE TABLE t (a int NULL NOT NULL);", Dialect::Postgres),
            ("CREATE TABLE t (a int PRIMARY KEY, b int PRIMARY KEY);", Dialect::Postgres),
            ("CREATE TABLE t (LIKE u);", Dialect::Postgres),
        ] {
            assert!(table(sql, d).is_err(), "{sql}");
        }
    }

    #[test]
    fn string_columns_and_constant_defaults_are_plain() {
        let t = table(
            "CREATE TABLE dbo.Cust (Id int NOT NULL CONSTRAINT PK_Cust PRIMARY KEY, \
             Name varchar(20) NOT NULL, City nvarchar(30) NULL DEFAULT 'Paris', \
             Note varchar(10) NULL CONSTRAINT DF_Note DEFAULT (N'x'), \
             Qty int NOT NULL CONSTRAINT DF_Qty DEFAULT ((0)), Neg int NULL DEFAULT (-1), \
             Z int NULL DEFAULT NULL, W character varying(5) NULL);",
            Dialect::Tsql,
        )
        .unwrap();
        let got: Vec<(&str, ColType, Option<ColDefault>)> = t
            .cols
            .iter()
            .map(|c| (c.name.as_str(), c.ty, c.default.clone()))
            .collect();
        assert_eq!(
            got,
            [
                ("Id", ColType::Int, None),
                ("Name", ColType::Varchar(20), None),
                (
                    "City",
                    ColType::NVarchar(30),
                    Some(ColDefault::Str("Paris".into()))
                ),
                (
                    "Note",
                    ColType::Varchar(10),
                    Some(ColDefault::Str("x".into()))
                ),
                ("Qty", ColType::Int, Some(ColDefault::Int(0))),
                ("Neg", ColType::Int, Some(ColDefault::Int(-1))),
                ("Z", ColType::Int, None),
                ("W", ColType::Varchar(5), None),
            ]
        );
        // The converter's output for it.
        let pg = table(
            "CREATE TABLE Cust (Id integer NOT NULL CONSTRAINT PK_Cust PRIMARY KEY, \
             Name varchar(20) NOT NULL, City varchar(30) NULL DEFAULT 'Paris', \
             Note varchar(10) NULL CONSTRAINT DF_Note DEFAULT ('x'), \
             Qty integer NOT NULL CONSTRAINT DF_Qty DEFAULT ((0)), Neg integer NULL DEFAULT (-1), \
             Z integer NULL DEFAULT NULL, W character varying(5) NULL);",
            Dialect::Postgres,
        )
        .unwrap();
        let tys: Vec<ColType> = pg.cols.iter().map(|c| c.ty).collect();
        assert_eq!(tys[2], ColType::Varchar(30));
        assert_eq!(
            pg.cols
                .iter()
                .map(|c| c.default.clone())
                .collect::<Vec<_>>(),
            t.cols.iter().map(|c| c.default.clone()).collect::<Vec<_>>()
        );
        for (sql, d) in [
            // Lengths: max, none, 0, over 4000; nvarchar in PostgreSQL; char(n).
            ("CREATE TABLE t (a varchar(max) NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a varchar NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a varchar(0) NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a nvarchar(4001) NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a nvarchar(10));", Dialect::Postgres),
            ("CREATE TABLE t (a varchar);", Dialect::Postgres),
            ("CREATE TABLE t (a text);", Dialect::Postgres),
            ("CREATE TABLE t (a char(3) NULL);", Dialect::Tsql),
            (
                "CREATE TABLE t (a varchar(10) COLLATE Latin1_General_BIN2 NULL);",
                Dialect::Tsql,
            ),
            (
                "CREATE TABLE t (a varchar(10) COLLATE \"C\");",
                Dialect::Postgres,
            ),
            // A string key: SQL Server pads it (eval/tables T6).
            (
                "CREATE TABLE t (a varchar(10) NOT NULL PRIMARY KEY);",
                Dialect::Tsql,
            ),
            (
                "CREATE TABLE t (a varchar(10), PRIMARY KEY (a));",
                Dialect::Postgres,
            ),
            // Defaults that are not constants, or that do not fit.
            (
                "CREATE TABLE t (a int NULL DEFAULT GETDATE());",
                Dialect::Tsql,
            ),
            (
                "CREATE TABLE t (a int NULL DEFAULT NEXT VALUE FOR s);",
                Dialect::Tsql,
            ),
            (
                "CREATE TABLE t (a int DEFAULT nextval('s'));",
                Dialect::Postgres,
            ),
            (
                "CREATE TABLE t (a int NULL DEFAULT (1 + 1));",
                Dialect::Tsql,
            ),
            ("CREATE TABLE t (a int NULL DEFAULT 1.5);", Dialect::Tsql),
            (
                "CREATE TABLE t (a int NULL DEFAULT 2147483648);",
                Dialect::Tsql,
            ),
            ("CREATE TABLE t (a int NULL DEFAULT '1');", Dialect::Tsql),
            (
                "CREATE TABLE t (a varchar(3) NULL DEFAULT 1);",
                Dialect::Tsql,
            ),
            (
                "CREATE TABLE t (a varchar(3) NULL DEFAULT 'abcd');",
                Dialect::Tsql,
            ),
            (
                "CREATE TABLE t (a varchar(3) DEFAULT 'a\\b');",
                Dialect::Postgres,
            ),
            (
                "CREATE TABLE t (a varchar(3) DEFAULT N'a');",
                Dialect::Postgres,
            ),
            (
                "CREATE TABLE t (a varchar(3) DEFAULT 'a'::varchar);",
                Dialect::Postgres,
            ),
            (
                "CREATE TABLE t (a int NULL DEFAULT 1 DEFAULT 2);",
                Dialect::Tsql,
            ),
            ("CREATE TABLE t (a int NULL DEFAULT (1);", Dialect::Tsql),
        ] {
            assert!(table(sql, d).is_err(), "{sql}");
        }
        // Edge values that are in.
        let e = table(
            "CREATE TABLE t (a int NULL DEFAULT -2147483648, b varchar(3) NULL DEFAULT '', \
             c varchar(3) NULL DEFAULT 'a''b');",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(e.cols[0].default, Some(ColDefault::Int(i32::MIN)));
        assert_eq!(e.cols[1].default, Some(ColDefault::Str(String::new())));
        assert_eq!(e.cols[2].default, Some(ColDefault::Str("a'b".into())));
    }

    /// C2c: the typed-value columns (eval/values V8) and their defaults.
    #[test]
    fn typed_value_columns_are_plain() {
        let t = table(
            "CREATE TABLE dbo.Ev (D date NOT NULL CONSTRAINT pk_ev PRIMARY KEY, N bigint NULL, \
             S smallint NULL CONSTRAINT df_ev_s DEFAULT 0, B bit NOT NULL CONSTRAINT df_ev_b DEFAULT 0, \
             T datetime2(6) NULL CONSTRAINT df_ev_t DEFAULT '2000-01-01T00:00:00', \
             U datetime2(3) NULL DEFAULT '2024-02-29T23:59:59.999', W date NULL DEFAULT '2024-02-29');",
            Dialect::Tsql,
        )
        .unwrap();
        let got: Vec<(&str, ColType, Option<ColDefault>, bool)> = t
            .cols
            .iter()
            .map(|c| (c.name.as_str(), c.ty, c.default.clone(), c.key))
            .collect();
        assert_eq!(
            got,
            vec![
                ("D", ColType::Date, None, true),
                ("N", ColType::BigInt, None, false),
                ("S", ColType::SmallInt, Some(ColDefault::Int(0)), false),
                ("B", ColType::Bool, Some(ColDefault::Int(0)), false),
                (
                    "T",
                    ColType::Timestamp(6),
                    Some(ColDefault::Str("2000-01-01T00:00:00".into())),
                    false
                ),
                (
                    "U",
                    ColType::Timestamp(3),
                    Some(ColDefault::Str("2024-02-29T23:59:59.999".into())),
                    false
                ),
                (
                    "W",
                    ColType::Date,
                    Some(ColDefault::Str("2024-02-29".into())),
                    false
                ),
            ]
        );
        let pg = table(
            "CREATE TABLE ev (d date NOT NULL CONSTRAINT pk_ev PRIMARY KEY, n int8 NULL, \
             s int2 NULL DEFAULT 0, b boolean NOT NULL DEFAULT FALSE, \
             t timestamp(6) NULL DEFAULT '2000-01-01T00:00:00', u timestamp without time zone NULL, \
             v timestamp(3) without time zone NULL, w bool NULL DEFAULT TRUE, x timestamp NULL);",
            Dialect::Postgres,
        )
        .unwrap();
        let tys: Vec<(ColType, Option<ColDefault>)> =
            pg.cols.iter().map(|c| (c.ty, c.default.clone())).collect();
        assert_eq!(
            tys,
            vec![
                (ColType::Date, None),
                (ColType::BigInt, None),
                (ColType::SmallInt, Some(ColDefault::Int(0))),
                (ColType::Bool, Some(ColDefault::Bool(false))),
                (
                    ColType::Timestamp(6),
                    Some(ColDefault::Str("2000-01-01T00:00:00".into()))
                ),
                (ColType::Timestamp(6), None),
                (ColType::Timestamp(3), None),
                (ColType::Bool, Some(ColDefault::Bool(true))),
                (ColType::Timestamp(6), None),
            ]
        );
        // Outside: datetime2 without a precision or with 7, a bit key, a bit
        // default of 2, a date default in another form, a timestamp default
        // with more digits than kept, a smallint default out of range,
        // T-SQL `boolean`, PostgreSQL `bit`.
        for (sql, d) in [
            ("CREATE TABLE t (a datetime2 NULL, b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a datetime2(7) NULL, b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a timestamp(7) NULL, b int NOT NULL PRIMARY KEY);", Dialect::Postgres),
            ("CREATE TABLE t (a bit NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a bit NOT NULL DEFAULT 2, b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a date NULL DEFAULT '20240229', b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a date NULL DEFAULT '2023-02-29', b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a datetime2(3) NULL DEFAULT '2024-01-01T00:00:00.1234', b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a datetime2(3) NULL DEFAULT '2024-01-01 00:00:00', b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a smallint NULL DEFAULT 40000, b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a boolean NULL, b int NOT NULL PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a bit NULL, b int NOT NULL PRIMARY KEY);", Dialect::Postgres),
            ("CREATE TABLE t (a boolean NULL DEFAULT 1, b int NOT NULL PRIMARY KEY);", Dialect::Postgres),
            ("CREATE TABLE t (a timestamptz NULL, b int NOT NULL PRIMARY KEY);", Dialect::Postgres),
        ] {
            assert!(table(sql, d).is_err(), "{sql}");
        }
        assert!(
            is_iso_date("2024-02-29") && !is_iso_date("2024-02-30") && !is_iso_date("0000-01-01")
        );
        assert!(is_iso_timestamp("2024-02-29T23:59:59.999999", 6));
        assert!(!is_iso_timestamp("2024-02-29T23:59:59.9999999", 6));
        assert!(
            !is_iso_timestamp("2024-02-29T24:00:00", 6)
                && !is_iso_timestamp("2024-02-29T00:00:00.", 6)
        );
    }

    #[test]
    fn plain_indexes_are_non_unique_and_bounded() {
        let ix = |sql: &str, d| index(&stmt(sql, d), d);
        let t = table(
            "CREATE TABLE dbo.Cust (Id int NOT NULL PRIMARY KEY, Name varchar(20) NOT NULL, \
             Big nvarchar(1000) NULL, Mid varchar(900) NULL);",
            Dialect::Tsql,
        )
        .unwrap();
        let a = ix("CREATE INDEX IX_Name ON dbo.Cust (Name);", Dialect::Tsql).unwrap();
        assert_eq!(a.table, ["dbo", "Cust"]);
        assert_eq!(a.cols, ["Name"]);
        assert!(a.fits(&t, Dialect::Tsql).is_ok());
        let b = ix(
            "CREATE NONCLUSTERED INDEX IX_2 ON dbo.Cust (Name DESC, Id ASC) INCLUDE (Big) ON [PRIMARY];",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(b.include, ["Big"]);
        assert!(b.fits(&t, Dialect::Tsql).is_ok());
        // nvarchar(1000) is 2000 bytes: over 1700 (eval/tables T8).
        let c = ix("CREATE INDEX IX_3 ON dbo.Cust (Big);", Dialect::Tsql).unwrap();
        assert!(c.fits(&t, Dialect::Tsql).is_err());
        // 900 bytes fit a clustered key, 904 do not.
        let d = ix(
            "CREATE CLUSTERED INDEX IX_4 ON dbo.Cust (Mid);",
            Dialect::Tsql,
        )
        .unwrap();
        assert!(d.clustered && d.fits(&t, Dialect::Tsql).is_ok());
        let e = ix(
            "CREATE CLUSTERED INDEX IX_5 ON dbo.Cust (Mid, Id);",
            Dialect::Tsql,
        )
        .unwrap();
        assert!(e.fits(&t, Dialect::Tsql).is_err());
        // A column the table does not have.
        let f = ix("CREATE INDEX IX_6 ON dbo.Cust (Nope);", Dialect::Tsql).unwrap();
        assert!(f.fits(&t, Dialect::Tsql).is_err());
        let pt = table(
            "CREATE TABLE cust (id integer PRIMARY KEY, name varchar(20), big varchar(700));",
            Dialect::Postgres,
        )
        .unwrap();
        let g = ix(
            "CREATE INDEX ix_name ON cust USING btree (name) INCLUDE (id);",
            Dialect::Postgres,
        )
        .unwrap();
        assert!(g.fits(&pt, Dialect::Postgres).is_ok());
        // 700 characters may be 2800 bytes: over the btree entry limit, and
        // INCLUDE columns count (eval/tables T8).
        let h = ix("CREATE INDEX ix_big ON cust (big);", Dialect::Postgres).unwrap();
        assert!(h.fits(&pt, Dialect::Postgres).is_err());
        let i = ix(
            "CREATE INDEX ix_inc ON cust (id) INCLUDE (big);",
            Dialect::Postgres,
        )
        .unwrap();
        assert!(i.fits(&pt, Dialect::Postgres).is_err());
        for (sql, d) in [
            ("CREATE UNIQUE INDEX u ON dbo.Cust (Name);", Dialect::Tsql),
            (
                "CREATE INDEX f ON dbo.Cust (Name) WHERE Name IS NOT NULL;",
                Dialect::Tsql,
            ),
            (
                "CREATE INDEX w ON dbo.Cust (Name) WITH (FILLFACTOR = 80);",
                Dialect::Tsql,
            ),
            (
                "CREATE COLUMNSTORE INDEX c ON dbo.Cust (Name);",
                Dialect::Tsql,
            ),
            (
                "CREATE NONCLUSTERED COLUMNSTORE INDEX c ON dbo.Cust (Name);",
                Dialect::Tsql,
            ),
            ("CREATE UNIQUE INDEX u ON cust (name);", Dialect::Postgres),
            ("CREATE INDEX ON cust (name);", Dialect::Postgres),
            (
                "CREATE INDEX CONCURRENTLY c ON cust (name);",
                Dialect::Postgres,
            ),
            (
                "CREATE INDEX IF NOT EXISTS c ON cust (name);",
                Dialect::Postgres,
            ),
            ("CREATE INDEX c ON ONLY cust (name);", Dialect::Postgres),
            (
                "CREATE INDEX c ON cust USING hash (name);",
                Dialect::Postgres,
            ),
            ("CREATE INDEX c ON cust (lower(name));", Dialect::Postgres),
            (
                "CREATE INDEX c ON cust (name COLLATE \"C\");",
                Dialect::Postgres,
            ),
            (
                "CREATE INDEX c ON cust (name varchar_pattern_ops);",
                Dialect::Postgres,
            ),
            (
                "CREATE INDEX c ON cust (name) WHERE name IS NOT NULL;",
                Dialect::Postgres,
            ),
            (
                "CREATE INDEX c ON cust (name) WITH (fillfactor = 80);",
                Dialect::Postgres,
            ),
            (
                "CREATE INDEX c ON cust (name) TABLESPACE t;",
                Dialect::Postgres,
            ),
            (
                "CREATE INDEX c ON cust (name) NULLS NOT DISTINCT;",
                Dialect::Postgres,
            ),
        ] {
            assert!(ix(sql, d).is_err(), "{sql}");
        }
    }

    fn ro(sql: &str, d: Dialect) -> Result<ReadTable, String> {
        read_only(&stmt(sql, d), d)
    }

    /// `(name, slice type, NOT NULL, key)` of each column.
    fn shape(t: &ReadTable) -> Vec<(&str, Option<ColType>, bool, bool)> {
        t.cols
            .iter()
            .map(|c| (c.name.as_str(), c.ty, c.not_null, c.key))
            .collect()
    }

    /// C3d part 2: a read-only table reads past what no read sees, and a
    /// column it cannot take as stored is opaque.
    #[test]
    fn read_only_tables_read_past_what_reads_do_not_see() {
        let t = ro(
            "CREATE TABLE [Sales].[Line] (\n\
             [OrdId] int NOT NULL CONSTRAINT FK_L_O REFERENCES Sales.Ord (Id),\n\
             LineNo int NOT NULL,\n\
             Id int IDENTITY(1,1) NOT NULL,\n\
             Qty int NOT NULL CHECK (Qty > 0) DEFAULT (NEXT VALUE FOR Seq.Q),\n\
             Price decimal(12,2) NOT NULL,\n\
             Code char(4) NOT NULL,\n\
             Name varchar(20) NULL UNIQUE,\n\
             Note varchar(20) COLLATE Latin1_General_BIN2 NULL,\n\
             Added datetime2 NOT NULL DEFAULT SYSDATETIME(),\n\
             At datetime2(6) NULL,\n\
             Total AS (Qty * 2) PERSISTED,\n\
             ValidFrom datetime2(7) GENERATED ALWAYS AS ROW START NOT NULL,\n\
             ValidTo datetime2(7) GENERATED ALWAYS AS ROW END NOT NULL,\n\
             Secret varchar(20) MASKED WITH (FUNCTION = 'default()') NULL,\n\
             Flag bit NOT NULL DEFAULT 0,\n\
             CONSTRAINT [PK_Line] PRIMARY KEY CLUSTERED ([OrdId] ASC, [LineNo]),\n\
             CONSTRAINT UQ_L UNIQUE (Name),\n\
             INDEX IX_L NONCLUSTERED (Qty),\n\
             PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo)\n\
             ) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Line_Archive), DATA_COMPRESSION = PAGE);",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(t.name, ["Sales", "Line"]);
        assert_eq!(
            shape(&t),
            [
                ("OrdId", Some(ColType::Int), true, true),
                ("LineNo", Some(ColType::Int), true, true),
                ("Id", Some(ColType::Int), true, false),
                ("Qty", Some(ColType::Int), true, false),
                ("Price", None, true, false),
                ("Code", None, true, false),
                ("Name", Some(ColType::Varchar(20)), false, false),
                ("Note", None, false, false),
                ("Added", None, true, false),
                ("At", Some(ColType::Timestamp(6)), false, false),
                ("Total", None, false, false),
                ("ValidFrom", None, true, false),
                ("ValidTo", None, true, false),
                ("Secret", None, false, false),
                ("Flag", Some(ColType::Bool), true, false),
            ]
        );
        // PostgreSQL: identity and serial columns are integers; a stored or
        // virtual generated column, a time zone, an array, a collation are
        // opaque; storage parameters and a tablespace are table options.
        let t = ro(
            "CREATE UNLOGGED TABLE line (id integer GENERATED ALWAYS AS IDENTITY (START WITH 1) PRIMARY KEY,\n\
             n serial, b bigserial NOT NULL, twice integer GENERATED ALWAYS AS (n * 2) STORED,\n\
             at timestamp with time zone, xs integer[], s varchar(5) COLLATE \"C\",\n\
             t timestamp NOT NULL DEFAULT statement_timestamp()::timestamp,\n\
             r integer REFERENCES ord (id) ON DELETE SET NULL,\n\
             CONSTRAINT ex EXCLUDE USING gist (id WITH =)) WITH (fillfactor = 90) TABLESPACE ts;",
            Dialect::Postgres,
        )
        .unwrap();
        assert_eq!(
            shape(&t),
            [
                ("id", Some(ColType::Int), true, true),
                ("n", Some(ColType::Int), false, false),
                ("b", Some(ColType::BigInt), true, false),
                ("twice", None, false, false),
                ("at", None, false, false),
                ("xs", None, false, false),
                ("s", None, false, false),
                ("t", Some(ColType::Timestamp(6)), true, false),
                ("r", Some(ColType::Int), false, false),
            ]
        );
        // A deferrable key, or any deferrable constraint: no key at all.
        let t = ro(
            "CREATE TABLE t (a integer, b integer, PRIMARY KEY (a, b) DEFERRABLE INITIALLY DEFERRED);",
            Dialect::Postgres,
        )
        .unwrap();
        assert!(t.cols.iter().all(|c| !c.key));
        let t = ro(
            "CREATE TABLE t (a integer PRIMARY KEY NOT DEFERRABLE, b integer);",
            Dialect::Postgres,
        )
        .unwrap();
        assert!(t.cols[0].key);
        // What can change a read, or what the reader cannot read, is refused.
        for (sql, d) in [
            ("CREATE TABLE #t (a int NOT NULL);", Dialect::Tsql),
            ("CREATE TEMP TABLE t (a integer);", Dialect::Postgres),
            ("CREATE TABLE IF NOT EXISTS t (a integer);", Dialect::Postgres),
            ("CREATE TABLE c (a integer) INHERITS (p);", Dialect::Postgres),
            ("CREATE TABLE p (a integer) PARTITION BY RANGE (a);", Dialect::Postgres),
            ("CREATE TABLE c PARTITION OF p FOR VALUES FROM (1) TO (2);", Dialect::Postgres),
            ("CREATE TABLE c (LIKE p);", Dialect::Postgres),
            ("CREATE TABLE c AS SELECT 1 AS a;", Dialect::Postgres),
            ("CREATE TABLE t (a integer) USING columnar;", Dialect::Postgres),
            ("CREATE TABLE t (a integer) WITH OIDS;", Dialect::Postgres),
            ("CREATE TABLE t (a int NOT NULL PRIMARY KEY NONCLUSTERED) WITH (MEMORY_OPTIMIZED = ON);", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL) WITH (LEDGER = ON);", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL) ON ps (a);", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL) AS NODE;", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL, a int NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a int NULL NOT NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a int PRIMARY KEY, b int PRIMARY KEY);", Dialect::Tsql),
            ("CREATE TABLE t (a int, PRIMARY KEY (a), PRIMARY KEY (a));", Dialect::Tsql),
            ("CREATE TABLE t (a int, PRIMARY KEY (z));", Dialect::Tsql),
            ("CREATE TABLE t (CONSTRAINT c DEFAULT 1 FOR a);", Dialect::Tsql),
        ] {
            assert!(ro(sql, d).is_err(), "{sql}");
        }
    }

    /// C3d part 2, review: a word that starts a constraint in one dialect or
    /// form may be a column's name; read as a constraint, the column list
    /// would miss a real column and the checker an ambiguity (PostgreSQL
    /// 42702 confirmed as equal).
    #[test]
    fn constraint_words_that_name_columns_are_columns() {
        let names = |t: &ReadTable| t.cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        let t = ro(
            "CREATE TABLE t1 (id integer PRIMARY KEY, exclude integer, period integer, index integer, \
             CONSTRAINT ex EXCLUDE USING gist (id WITH =), EXCLUDE (id WITH =));",
            Dialect::Postgres,
        )
        .unwrap();
        assert_eq!(names(&t), ["id", "exclude", "period", "index"]);
        assert!(t.cols[1..].iter().all(|c| c.ty == Some(ColType::Int)));
        let t = ro(
            "CREATE TABLE dbo.T2 (K int NOT NULL PRIMARY KEY, period int NOT NULL, exclude int NULL, \
             [index] int NULL, INDEX ix NONCLUSTERED (K));",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(names(&t), ["K", "period", "exclude", "index"]);
        // The T-SQL period and the PostgreSQL exclusion constraint still read
        // as no column.
        let t = ro(
            "CREATE TABLE dbo.T3 (K int NOT NULL PRIMARY KEY, \
             F datetime2(7) GENERATED ALWAYS AS ROW START NOT NULL, \
             T datetime2(7) GENERATED ALWAYS AS ROW END NOT NULL, PERIOD FOR SYSTEM_TIME (F, T));",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(names(&t), ["K", "F", "T"]);
        // Not a constraint in the other dialect: a column (opaque, its type
        // unread) or refused, never skipped.
        assert!(ro(
            "CREATE TABLE t (a int NULL, EXCLUDE (a WITH =));",
            Dialect::Tsql
        )
        .map_or(true, |t| names(&t).contains(&"EXCLUDE".to_string())));
        assert!(ro(
            "CREATE TABLE t (a integer, PERIOD FOR SYSTEM_TIME (a, a));",
            Dialect::Postgres
        )
        .map_or(true, |t| names(&t).contains(&"PERIOD".to_string())));
        // `ALTER TABLE ... ADD`: a column of that name keeps the table out.
        let leaves = |sql: &str, d: Dialect| leaves_reads(&stmt(sql, d), d);
        for (sql, d) in [
            ("ALTER TABLE t1 ADD exclude integer;", Dialect::Postgres),
            ("ALTER TABLE t1 ADD period integer;", Dialect::Postgres),
            ("ALTER TABLE t1 ADD index integer;", Dialect::Postgres),
            ("ALTER TABLE dbo.t1 ADD exclude int NULL;", Dialect::Tsql),
            ("ALTER TABLE dbo.t1 ADD period int NULL;", Dialect::Tsql),
            (
                "ALTER TABLE dbo.t1 ADD PERIOD FOR SYSTEM_TIME (F, T);",
                Dialect::Tsql,
            ),
            ("ALTER TABLE dbo.t1 ADD EXCLUDE (a WITH =);", Dialect::Tsql),
            (
                "ALTER TABLE t1 ADD CONSTRAINT c PERIOD FOR SYSTEM_TIME (F, T);",
                Dialect::Tsql,
            ),
        ] {
            assert!(!leaves(sql, d), "{sql}");
        }
        for (sql, d) in [
            (
                "ALTER TABLE t1 ADD EXCLUDE USING gist (a WITH =);",
                Dialect::Postgres,
            ),
            ("ALTER TABLE t1 ADD EXCLUDE (a WITH =);", Dialect::Postgres),
            (
                "ALTER TABLE t1 ADD CONSTRAINT c CHECK (a > 0);",
                Dialect::Postgres,
            ),
            ("ALTER TABLE dbo.t1 ADD DEFAULT 0 FOR a;", Dialect::Tsql),
        ] {
            assert!(leaves(sql, d), "{sql}");
        }
    }

    /// C3d part 2, review: only `GENERATED {ALWAYS | BY DEFAULT} AS
    /// IDENTITY` is an identity column; a generated column followed by a
    /// constraint named `identity` raises where SQL Server returns the
    /// stored value (22012).
    #[test]
    fn only_an_identity_is_read_through_generated() {
        let t = ro(
            "CREATE TABLE t1 (id integer PRIMARY KEY, \
             v integer GENERATED ALWAYS AS (1/(id-id)) CONSTRAINT identity CHECK (true), \
             w integer GENERATED ALWAYS AS (id) STORED CONSTRAINT identity2 CHECK (true), \
             a integer GENERATED ALWAYS AS IDENTITY, b integer GENERATED BY DEFAULT AS IDENTITY);",
            Dialect::Postgres,
        )
        .unwrap();
        assert_eq!(
            shape(&t),
            [
                ("id", Some(ColType::Int), true, true),
                ("v", None, false, false),
                ("w", None, false, false),
                ("a", Some(ColType::Int), false, false),
                ("b", Some(ColType::Int), false, false),
            ]
        );
    }

    /// C3d part 2, review: `INITIALLY DEFERRED` alone makes a PostgreSQL key
    /// deferrable; any `INITIALLY` leaves the table without a key.
    #[test]
    fn initially_deferred_keeps_no_key() {
        for sql in [
            "CREATE TABLE t (a integer PRIMARY KEY INITIALLY DEFERRED, b integer);",
            "CREATE TABLE t (a integer, b integer, PRIMARY KEY (a) INITIALLY DEFERRED);",
            "CREATE TABLE t (a integer PRIMARY KEY, b integer UNIQUE INITIALLY DEFERRED);",
            "CREATE TABLE t (a integer PRIMARY KEY NOT DEFERRABLE INITIALLY IMMEDIATE);",
        ] {
            let t = ro(sql, Dialect::Postgres).unwrap();
            assert!(t.cols.iter().all(|c| !c.key), "{sql}");
        }
    }

    /// C3d part 2, review: the words of a `CASE` in a default say nothing of
    /// the column (`IS NOT NULL` there is no `NOT NULL`).
    #[test]
    fn a_case_in_a_default_says_nothing_of_the_column() {
        let t = ro(
            "CREATE TABLE dbo.t (k int NOT NULL PRIMARY KEY, \
             a int DEFAULT CASE WHEN 1 IS NOT NULL THEN 1 END, \
             b int DEFAULT CASE WHEN CASE WHEN 1 = 1 THEN 1 END IS NOT NULL THEN 1 END NOT NULL);",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(
            shape(&t),
            [
                ("k", Some(ColType::Int), true, true),
                ("a", Some(ColType::Int), false, false),
                ("b", Some(ColType::Int), true, false),
            ]
        );
    }

    /// C3d part 2: the statements that cannot change what a read returns.
    #[test]
    fn statements_that_leave_reads_alone() {
        let leaves = |sql: &str, d: Dialect| leaves_reads(&stmt(sql, d), d);
        for (sql, d) in [
            ("CREATE UNIQUE NONCLUSTERED INDEX ix ON dbo.t (a) WHERE a > 0;", Dialect::Tsql),
            ("CREATE NONCLUSTERED COLUMNSTORE INDEX ix ON dbo.t (a);", Dialect::Tsql),
            ("CREATE INDEX CONCURRENTLY IF NOT EXISTS ix ON t USING gin (a);", Dialect::Postgres),
            ("ALTER TABLE [dbo].[t] ADD CONSTRAINT [FK_t] FOREIGN KEY ([a]) REFERENCES [dbo].[u] ([id]);", Dialect::Tsql),
            ("ALTER TABLE dbo.t WITH NOCHECK ADD CONSTRAINT ck CHECK (a > 0);", Dialect::Tsql),
            ("ALTER TABLE dbo.t ADD CONSTRAINT df DEFAULT (0) FOR a;", Dialect::Tsql),
            ("ALTER TABLE dbo.t ADD UNIQUE (a);", Dialect::Tsql),
            ("ALTER TABLE dbo.t NOCHECK CONSTRAINT ALL;", Dialect::Tsql),
            ("ALTER TABLE dbo.t CHECK CONSTRAINT fk1, fk2;", Dialect::Tsql),
            ("ALTER TABLE dbo.t DISABLE TRIGGER ALL;", Dialect::Tsql),
            ("DISABLE TRIGGER tr ON dbo.t;", Dialect::Tsql),
            ("EXEC sys.sp_addextendedproperty @name = N'd', @value = N'x', @level0type = N'SCHEMA', @level0name = 'dbo', @level1type = N'TABLE', @level1name = 't';", Dialect::Tsql),
            ("ALTER TABLE ONLY t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id) NOT VALID;", Dialect::Postgres),
            ("ALTER TABLE t VALIDATE CONSTRAINT fk;", Dialect::Postgres),
            ("ALTER TABLE t ENABLE ALWAYS TRIGGER tr;", Dialect::Postgres),
            ("ALTER TABLE t ADD CONSTRAINT ex EXCLUDE USING gist (a WITH =);", Dialect::Postgres),
            ("CREATE OR REPLACE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();", Dialect::Postgres),
            ("CREATE CONSTRAINT TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();", Dialect::Postgres),
            ("CREATE VIEW v AS SELECT a FROM t;", Dialect::Postgres),
            ("CREATE MATERIALIZED VIEW v AS SELECT a FROM t;", Dialect::Postgres),
            ("COMMENT ON TABLE t IS 'x';", Dialect::Postgres),
        ] {
            assert!(leaves(sql, d), "{sql}");
        }
        for (sql, d) in [
            (
                "CREATE UNIQUE CLUSTERED INDEX pk ON dbo.t (a) WITH (DROP_EXISTING = ON);",
                Dialect::Tsql,
            ),
            ("ALTER TABLE dbo.t ADD b int NULL;", Dialect::Tsql),
            (
                "ALTER TABLE dbo.t ADD CONSTRAINT ck CHECK (a > 0), b int NULL;",
                Dialect::Tsql,
            ),
            (
                "ALTER TABLE dbo.t ALTER COLUMN a bigint NOT NULL;",
                Dialect::Tsql,
            ),
            ("ALTER TABLE dbo.t DROP CONSTRAINT pk_t;", Dialect::Tsql),
            (
                "ALTER TABLE dbo.t SET (SYSTEM_VERSIONING = OFF);",
                Dialect::Tsql,
            ),
            ("ALTER TABLE dbo.t SWITCH TO dbo.u;", Dialect::Tsql),
            (
                "CREATE SECURITY POLICY p ADD FILTER PREDICATE dbo.f(a) ON dbo.t;",
                Dialect::Tsql,
            ),
            ("ALTER SECURITY POLICY p WITH (STATE = ON);", Dialect::Tsql),
            ("GRANT SELECT ON dbo.t TO u;", Dialect::Tsql),
            ("DENY SELECT ON dbo.t (a) TO u;", Dialect::Tsql),
            ("INSERT INTO dbo.t (a) VALUES (1);", Dialect::Tsql),
            ("TRUNCATE TABLE dbo.t;", Dialect::Tsql),
            ("DROP TABLE dbo.t;", Dialect::Tsql),
            ("EXEC sp_rename 'dbo.t', 'u';", Dialect::Tsql),
            ("EXEC dbo.sp_addextendedproperty 'x';", Dialect::Tsql),
            ("CREATE POLICY p ON t USING (a = 1);", Dialect::Postgres),
            (
                "ALTER TABLE t ENABLE ROW LEVEL SECURITY;",
                Dialect::Postgres,
            ),
            ("ALTER TABLE t FORCE ROW LEVEL SECURITY;", Dialect::Postgres),
            ("ALTER TABLE t ENABLE RULE r;", Dialect::Postgres),
            (
                "CREATE RULE r AS ON SELECT TO t DO INSTEAD SELECT 1;",
                Dialect::Postgres,
            ),
            ("ALTER TABLE c INHERIT t;", Dialect::Postgres),
            (
                "ALTER TABLE t ATTACH PARTITION c FOR VALUES IN (1);",
                Dialect::Postgres,
            ),
            (
                "ALTER TABLE t ALTER CONSTRAINT pk DEFERRABLE;",
                Dialect::Postgres,
            ),
            ("ALTER TABLE t ADD COLUMN b integer;", Dialect::Postgres),
            ("ALTER TABLE t ADD b integer;", Dialect::Postgres),
            ("ALTER TABLE t RENAME TO u;", Dialect::Postgres),
            ("ALTER TABLE t OWNER TO r;", Dialect::Postgres),
            ("COMMENT ON TABLE t IS 'x';", Dialect::Tsql),
        ] {
            assert!(!leaves(sql, d), "{sql}");
        }
        // Another table's definition names only itself; inheritance and
        // partitions name the parent.
        let other =
            |sql: &str| creates_other_table(&stmt(sql, Dialect::Postgres), Dialect::Postgres);
        assert_eq!(
            other("CREATE TABLE a.x (LIKE a.t);"),
            Some(vec!["a".into(), "x".into()])
        );
        assert_eq!(
            other("CREATE TEMP TABLE x AS SELECT * FROM t;"),
            Some(vec!["x".into()])
        );
        assert_eq!(other("CREATE TABLE x (a integer) INHERITS (t);"), None);
        assert_eq!(other("CREATE TABLE x PARTITION OF t DEFAULT;"), None);
        assert_eq!(other("DROP TABLE t;"), None);
    }
}
