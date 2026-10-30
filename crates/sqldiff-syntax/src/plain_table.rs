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

use crate::abstract_syntax::{string_value, tokens, Parser, K};
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
}
