//! `CREATE TABLE` compared across dialects: T-SQL against PostgreSQL.
//!
//! Each statement is read completely into a table model: its name, its
//! columns (type, nullability, identity, default) and its constraints
//! (primary key, unique, foreign key, check). A clause the model does not
//! know leaves the table not compared: nothing is skipped silently, and
//! only clauses that change nothing but storage (filegroups, compression,
//! clustering, fill factor) are passed over, as normalizations.
//!
//! The two models are then compared column by column and constraint by
//! constraint. Types are compared by the values each column stores and
//! rejects (`types`); defaults and check predicates are compared by the
//! statement engine (`mod.rs`), as `SELECT default` and
//! `SELECT 1 FROM t WHERE NOT (predicate)`: a check rejects exactly the
//! rows its predicate makes false.
//!
//! Facts used (SQL Server 2022, PostgreSQL 18), see
//! `docs/cross-dialect-proofs.md`, "Tables".

use sqldiff_syntax::Dialect;

use super::ast::{self, Tok, K};

/// A name part as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub quoted: bool,
}

/// A column type, read for the values it stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    /// Signed integer of this many bits.
    Int(u8),
    /// T-SQL tinyint: 0 to 255.
    TinyInt,
    /// T-SQL bit.
    Bit,
    Bool,
    /// Precision and scale; `None` is PostgreSQL's unconstrained numeric.
    Dec(Option<(u32, u32)>),
    Money,
    SmallMoney,
    /// IEEE single (`false`) or double (`true`) precision.
    Float(bool),
    /// Character string: blank-padded (`fixed`), UTF-16 (`national`, T-SQL
    /// nchar/nvarchar), length limit (`None`: max or unbounded).
    Str {
        fixed: bool,
        national: bool,
        len: Option<u32>,
    },
    Date,
    /// Time of day with this many fraction digits.
    Time(u32),
    DateTime,
    SmallDateTime,
    DateTime2(u32),
    Timestamp(u32),
    TimestampTz(u32),
    DateTimeOffset(u32),
    Uuid,
    /// Byte string: zero-padded (`fixed`), length limit (`None`: max or
    /// unbounded).
    Binary {
        fixed: bool,
        len: Option<u32>,
    },
}

/// A sequence-generated column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub start: i128,
    pub step: i128,
    /// An INSERT cannot give the column a value (T-SQL without
    /// IDENTITY_INSERT, PostgreSQL GENERATED ALWAYS).
    pub always: bool,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: Name,
    pub ty: Ty,
    /// The type as written.
    pub ty_text: String,
    /// `Some(true)` for NULL, `Some(false)` for NOT NULL, `None` unsaid.
    pub null: Option<bool>,
    pub identity: Option<Identity>,
    /// The default expression's tokens.
    pub default: Option<Vec<Tok>>,
    /// Byte offset of the column definition.
    pub off: usize,
}

#[derive(Debug, Clone)]
pub struct Key {
    pub primary: bool,
    pub name: Option<Name>,
    pub cols: Vec<Name>,
    /// PostgreSQL `UNIQUE NULLS NOT DISTINCT`.
    pub nulls_not_distinct: bool,
    pub off: usize,
}

#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub name: Option<Name>,
    pub cols: Vec<Name>,
    pub table: Vec<Name>,
    /// `None`: the referenced table's primary key.
    pub ref_cols: Option<Vec<Name>>,
    pub on_delete: String,
    pub on_update: String,
    pub off: usize,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: Option<Name>,
    pub pred: Vec<Tok>,
    pub off: usize,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub dialect: Dialect,
    pub name: Vec<Name>,
    /// The name's tokens, for statements built around the table.
    pub name_toks: Vec<Tok>,
    pub columns: Vec<Column>,
    pub keys: Vec<Key>,
    pub fks: Vec<ForeignKey>,
    pub checks: Vec<Check>,
    /// Storage clauses passed over, as written.
    pub storage: Vec<String>,
}

type R<T> = Result<T, String>;

pub(super) fn word(t: Option<&Tok>, w: &str) -> bool {
    t.is_some_and(|t| t.kind == K::Word && t.up == w)
}

pub(super) fn name_of(t: &Tok) -> Option<Name> {
    match t.kind {
        K::Word => Some(Name {
            text: t.text.clone(),
            quoted: false,
        }),
        K::Quoted => Some(Name {
            text: sqldiff_syntax::ast::unquote(&t.text),
            quoted: true,
        }),
        _ => None,
    }
}

pub(super) fn text(ts: &[Tok]) -> String {
    let mut s = String::new();
    for (i, t) in ts.iter().enumerate() {
        let glue = i == 0
            || matches!(t.kind, K::Comma | K::RParen | K::Dot)
            || matches!(ts[i - 1].kind, K::LParen | K::Dot);
        if !glue {
            s.push(' ');
        }
        s.push_str(&t.text);
    }
    s
}

/// The index of the parenthesis closing the one at `open`.
pub(super) fn close(ts: &[Tok], open: usize) -> R<usize> {
    let mut depth = 0usize;
    for (i, t) in ts.iter().enumerate().skip(open) {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => {
                depth = depth.checked_sub(1).ok_or("unbalanced parentheses")?;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    Err("unbalanced parentheses".into())
}

/// Splits at commas outside parentheses.
pub(super) fn split(ts: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, t) in ts.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
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

/// A dotted name starting at `i`: its parts and the index after it.
pub(super) fn dotted(ts: &[Tok], mut i: usize) -> R<(Vec<Name>, usize)> {
    let mut parts = Vec::new();
    loop {
        let n = ts
            .get(i)
            .and_then(name_of)
            .ok_or_else(|| format!("expected a name at `{}`", text(&ts[i.min(ts.len())..])))?;
        parts.push(n);
        i += 1;
        if ts.get(i).is_some_and(|t| t.kind == K::Dot) {
            i += 1;
        } else {
            return Ok((parts, i));
        }
    }
}

/// `( a [ASC|DESC], b ... )` at `i`: the names and the index after it. A
/// sort order shapes only the key's index.
fn col_list(ts: &[Tok], i: usize, sorted: bool) -> R<(Vec<Name>, usize)> {
    if !ts.get(i).is_some_and(|t| t.kind == K::LParen) {
        return Err("expected a column list".into());
    }
    let end = close(ts, i)?;
    let mut cols = Vec::new();
    for item in split(&ts[i + 1..end]) {
        let ok = match item {
            [n] => name_of(n),
            [n, o] if sorted && (word(Some(o), "ASC") || word(Some(o), "DESC")) => name_of(n),
            _ => None,
        };
        cols.push(ok.ok_or_else(|| format!("column list item `{}`", text(item)))?);
    }
    Ok((cols, end + 1))
}

/// A signed whole number at `i`, and the index after it.
pub(super) fn number(ts: &[Tok], i: usize) -> R<(i128, usize)> {
    let (neg, j) = match ts.get(i) {
        Some(t) if t.kind == K::Op && (t.text == "-" || t.text == "+") => (t.text == "-", i + 1),
        _ => (false, i),
    };
    let t = ts
        .get(j)
        .filter(|t| t.kind == K::Num)
        .ok_or("expected a number")?;
    let v: i128 = t
        .text
        .parse()
        .map_err(|_| format!("`{}` is not a whole number", t.text))?;
    Ok((if neg { -v } else { v }, j + 1))
}

/// Index options of a T-SQL key that only shape its index. IGNORE_DUP_KEY
/// = ON changes what an INSERT does, so it is not among them.
pub(super) const TSQL_INDEX_STORAGE: &[&str] = &[
    "PAD_INDEX",
    "FILLFACTOR",
    "STATISTICS_NORECOMPUTE",
    "STATISTICS_INCREMENTAL",
    "ALLOW_ROW_LOCKS",
    "ALLOW_PAGE_LOCKS",
    "OPTIMIZE_FOR_SEQUENTIAL_KEY",
    "DATA_COMPRESSION",
    "XML_COMPRESSION",
    "SORT_IN_TEMPDB",
];

/// `WITH ( name = value, ... )` at `i` whose names are all `allowed`
/// (`IGNORE_DUP_KEY = OFF` too, the default): the index after it.
pub(super) fn storage_options(
    ts: &[Tok],
    i: usize,
    allowed: &[&str],
    storage: &mut Vec<String>,
) -> R<usize> {
    let open = i + 1;
    if !ts.get(open).is_some_and(|t| t.kind == K::LParen) {
        return Err("WITH without an option list".into());
    }
    let end = close(ts, open)?;
    for opt in split(&ts[open + 1..end]) {
        let ok = match opt {
            [n, eq, ..] if eq.kind == K::Op && eq.text == "=" => {
                allowed.iter().any(|a| n.up == *a)
                    || (n.up == "IGNORE_DUP_KEY" && opt.len() == 3 && opt[2].up == "OFF")
            }
            _ => false,
        };
        if !ok {
            return Err(format!("option `{}`", text(opt)));
        }
    }
    storage.push(text(&ts[i..=end]));
    Ok(end + 1)
}

/// T-SQL `ON filegroup` / `ON "default"` after a key or the table.
pub(super) fn filegroup(ts: &[Tok], i: usize, storage: &mut Vec<String>) -> R<usize> {
    let n = ts.get(i + 1).ok_or("ON without a filegroup")?;
    if !matches!(n.kind, K::Word | K::Quoted | K::Str) {
        return Err(format!("ON `{}`", n.text));
    }
    if ts.get(i + 2).is_some_and(|t| t.kind == K::LParen) {
        return Err("a partition scheme".into());
    }
    storage.push(text(&ts[i..i + 2]));
    Ok(i + 2)
}

/// Reads a whole `CREATE TABLE` statement (terminator dropped).
pub fn parse(ts: &[Tok], d: Dialect) -> R<Table> {
    if !word(ts.first(), "CREATE") {
        return Err("not a CREATE TABLE".into());
    }
    let mut i = 1;
    if d == Dialect::Postgres {
        if let Some(t) = ts.get(i).filter(|t| t.kind == K::Word) {
            if matches!(
                t.up.as_str(),
                "TEMP" | "TEMPORARY" | "UNLOGGED" | "GLOBAL" | "LOCAL"
            ) {
                return Err(format!("a {} table", t.up));
            }
        }
    }
    if !word(ts.get(i), "TABLE") {
        return Err("not a CREATE TABLE".into());
    }
    i += 1;
    if word(ts.get(i), "IF") {
        return Err("CREATE TABLE IF NOT EXISTS (it does nothing when the table exists)".into());
    }
    if ts.get(i).is_some_and(|t| t.kind == K::Op && t.text == "#") {
        return Err("a temporary table".into());
    }
    let (name, j) = dotted(ts, i)?;
    if name.iter().any(|n| n.text.starts_with('#')) {
        return Err("a temporary table".into());
    }
    if name.len() > 2 {
        return Err("a name with a database or server part".into());
    }
    let name_toks = ts[i..j].to_vec();
    i = j;
    if !ts.get(i).is_some_and(|t| t.kind == K::LParen) {
        return Err("CREATE TABLE without a column list (AS, OF, PARTITION OF, LIKE)".into());
    }
    let end = close(ts, i)?;
    let mut t = Table {
        dialect: d,
        name,
        name_toks,
        columns: Vec::new(),
        keys: Vec::new(),
        fks: Vec::new(),
        checks: Vec::new(),
        storage: Vec::new(),
    };
    for item in split(&ts[i + 1..end]) {
        let first = item.first().ok_or("an empty item in the column list")?;
        let constraint = first.kind == K::Word
            && matches!(
                first.up.as_str(),
                "CONSTRAINT"
                    | "PRIMARY"
                    | "UNIQUE"
                    | "FOREIGN"
                    | "CHECK"
                    | "INDEX"
                    | "PERIOD"
                    | "LIKE"
                    | "EXCLUDE"
            );
        if constraint {
            table_constraint(item, d, &mut t)?;
        } else {
            column(item, d, &mut t)?;
        }
    }
    tail(&ts[end + 1..], d, &mut t)?;
    if t.columns.is_empty() {
        return Err("a table without columns".into());
    }
    Ok(t)
}

fn tail(ts: &[Tok], d: Dialect, t: &mut Table) -> R<()> {
    let mut i = 0;
    while i < ts.len() {
        let w = ts[i].up.as_str();
        i = match (d, w) {
            (Dialect::Tsql, "ON" | "TEXTIMAGE_ON") => filegroup(ts, i, &mut t.storage)?,
            (Dialect::Tsql, "WITH") => storage_options(
                ts,
                i,
                &["DATA_COMPRESSION", "XML_COMPRESSION"],
                &mut t.storage,
            )?,
            (Dialect::Postgres, "WITH") => storage_options(
                ts,
                i,
                &[
                    "FILLFACTOR",
                    "TOAST_TUPLE_TARGET",
                    "AUTOVACUUM_ENABLED",
                    "TOAST.AUTOVACUUM_ENABLED",
                    "PARALLEL_WORKERS",
                ],
                &mut t.storage,
            )?,
            (Dialect::Postgres, "TABLESPACE") => {
                let n = ts
                    .get(i + 1)
                    .and_then(name_of)
                    .ok_or("TABLESPACE without a name")?;
                t.storage.push(format!("TABLESPACE {}", n.text));
                i + 2
            }
            _ => return Err(format!("the table option `{}`", text(&ts[i..]))),
        };
    }
    Ok(())
}

/// Index after `CLUSTERED` / `NONCLUSTERED` (T-SQL): the clustering is storage.
fn clustering(ts: &[Tok], i: usize, d: Dialect, storage: &mut Vec<String>) -> usize {
    if d == Dialect::Tsql && (word(ts.get(i), "CLUSTERED") || word(ts.get(i), "NONCLUSTERED")) {
        storage.push(ts[i].up.clone());
        i + 1
    } else {
        i
    }
}

/// Index options and filegroup after a key's column list.
fn key_tail(ts: &[Tok], mut i: usize, d: Dialect, storage: &mut Vec<String>) -> R<usize> {
    loop {
        match (d, ts.get(i).map(|t| t.up.as_str())) {
            (Dialect::Tsql, Some("WITH")) => {
                i = storage_options(ts, i, TSQL_INDEX_STORAGE, storage)?
            }
            (Dialect::Tsql, Some("ON")) => i = filegroup(ts, i, storage)?,
            (Dialect::Postgres, Some("WITH")) if !word(ts.get(i + 1), "TIME") => {
                i = storage_options(ts, i, &["FILLFACTOR", "DEDUPLICATE_ITEMS"], storage)?
            }
            _ => return Ok(i),
        }
    }
}

/// `UNIQUE` at `i`: whether NULLS NOT DISTINCT follows, and the index after.
fn unique_nulls(ts: &[Tok], i: usize, d: Dialect) -> R<(bool, usize)> {
    if d == Dialect::Postgres && word(ts.get(i), "NULLS") {
        return match (word(ts.get(i + 1), "NOT"), word(ts.get(i + 2), "DISTINCT")) {
            (true, true) => Ok((true, i + 3)),
            _ if word(ts.get(i + 1), "DISTINCT") => Ok((false, i + 2)),
            _ => Err("UNIQUE NULLS without DISTINCT".into()),
        };
    }
    Ok((false, i))
}

/// Referential actions and match type after `REFERENCES t (c)`.
fn fk_tail(ts: &[Tok], mut i: usize, d: Dialect, fk: &mut ForeignKey) -> R<usize> {
    loop {
        if word(ts.get(i), "ON") && (word(ts.get(i + 1), "DELETE") || word(ts.get(i + 1), "UPDATE"))
        {
            let (action, n) = match ts.get(i + 2).map(|t| t.up.as_str()) {
                Some("CASCADE") => ("CASCADE", 3),
                Some("RESTRICT") if d == Dialect::Postgres => ("RESTRICT", 3),
                Some("NO") if word(ts.get(i + 3), "ACTION") => ("NO ACTION", 4),
                Some("SET") if word(ts.get(i + 3), "NULL") => ("SET NULL", 4),
                Some("SET") if word(ts.get(i + 3), "DEFAULT") => ("SET DEFAULT", 4),
                _ => return Err(format!("the referential action `{}`", text(&ts[i..]))),
            };
            if ts.get(i + n).is_some_and(|t| t.kind == K::LParen) {
                return Err("a referential action with a column list".into());
            }
            if ts[i + 1].up == "DELETE" {
                fk.on_delete = action.into();
            } else {
                fk.on_update = action.into();
            }
            i += n;
        } else if d == Dialect::Postgres && word(ts.get(i), "MATCH") {
            // MATCH SIMPLE, the default, is T-SQL's rule: a key with a NULL
            // part is not checked.
            if !word(ts.get(i + 1), "SIMPLE") {
                return Err(format!("MATCH {}", ts.get(i + 1).map_or("", |t| &t.up)));
            }
            i += 2;
        } else {
            return Ok(i);
        }
    }
}

/// `REFERENCES t [(c, ...)] [actions]` at `i`.
fn references(
    ts: &[Tok],
    i: usize,
    d: Dialect,
    name: Option<Name>,
    cols: Vec<Name>,
    off: usize,
) -> R<(ForeignKey, usize)> {
    let (table, mut j) = dotted(ts, i + 1)?;
    let ref_cols = if ts.get(j).is_some_and(|t| t.kind == K::LParen) {
        let (c, k) = col_list(ts, j, false)?;
        j = k;
        Some(c)
    } else {
        None
    };
    let mut fk = ForeignKey {
        name,
        cols,
        table,
        ref_cols,
        on_delete: "NO ACTION".into(),
        on_update: "NO ACTION".into(),
        off,
    };
    j = fk_tail(ts, j, d, &mut fk)?;
    Ok((fk, j))
}

/// `CHECK (predicate)` at `i`.
fn check(ts: &[Tok], i: usize, d: Dialect, name: Option<Name>) -> R<(Check, usize)> {
    let mut j = i + 1;
    if d == Dialect::Tsql && word(ts.get(j), "NOT") && word(ts.get(j + 1), "FOR") {
        return Err("CHECK NOT FOR REPLICATION".into());
    }
    if !ts.get(j).is_some_and(|t| t.kind == K::LParen) {
        return Err("CHECK without a parenthesised predicate".into());
    }
    let end = close(ts, j)?;
    let pred = ts[j + 1..end].to_vec();
    j = end + 1;
    if d == Dialect::Postgres && word(ts.get(j), "NO") && word(ts.get(j + 1), "INHERIT") {
        return Err("CHECK … NO INHERIT".into());
    }
    if d == Dialect::Postgres && word(ts.get(j), "NOT") && word(ts.get(j + 1), "VALID") {
        return Err("CHECK … NOT VALID (existing rows are not checked)".into());
    }
    Ok((
        Check {
            name,
            pred,
            off: ts[i].off,
        },
        j,
    ))
}

fn table_constraint(ts: &[Tok], d: Dialect, t: &mut Table) -> R<()> {
    let mut i = 0;
    let mut name = None;
    if word(ts.first(), "CONSTRAINT") {
        name = Some(
            ts.get(1)
                .and_then(name_of)
                .ok_or("CONSTRAINT without a name")?,
        );
        i = 2;
    }
    let off = ts[0].off;
    let w = ts.get(i).map(|t| t.up.clone()).unwrap_or_default();
    let end = match w.as_str() {
        "PRIMARY" | "UNIQUE" => {
            let primary = w == "PRIMARY";
            i += 1;
            if primary {
                if !word(ts.get(i), "KEY") {
                    return Err("PRIMARY without KEY".into());
                }
                i += 1;
            }
            let (nnd, j) = if primary {
                (false, i)
            } else {
                unique_nulls(ts, i, d)?
            };
            i = clustering(ts, j, d, &mut t.storage);
            let (cols, j) = col_list(ts, i, d == Dialect::Tsql)?;
            i = key_tail(ts, j, d, &mut t.storage)?;
            t.keys.push(Key {
                primary,
                name,
                cols,
                nulls_not_distinct: nnd,
                off,
            });
            i
        }
        "FOREIGN" => {
            if !word(ts.get(i + 1), "KEY") {
                return Err("FOREIGN without KEY".into());
            }
            let (cols, j) = col_list(ts, i + 2, false)?;
            if !word(ts.get(j), "REFERENCES") {
                return Err("FOREIGN KEY without REFERENCES".into());
            }
            let (fk, j) = references(ts, j, d, name, cols, off)?;
            t.fks.push(fk);
            j
        }
        "CHECK" => {
            let (c, j) = check(ts, i, d, name)?;
            t.checks.push(c);
            j
        }
        _ => return Err(format!("the table item `{}`", text(ts))),
    };
    if d == Dialect::Tsql && word(ts.get(end), "NOT") && word(ts.get(end + 1), "FOR") {
        return Err("NOT FOR REPLICATION".into());
    }
    if end != ts.len() {
        return Err(format!("`{}` in `{}`", text(&ts[end..]), text(ts)));
    }
    Ok(())
}

/// Where a column's default expression ends: at the next column clause
/// outside parentheses and CASE … END.
fn default_end(ts: &[Tok], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < ts.len() {
        let t = &ts[i];
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            K::Word if t.up == "CASE" => depth += 1,
            K::Word if t.up == "END" => depth = depth.saturating_sub(1),
            K::Word if depth == 0 && i > start => {
                let clause = matches!(
                    t.up.as_str(),
                    "NULL"
                        | "CONSTRAINT"
                        | "PRIMARY"
                        | "UNIQUE"
                        | "CHECK"
                        | "REFERENCES"
                        | "COLLATE"
                        | "GENERATED"
                        | "IDENTITY"
                        | "ROWGUIDCOL"
                        | "SPARSE"
                        | "FILESTREAM"
                        | "MASKED"
                        | "DEFAULT"
                        | "FOREIGN"
                        | "DEFERRABLE"
                        | "INITIALLY"
                ) || (t.up == "NOT" && word(ts.get(i + 1), "NULL"));
                if clause {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    ts.len()
}

fn column(ts: &[Tok], d: Dialect, t: &mut Table) -> R<()> {
    let name = name_of(&ts[0]).ok_or_else(|| format!("the column item `{}`", text(ts)))?;
    let what = |s: &str| format!("column {}: {s}", name.text);
    if word(ts.get(1), "AS") {
        return Err(what("a computed column"));
    }
    let mut p = ast::Parser::new(&ts[1..], d);
    let ty = p.ty().map_err(|e| what(&e))?;
    let mut i = 1 + p.pos();
    let mut ty = ty;
    // PostgreSQL `timestamp(3) with time zone`: the zone comes after the precision.
    if d == Dialect::Postgres
        && matches!(ty.name.as_str(), "TIMESTAMP" | "TIME")
        && (word(ts.get(i), "WITH") || word(ts.get(i), "WITHOUT"))
        && word(ts.get(i + 1), "TIME")
        && word(ts.get(i + 2), "ZONE")
    {
        ty.name = format!("{} {} TIME ZONE", ty.name, ts[i].up);
        i += 3;
    }
    let ty_text = text(&ts[1..i]);
    let parsed = match d {
        Dialect::Tsql => tsql_type(&ty),
        Dialect::Postgres => pg_type(&ty),
        Dialect::Db2i => Err("Db2 for i column types are not compared".into()),
    }
    .map_err(|e| what(&e))?;
    let mut c = Column {
        name: name.clone(),
        ty: parsed,
        ty_text,
        null: None,
        identity: None,
        default: None,
        off: ts[0].off,
    };
    let mut cname: Option<Name> = None;
    while i < ts.len() {
        let w = ts[i].up.as_str();
        if ts[i].kind != K::Word {
            return Err(what(&format!("`{}`", text(&ts[i..]))));
        }
        let set_null = |c: &mut Column, v: bool| {
            if c.null.is_some_and(|x| x != v) {
                return Err(what("both NULL and NOT NULL"));
            }
            c.null = Some(v);
            Ok(())
        };
        match w {
            "NULL" => {
                set_null(&mut c, true)?;
                i += 1;
            }
            "NOT" if word(ts.get(i + 1), "NULL") => {
                set_null(&mut c, false)?;
                i += 2;
            }
            "CONSTRAINT" => {
                cname = Some(
                    ts.get(i + 1)
                        .and_then(name_of)
                        .ok_or(what("CONSTRAINT without a name"))?,
                );
                i += 2;
                continue;
            }
            "DEFAULT" => {
                if c.default.is_some() {
                    return Err(what("two defaults"));
                }
                let end = default_end(ts, i + 1);
                if end == i + 1 {
                    return Err(what("DEFAULT without a value"));
                }
                c.default = Some(ts[i + 1..end].to_vec());
                i = end;
            }
            "IDENTITY" if d == Dialect::Tsql => {
                let (start, step, j) = if ts.get(i + 1).is_some_and(|t| t.kind == K::LParen) {
                    let end = close(ts, i + 1)?;
                    let (s, k) = number(ts, i + 2)?;
                    if !ts.get(k).is_some_and(|t| t.kind == K::Comma) {
                        return Err(what("IDENTITY needs both seed and increment"));
                    }
                    let (st, k) = number(ts, k + 1)?;
                    if k != end {
                        return Err(what("IDENTITY arguments"));
                    }
                    (s, st, end + 1)
                } else {
                    (1, 1, i + 1)
                };
                if word(ts.get(j), "NOT") && word(ts.get(j + 1), "FOR") {
                    return Err(what("IDENTITY NOT FOR REPLICATION"));
                }
                c.identity = Some(Identity {
                    start,
                    step,
                    always: true,
                });
                i = j;
            }
            "GENERATED" if d == Dialect::Postgres => {
                let (always, j) = if word(ts.get(i + 1), "ALWAYS") {
                    (true, i + 2)
                } else if word(ts.get(i + 1), "BY") && word(ts.get(i + 2), "DEFAULT") {
                    (false, i + 3)
                } else {
                    return Err(what("GENERATED"));
                };
                if !(word(ts.get(j), "AS") && word(ts.get(j + 1), "IDENTITY")) {
                    return Err(what("a generated column"));
                }
                let mut k = j + 2;
                let (mut start, mut step) = (None, 1i128);
                if ts.get(k).is_some_and(|t| t.kind == K::LParen) {
                    let end = close(ts, k)?;
                    let mut m = k + 1;
                    while m < end {
                        if word(ts.get(m), "START") {
                            m += usize::from(word(ts.get(m + 1), "WITH")) + 1;
                            let (v, n) = number(ts, m)?;
                            start = Some(v);
                            m = n;
                        } else if word(ts.get(m), "INCREMENT") {
                            m += usize::from(word(ts.get(m + 1), "BY")) + 1;
                            let (v, n) = number(ts, m)?;
                            step = v;
                            m = n;
                        } else {
                            return Err(what(&format!(
                                "the identity option `{}`",
                                text(&ts[m..end])
                            )));
                        }
                    }
                    k = end + 1;
                }
                // Without START WITH an ascending sequence starts at 1, a
                // descending one at -1 (its MINVALUE / MAXVALUE defaults).
                let start = start.unwrap_or(if step > 0 { 1 } else { -1 });
                c.identity = Some(Identity {
                    start,
                    step,
                    always,
                });
                i = k;
            }
            "PRIMARY" | "UNIQUE" => {
                let primary = w == "PRIMARY";
                let mut j = i + 1;
                if primary {
                    if !word(ts.get(j), "KEY") {
                        return Err(what("PRIMARY without KEY"));
                    }
                    j += 1;
                }
                let (nnd, k) = if primary {
                    (false, j)
                } else {
                    unique_nulls(ts, j, d)?
                };
                let k = clustering(ts, k, d, &mut t.storage);
                let k = key_tail(ts, k, d, &mut t.storage)?;
                t.keys.push(Key {
                    primary,
                    name: cname.take(),
                    cols: vec![name.clone()],
                    nulls_not_distinct: nnd,
                    off: ts[i].off,
                });
                i = k;
            }
            "CHECK" => {
                let (ck, j) = check(ts, i, d, cname.take())?;
                t.checks.push(ck);
                i = j;
            }
            "REFERENCES" | "FOREIGN" => {
                let j = if w == "FOREIGN" {
                    if !(word(ts.get(i + 1), "KEY") && word(ts.get(i + 2), "REFERENCES")) {
                        return Err(what("FOREIGN KEY"));
                    }
                    i + 2
                } else {
                    i
                };
                let (fk, k) = references(ts, j, d, cname.take(), vec![name.clone()], ts[i].off)?;
                if d == Dialect::Tsql && word(ts.get(k), "NOT") && word(ts.get(k + 1), "FOR") {
                    return Err(what("NOT FOR REPLICATION"));
                }
                t.fks.push(fk);
                i = k;
            }
            "ROWGUIDCOL" if d == Dialect::Tsql => {
                // Only names the column `$ROWGUID` stands for.
                t.storage.push(format!("ROWGUIDCOL on {}", name.text));
                i += 1;
            }
            "COLLATE" => return Err(what("a column collation")),
            _ => return Err(what(&format!("`{}`", text(&ts[i..])))),
        }
        if cname.is_some() && !matches!(w, "CONSTRAINT") {
            // A name before DEFAULT names the default (T-SQL), which no
            // statement compared here uses; before NULL / NOT NULL it names
            // nothing that can be dropped on its own.
            cname = None;
        }
    }
    if cname.is_some() {
        return Err(what("CONSTRAINT with nothing after it"));
    }
    t.columns.push(c);
    Ok(())
}

fn arg(ty: &ast::Type, i: usize) -> R<Option<u32>> {
    match ty.args.get(i) {
        None => Ok(None),
        Some(a) => a
            .parse::<u32>()
            .map(Some)
            .map_err(|_| format!("the type argument `{a}`")),
    }
}

fn max_arg(ty: &ast::Type) -> bool {
    ty.args.first().is_some_and(|a| a == "MAX")
}

pub(crate) fn tsql_type(ty: &ast::Type) -> R<Ty> {
    let n = ty.args.len();
    let len = |default: u32| -> R<Option<u32>> {
        if max_arg(ty) {
            Ok(None)
        } else {
            Ok(Some(arg(ty, 0)?.unwrap_or(default)))
        }
    };
    let none = |t: Ty| {
        if n == 0 {
            Ok(t)
        } else {
            Err(format!("{} takes no arguments", ty.name))
        }
    };
    match ty.name.as_str() {
        "BIGINT" => none(Ty::Int(64)),
        "INT" | "INTEGER" => none(Ty::Int(32)),
        "SMALLINT" => none(Ty::Int(16)),
        "TINYINT" => none(Ty::TinyInt),
        "BIT" => none(Ty::Bit),
        "DECIMAL" | "NUMERIC" | "DEC" => {
            let p = arg(ty, 0)?.unwrap_or(18);
            let s = arg(ty, 1)?.unwrap_or(0);
            Ok(Ty::Dec(Some((p, s))))
        }
        "MONEY" => none(Ty::Money),
        "SMALLMONEY" => none(Ty::SmallMoney),
        // float(1..24) is real, float(25..53) and float are double precision.
        "FLOAT" => match arg(ty, 0)? {
            None => Ok(Ty::Float(true)),
            Some(1..=24) => Ok(Ty::Float(false)),
            Some(25..=53) => Ok(Ty::Float(true)),
            Some(x) => Err(format!("float({x})")),
        },
        "REAL" => none(Ty::Float(false)),
        "DOUBLE PRECISION" => none(Ty::Float(true)),
        "CHAR" | "CHARACTER" => Ok(Ty::Str {
            fixed: true,
            national: false,
            len: Some(arg(ty, 0)?.unwrap_or(1)),
        }),
        "VARCHAR" | "CHAR VARYING" | "CHARACTER VARYING" => Ok(Ty::Str {
            fixed: false,
            national: false,
            len: len(1)?,
        }),
        "NCHAR" => Ok(Ty::Str {
            fixed: true,
            national: true,
            len: Some(arg(ty, 0)?.unwrap_or(1)),
        }),
        "NVARCHAR" => Ok(Ty::Str {
            fixed: false,
            national: true,
            len: len(1)?,
        }),
        "DATE" => none(Ty::Date),
        "TIME" => Ok(Ty::Time(arg(ty, 0)?.unwrap_or(7))),
        "DATETIME" => none(Ty::DateTime),
        "SMALLDATETIME" => none(Ty::SmallDateTime),
        "DATETIME2" => Ok(Ty::DateTime2(arg(ty, 0)?.unwrap_or(7))),
        "DATETIMEOFFSET" => Ok(Ty::DateTimeOffset(arg(ty, 0)?.unwrap_or(7))),
        "UNIQUEIDENTIFIER" => none(Ty::Uuid),
        "BINARY" => Ok(Ty::Binary {
            fixed: true,
            len: Some(arg(ty, 0)?.unwrap_or(1)),
        }),
        "VARBINARY" => Ok(Ty::Binary {
            fixed: false,
            len: len(1)?,
        }),
        other => Err(format!("the type {other} is not compared")),
    }
}

pub(crate) fn pg_type(ty: &ast::Type) -> R<Ty> {
    let n = ty.args.len();
    let none = |t: Ty| {
        if n == 0 {
            Ok(t)
        } else {
            Err(format!("{} takes no arguments", ty.name))
        }
    };
    match ty.name.as_str() {
        "SMALLINT" | "INT2" => none(Ty::Int(16)),
        "INTEGER" | "INT" | "INT4" => none(Ty::Int(32)),
        "BIGINT" | "INT8" => none(Ty::Int(64)),
        "BOOLEAN" | "BOOL" => none(Ty::Bool),
        "NUMERIC" | "DECIMAL" => match (arg(ty, 0)?, arg(ty, 1)?) {
            (None, _) => Ok(Ty::Dec(None)),
            (Some(p), s) => Ok(Ty::Dec(Some((p, s.unwrap_or(0))))),
        },
        "REAL" | "FLOAT4" => none(Ty::Float(false)),
        "DOUBLE PRECISION" | "FLOAT8" => none(Ty::Float(true)),
        "FLOAT" => match arg(ty, 0)? {
            None => Ok(Ty::Float(true)),
            Some(1..=24) => Ok(Ty::Float(false)),
            Some(25..=53) => Ok(Ty::Float(true)),
            Some(x) => Err(format!("float({x})")),
        },
        "CHAR" | "CHARACTER" => Ok(Ty::Str {
            fixed: true,
            national: false,
            len: Some(arg(ty, 0)?.unwrap_or(1)),
        }),
        "VARCHAR" | "CHARACTER VARYING" | "CHAR VARYING" => Ok(Ty::Str {
            fixed: false,
            national: false,
            len: arg(ty, 0)?,
        }),
        "TEXT" => none(Ty::Str {
            fixed: false,
            national: false,
            len: None,
        }),
        "DATE" => none(Ty::Date),
        "TIME" | "TIME WITHOUT TIME ZONE" => Ok(Ty::Time(arg(ty, 0)?.unwrap_or(6))),
        "TIMESTAMP" | "TIMESTAMP WITHOUT TIME ZONE" => Ok(Ty::Timestamp(arg(ty, 0)?.unwrap_or(6))),
        "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => Ok(Ty::TimestampTz(arg(ty, 0)?.unwrap_or(6))),
        "UUID" => none(Ty::Uuid),
        "BYTEA" => none(Ty::Binary {
            fixed: false,
            len: None,
        }),
        other => Err(format!("the type {other} is not compared")),
    }
}

/// How two column types compare: what the comparison relies on, and a
/// difference or a reason it is not decided. Witnesses name the sides
/// `{T}` (T-SQL) and `{P}` (PostgreSQL).
#[derive(Debug, Default)]
pub struct TyCmp {
    pub asms: Vec<(&'static str, &'static str)>,
    /// PostgreSQL stores values T-SQL rejects (`ASM-COLUMN-RANGE`): what.
    pub wider: Option<String>,
    pub diff: Option<String>,
}

pub mod asm {
    pub const COLUMN_RANGE: (&str, &str) = (
        "ASM-COLUMN-RANGE",
        "Stored values stay within what both sides' column types accept: a converted column that accepts more (larger numbers, longer strings, wider date ranges, NaN and infinity, 24:00:00) lets a write succeed that the original rejects",
    );
    pub const TSQL_CODE_PAGE: (&str, &str) = (
        "ASM-TSQL-CODE-PAGE",
        "T-SQL char and varchar values hold only characters of the column collation's code page (SQL Server stores any other character as '?'), and that code page is single-byte, so a length counts characters on both sides (with a double-byte code page such as 932 or 936, or a UTF-8 collation, T-SQL counts bytes and PostgreSQL accepts more)",
    );
    pub const VALUE_SIZE: (&str, &str) = (
        "ASM-VALUE-SIZE-1GB",
        "No single value is larger than 1 GB, PostgreSQL's limit for one field (T-SQL max types hold up to 2 GB)",
    );
    pub const BIT_BOOLEAN: (&str, &str) = (
        "ASM-BIT-BOOLEAN-COLUMN",
        "The T-SQL bit column's 0 and 1 are the PostgreSQL boolean column's false and true; every statement that uses the column is compared on its own",
    );
    pub const MONEY_ROUNDING: (&str, &str) = (
        "ASM-TSQL-MONEY-ROUNDING",
        "T-SQL rounds a value with more than four decimals to money the way PostgreSQL rounds it to numeric(…, 4): half away from zero",
    );
    pub const IDENTITY: (&str, &str) = (
        "ASM-IDENTITY-VALUES",
        "Identity columns hand out the same values: nobody relies on the gaps rolled-back inserts or restarts leave (SQL Server caches identity values, PostgreSQL sequences do not), and explicit values go in only with IDENTITY_INSERT on one side and OVERRIDING SYSTEM VALUE on the other",
    );
    pub const ANSI_NULL_DEFAULT: (&str, &str) = (
        "ASM-TSQL-ANSI-NULL-DEFAULT",
        "T-SQL columns declared without NULL or NOT NULL are nullable: ANSI_NULL_DFLT_ON is in effect when the table is created, as it is for ODBC, OLE DB and .NET connections and SSDT deployments",
    );
}

/// T-SQL type `t` against PostgreSQL type `p`.
pub fn types(t: &Ty, p: &Ty) -> TyCmp {
    let mut c = TyCmp::default();
    let diff = |c: &mut TyCmp, s: String| c.diff = Some(s);
    match (t, p) {
        (Ty::Int(a), Ty::Int(b)) if a == b => {}
        (Ty::Int(a), Ty::Int(b)) if b > a => {
            c.wider = Some(format!("{b}-bit integers beyond the {a}-bit range"))
        }
        (Ty::Int(_), Ty::Int(b)) => diff(
            &mut c,
            format!("{{T}} stores {} in the column, {{P}} rejects it ({b}-bit integer)", 1i128 << (*b - 1)),
        ),
        (Ty::TinyInt, Ty::Int(_)) => {
            c.wider = Some("negative numbers and numbers above 255 (tinyint is 0 to 255)".into())
        }
        (Ty::Bit, Ty::Bool) => c.asms.push(asm::BIT_BOOLEAN),
        (Ty::Dec(Some((p1, s1))), Ty::Dec(Some((p2, s2)))) => {
            if s1 != s2 {
                diff(
                    &mut c,
                    format!("{{T}} rounds 0.{} to {s1} decimal places, {{P}} to {s2}", "1".repeat(*s1.max(s2) as usize + 1)),
                );
            } else if p2 < p1 {
                diff(
                    &mut c,
                    format!("{{T}} stores a value with {} digits before the decimal point, {{P}} rejects it (numeric({p2}, {s2}))", p1 - s1),
                );
            } else if p2 > p1 {
                c.wider = Some(format!("numbers with more than {} digits before the decimal point", p1 - s1));
            }
        }
        (Ty::Dec(Some((_, s))), Ty::Dec(None)) => diff(
            &mut c,
            format!("{{P}} keeps every decimal of 0.{}, {{T}} rounds it to {s} decimal places", "1".repeat(*s as usize + 1)),
        ),
        (Ty::Money | Ty::SmallMoney, Ty::Dec(Some((p2, 4)))) => {
            let need = if *t == Ty::Money { 19 } else { 10 };
            if *p2 < need {
                diff(
                    &mut c,
                    format!("{{T}} stores money values with {} digits before the decimal point, {{P}} rejects them (numeric({p2}, 4))", need - 4),
                );
            } else {
                c.asms.push(asm::MONEY_ROUNDING);
                c.wider = Some("amounts beyond the money range".into());
            }
        }
        (Ty::Float(a), Ty::Float(b)) if a == b => {
            c.wider = Some("NaN and infinity".into());
        }
        (Ty::Float(a), Ty::Float(_)) => diff(
            &mut c,
            format!(
                "0.1 is stored as a {}-bit binary fraction by {{T}} and a {}-bit one by {{P}}, so the stored values differ",
                if *a { 53 } else { 24 },
                if *a { 24 } else { 53 }
            ),
        ),
        (
            Ty::Str {
                fixed: f1,
                national,
                len: l1,
            },
            Ty::Str {
                fixed: f2,
                len: l2,
                ..
            },
        ) => {
            if f1 != f2 {
                let (pad, var) = if *f1 { ("{T}", "{P}") } else { ("{P}", "{T}") };
                diff(
                    &mut c,
                    format!("{pad} pads 'a' with blanks to the column's length, {var} stores 'a' as it is"),
                );
                return c;
            }
            if !national {
                c.asms.push(asm::TSQL_CODE_PAGE);
            }
            match (l1, l2) {
                (None, None) => c.asms.push(asm::VALUE_SIZE),
                (None, Some(m)) => diff(
                    &mut c,
                    format!("{{T}} stores a string of {} characters in the column, {{P}} rejects it (length {m})", u64::from(*m) + 1),
                ),
                (Some(n), None) => {
                    c.wider = Some(format!("strings longer than {n} characters"));
                }
                (Some(n), Some(m)) if m < n => diff(
                    &mut c,
                    format!("{{T}} stores a string of {n} characters in the column, {{P}} rejects it (length {m})"),
                ),
                (Some(n), Some(m)) if m > n => {
                    c.wider = Some(format!("strings longer than {n} characters"));
                }
                (Some(n), Some(_)) if *national => {
                    // nchar / nvarchar count UTF-16 code units: a character
                    // outside the Basic Multilingual Plane counts twice.
                    c.wider = Some(format!(
                        "strings of {n} characters some of which lie outside the Basic Multilingual Plane (an emoji counts twice in nvarchar)"
                    ));
                }
                _ => {}
            }
        }
        (Ty::Date, Ty::Date) => c.wider = Some("dates before 0001-01-01 or after 9999-12-31, and infinity".into()),
        (Ty::Time(a), Ty::Time(b)) if a == b => c.wider = Some("24:00:00".into()),
        (Ty::DateTime2(a), Ty::Timestamp(b)) if a == b => {
            c.wider = Some("timestamps before year 1 or after year 9999, and infinity".into())
        }
        (Ty::Time(a), Ty::Time(b)) | (Ty::DateTime2(a), Ty::Timestamp(b)) => diff(
            &mut c,
            format!(
                "{{T}} keeps {a} fraction digits of the seconds and {{P}} {b}, so 00:00:00.{} is stored differently",
                "1".repeat(*a.max(b) as usize)
            ),
        ),
        (Ty::DateTime, Ty::Timestamp(_)) => diff(
            &mut c,
            "{T} datetime rounds to 1/300 of a second, so 00:00:00.001 is stored as 00:00:00.000; {P} stores 00:00:00.001".into(),
        ),
        (Ty::SmallDateTime, Ty::Timestamp(_)) => diff(
            &mut c,
            "{T} smalldatetime rounds to the minute, so 10:00:29 is stored as 10:00:00; {P} stores 10:00:29".into(),
        ),
        (Ty::DateTimeOffset(_), Ty::TimestampTz(_)) => diff(
            &mut c,
            "{T} keeps the offset of 2024-01-01 10:00 +02:00, {P} stores the instant only and reads it back in the session's time zone".into(),
        ),
        (Ty::Uuid, Ty::Uuid) => {}
        (Ty::Binary { fixed: true, .. }, Ty::Binary { fixed: false, .. }) => diff(
            &mut c,
            "{T} pads 0x01 with zero bytes to the column's length, {P} stores 0x01".into(),
        ),
        (Ty::Binary { fixed: false, len: l1 }, Ty::Binary { fixed: false, len: None }) => match l1 {
            None => c.asms.push(asm::VALUE_SIZE),
            Some(n) => c.wider = Some(format!("byte strings longer than {n} bytes")),
        },
        _ => diff(&mut c, "the column types store different kinds of values".into()),
    }
    c
}

/// What comparing two tables found. Byte offsets are (side A, side B).
#[derive(Debug)]
pub enum Event {
    Asm((&'static str, &'static str)),
    /// A difference with a concrete case.
    Diff {
        rule: &'static str,
        witness: String,
        at: (usize, usize),
    },
    /// Not decided, and why.
    Unknown(String),
    /// Something passed over or relied on, for the normalizations list.
    Note(String),
    /// Two defaults to compare as `SELECT a` against `SELECT b`.
    Default {
        col: String,
        a: Vec<Tok>,
        b: Vec<Tok>,
        at: (usize, usize),
    },
    /// Two check predicates: rows rejected are those `NOT (pred)` selects.
    Check {
        what: String,
        a: Vec<Tok>,
        b: Vec<Tok>,
        at: (usize, usize),
    },
}

/// The name a part resolves to, up to letter case: T-SQL names ignore case
/// (case-insensitive catalog collation, `ASM-IDENT-FOLD`), PostgreSQL folds
/// unquoted names to lower case. A quoted PostgreSQL name keeps its case in
/// PostgreSQL; it folds here only to meet the T-SQL name, and
/// `Table::case_check` (`ddl::case_check` for indexes) first makes sure that
/// no other name the statement or the catalog holds differs from it only in
/// letter case.
pub(super) fn key(n: &Name, _d: Dialect) -> R<String> {
    Ok(n.text.to_lowercase())
}

/// The name as PostgreSQL stores it: a quoted name as written, others in
/// lower case.
pub(super) fn stored(n: &Name) -> String {
    if n.quoted {
        n.text.clone()
    } else {
        n.text.to_lowercase()
    }
}

fn keys(ns: &[Name], d: Dialect) -> R<Vec<String>> {
    ns.iter().map(|n| key(n, d)).collect()
}

/// Schema (None for the default one) and table name.
fn table_key(parts: &[Name], d: Dialect) -> R<(Option<String>, String)> {
    let ks = keys(parts, d)?;
    let default = match d {
        Dialect::Tsql => "dbo",
        Dialect::Postgres => "public",
        Dialect::Db2i => return Err("Db2 for i tables are not compared".into()),
    };
    // The default schema by its stored spelling: PostgreSQL's "Public" is
    // another schema.
    let is_default = |n: &Name| match d {
        Dialect::Postgres => stored(n) == default,
        _ => n.text.eq_ignore_ascii_case(default),
    };
    match (ks.as_slice(), parts) {
        ([t], _) => Ok((None, t.clone())),
        ([_, t], [s, _]) if is_default(s) => Ok((None, t.clone())),
        ([s, t], _) => Ok((Some(s.clone()), t.clone())),
        _ => Err("a name with more than two parts".into()),
    }
}

impl Table {
    /// PostgreSQL: whether this table's names may be compared up to letter
    /// case (`key`), given `schema`, the catalog with this table added (see
    /// `Schema::case_check`). Names of other objects (referenced tables and
    /// their columns, sequences) must be catalog names when quoting keeps
    /// their capitals.
    pub fn case_check(&self, schema: &super::schema::Schema) -> R<()> {
        if self.dialect != Dialect::Postgres {
            return Ok(());
        }
        let own = self
            .name
            .iter()
            .chain(self.columns.iter().map(|c| &c.name))
            .chain(self.keys.iter().flat_map(|k| k.name.iter().chain(&k.cols)))
            .chain(self.fks.iter().flat_map(|f| f.name.iter().chain(&f.cols)))
            .chain(self.checks.iter().filter_map(|c| c.name.as_ref()));
        for n in own {
            schema.case_check(&stored(n), false)?;
        }
        let seqs: Vec<Vec<Name>> = self
            .columns
            .iter()
            .filter_map(|c| sequence(c.default.as_ref()?, self.dialect))
            .collect();
        let other = self
            .fks
            .iter()
            .flat_map(|f| f.table.iter().chain(f.ref_cols.iter().flatten()))
            .chain(seqs.iter().flatten());
        for n in other {
            schema.case_check(&stored(n), true)?;
        }
        Ok(())
    }
}

/// A column is nullable when it says NULL, or says nothing and is neither
/// in the primary key nor an identity column.
pub(super) fn nullable(t: &Table, c: &Column) -> R<bool> {
    let k = key(&c.name, t.dialect)?;
    let in_pk = t.keys.iter().filter(|x| x.primary).any(|x| {
        x.cols
            .iter()
            .any(|n| key(n, t.dialect).is_ok_and(|n| n == k))
    });
    Ok(c.null.unwrap_or(!(in_pk || c.identity.is_some())))
}

pub(super) fn column_by_key<'t>(t: &'t Table, k: &str) -> Option<&'t Column> {
    t.columns
        .iter()
        .find(|c| key(&c.name, t.dialect).is_ok_and(|n| n == k))
}

/// The value a T-SQL bit default or a PostgreSQL boolean default stands for.
fn bool_literal(ts: &[Tok]) -> Option<bool> {
    let mut ts = ts;
    while ts.len() >= 2
        && ts[0].kind == K::LParen
        && ts[ts.len() - 1].kind == K::RParen
        && close(ts, 0).ok() == Some(ts.len() - 1)
    {
        ts = &ts[1..ts.len() - 1];
    }
    match ts {
        [t] if t.kind == K::Num && t.text == "0" => Some(false),
        [t] if t.kind == K::Num && t.text == "1" => Some(true),
        [t] if t.kind == K::Word && t.up == "FALSE" => Some(false),
        [t] if t.kind == K::Word && t.up == "TRUE" => Some(true),
        _ => None,
    }
}

/// Whether a default already has its column's type, so storing it converts
/// nothing. The engine compares a default as `SELECT default`, but the column
/// stores it converted to the column's type, and the dialects convert
/// differently (`int DEFAULT 1.5`: T-SQL truncates to 1, PostgreSQL rounds to
/// 2). Only NULL, literals the type holds exactly, and argument-free calls on
/// date, time and uuid columns pass; anything else is not compared.
fn default_fits(ts: &[Tok], ty: &Ty) -> R<()> {
    let mut ts = ts;
    while ts.len() >= 2
        && ts[0].kind == K::LParen
        && ts[ts.len() - 1].kind == K::RParen
        && close(ts, 0).ok() == Some(ts.len() - 1)
    {
        ts = &ts[1..ts.len() - 1];
    }
    let not = |why: &str| Err(format!("{} for a {ty:?} column {why}", text(ts)));
    let (neg, num) = match ts {
        [t] if t.kind == K::Word && t.up == "NULL" => return Ok(()),
        [m, t] if m.kind == K::Op && (m.text == "-" || m.text == "+") && t.kind == K::Num => {
            (m.text == "-", Some(t))
        }
        [t] if t.kind == K::Num => (false, Some(t)),
        _ => (false, None),
    };
    if let Some(n) = num {
        // Digits before and after the point; an exponent makes T-SQL a float.
        let s = n.text.as_str();
        if !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') || s.matches('.').count() > 1 {
            return not("(not a plain decimal number)");
        }
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        let fits_dec = |p: u32, sc: u32| int.len() as u32 <= p - sc && frac.len() as u32 <= sc;
        let fits_int = |lo: i128, hi: i128| {
            let v = match int {
                "" => Some(0),
                _ if int.len() <= 38 => int.parse::<i128>().ok(),
                _ => None,
            };
            frac.is_empty()
                && v.is_some_and(|v| {
                    let v = if neg { -v } else { v };
                    lo <= v && v <= hi
                })
        };
        let ok = match ty {
            Ty::Int(bits) => {
                let hi = (1i128 << (bits - 1)) - 1;
                fits_int(-hi - 1, hi)
            }
            Ty::TinyInt => fits_int(0, 255),
            Ty::Dec(Some((p, sc))) => fits_dec(*p, *sc),
            Ty::Money => fits_dec(15 + 4, 4),
            Ty::SmallMoney => fits_dec(6 + 4, 4),
            _ => false,
        };
        return if ok {
            Ok(())
        } else {
            not("(converted on storing)")
        };
    }
    match ts {
        [t] if t.kind == K::Str => {
            let Ty::Str { len, .. } = ty else {
                return not("(a string read as another type)");
            };
            let v = ast::string_value(&t.text)?;
            if !v.is_ascii() {
                return not("(characters outside ASCII)");
            }
            if len.is_some_and(|l| v.chars().count() as u32 > l) {
                return not("(longer than the column)");
            }
            Ok(())
        }
        // A call with no arguments: GETDATE(), SYSDATETIME(), NEWID(), now().
        [f, o, c] if f.kind == K::Word && o.kind == K::LParen && c.kind == K::RParen => match ty {
            Ty::Date
            | Ty::Time(_)
            | Ty::DateTime
            | Ty::SmallDateTime
            | Ty::DateTime2(_)
            | Ty::Timestamp(_)
            | Ty::TimestampTz(_)
            | Ty::DateTimeOffset(_)
            | Ty::Uuid => Ok(()),
            _ => not("(a call's result converted on storing)"),
        },
        // PostgreSQL niladic forms: CURRENT_TIMESTAMP, LOCALTIMESTAMP, ...
        [f] if f.kind == K::Word
            && matches!(
                f.up.as_str(),
                "CURRENT_TIMESTAMP"
                    | "CURRENT_DATE"
                    | "CURRENT_TIME"
                    | "LOCALTIMESTAMP"
                    | "LOCALTIME"
            )
            && matches!(
                ty,
                Ty::Date
                    | Ty::Time(_)
                    | Ty::DateTime
                    | Ty::SmallDateTime
                    | Ty::DateTime2(_)
                    | Ty::Timestamp(_)
                    | Ty::TimestampTz(_)
                    | Ty::DateTimeOffset(_)
            ) =>
        {
            Ok(())
        }
        _ => not("(an expression whose type the table proof does not check)"),
    }
}

/// The sequence a default takes its next value from: T-SQL `NEXT VALUE
/// FOR s`, PostgreSQL `nextval('s')` (the text read as a name: unquoted
/// parts fold, `"…"` parts keep their case).
fn sequence(ts: &[Tok], d: Dialect) -> Option<Vec<Name>> {
    let mut ts = ts;
    while ts.len() >= 2
        && ts[0].kind == K::LParen
        && ts[ts.len() - 1].kind == K::RParen
        && close(ts, 0).ok() == Some(ts.len() - 1)
    {
        ts = &ts[1..ts.len() - 1];
    }
    match d {
        Dialect::Tsql => {
            if !(word(ts.first(), "NEXT") && word(ts.get(1), "VALUE") && word(ts.get(2), "FOR")) {
                return None;
            }
            let (n, end) = dotted(ts, 3).ok()?;
            (end == ts.len()).then_some(n)
        }
        Dialect::Postgres => {
            let lit = match ts {
                [f, o, s, c]
                    if word(Some(f), "NEXTVAL") && o.kind == K::LParen && c.kind == K::RParen =>
                {
                    s
                }
                [f, o, s, cast, ty, c]
                    if word(Some(f), "NEXTVAL")
                        && o.kind == K::LParen
                        && cast.kind == K::Op
                        && cast.text == "::"
                        && word(Some(ty), "REGCLASS")
                        && c.kind == K::RParen =>
                {
                    s
                }
                _ => return None,
            };
            if lit.kind != K::Str
                || !lit.text.starts_with('\'')
                || !lit.text.ends_with('\'')
                || lit.text.len() < 2
            {
                return None;
            }
            let body = lit.text[1..lit.text.len() - 1].replace("''", "'");
            let mut parts = Vec::new();
            let mut rest = body.as_str();
            loop {
                let (part, next) = if let Some(r) = rest.strip_prefix('"') {
                    let end = r.find('"')?;
                    (
                        Name {
                            text: r[..end].to_string(),
                            quoted: true,
                        },
                        &r[end + 1..],
                    )
                } else {
                    let end = rest.find('.').unwrap_or(rest.len());
                    let t = &rest[..end];
                    if t.is_empty() || !t.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return None;
                    }
                    (
                        Name {
                            text: t.to_string(),
                            quoted: false,
                        },
                        &rest[end..],
                    )
                };
                parts.push(part);
                match next.strip_prefix('.') {
                    Some(r) => rest = r,
                    None if next.is_empty() => return Some(parts),
                    None => return None,
                }
            }
        }
        Dialect::Db2i => None,
    }
}

/// Constraint names: a name on one side must be the name on the other.
/// Unnamed constraints get names neither side's code can know.
fn names_match(a: &Option<Name>, b: &Option<Name>, da: Dialect, db: Dialect) -> R<bool> {
    Ok(match (a, b) {
        (Some(x), Some(y)) => key(x, da)? == key(y, db)?,
        (None, None) => true,
        _ => false,
    })
}

/// Compares table `x` (side A) with table `y` (side B), one of them T-SQL
/// and the other PostgreSQL. Side names fill the witnesses.
pub fn compare(x: &Table, y: &Table, na: &str, nb: &str) -> Vec<Event> {
    let mut ev = Vec::new();
    if let Err(e) = compare_into(x, y, na, nb, &mut ev) {
        ev.push(Event::Unknown(e));
    }
    ev
}

fn compare_into(x: &Table, y: &Table, na: &str, nb: &str, ev: &mut Vec<Event>) -> R<()> {
    let (da, db) = (x.dialect, y.dialect);
    let tsql_a = match (da, db) {
        (Dialect::Tsql, Dialect::Postgres) => true,
        (Dialect::Postgres, Dialect::Tsql) => false,
        _ => {
            return Err(
                "only T-SQL and PostgreSQL tables are compared (Db2 for i column types have no verified facts yet)"
                    .into(),
            )
        }
    };
    let (tn, pn) = if tsql_a { (na, nb) } else { (nb, na) };
    let side = |s: String| s.replace("{T}", tn).replace("{P}", pn);
    // A's and B's versions of a pair (T-SQL, PostgreSQL), and back.
    let ab = |a: usize, b: usize| (a, b);
    let start = (
        x.name_toks.first().map_or(0, |t| t.off),
        y.name_toks.first().map_or(0, |t| t.off),
    );
    let diff = |rule: &'static str, witness: String, at: (usize, usize)| Event::Diff {
        rule,
        witness: side(witness),
        at,
    };

    let (ka, kb) = (table_key(&x.name, da)?, table_key(&y.name, db)?);
    ev.push(Event::Asm(super::canon::asm::IDENT_FOLD));
    if ka.0.is_none() && kb.0.is_none() {
        ev.push(Event::Asm(super::canon::asm::DEFAULT_SCHEMA));
    }
    if ka != kb {
        ev.push(diff(
            "equiv.table-name",
            format!(
                "{na} creates {} and {nb} creates {}, so statements naming one find the other missing",
                text(&x.name_toks),
                text(&y.name_toks)
            ),
            start,
        ));
        return Ok(());
    }
    for (t, n) in [(x, na), (y, nb)] {
        for s in &t.storage {
            ev.push(Event::Note(format!(
                "storage clause passed over on {n}: {s}"
            )));
        }
    }

    // Columns, in order: SELECT * and INSERT without a column list see them so.
    if x.columns.len() != y.columns.len() {
        ev.push(diff(
            "equiv.table-columns",
            format!(
                "{na} creates {} columns and {nb} {}, so SELECT * returns different rows",
                x.columns.len(),
                y.columns.len()
            ),
            start,
        ));
        return Ok(());
    }
    let mut wider = Vec::new();
    for (cx, cy) in x.columns.iter().zip(&y.columns) {
        let at = ab(cx.off, cy.off);
        let (kx, ky) = (key(&cx.name, da)?, key(&cy.name, db)?);
        if kx != ky {
            ev.push(diff(
                "equiv.table-columns",
                format!(
                    "the column in this position is {} on {na} and {} on {nb}",
                    cx.name.text, cy.name.text
                ),
                at,
            ));
            continue;
        }
        let (ct, cp) = if tsql_a { (cx, cy) } else { (cy, cx) };
        let (tt, tp) = if tsql_a { (x, y) } else { (y, x) };
        let tc = types(&ct.ty, &cp.ty);
        for a in tc.asms {
            ev.push(Event::Asm(a));
        }
        if let Some(d) = tc.diff {
            ev.push(diff(
                "equiv.column-type",
                format!(
                    "column {} is {} on {{T}} and {} on {{P}}: {d}",
                    cx.name.text, ct.ty_text, cp.ty_text
                ),
                at,
            ));
            continue;
        }
        if let Some(w) = tc.wider {
            wider.push(format!("{} ({w})", cx.name.text));
        }
        // NULL.
        if ct.null.is_none() && ct.identity.is_none() && nullable(tt, ct)? {
            ev.push(Event::Asm(asm::ANSI_NULL_DEFAULT));
        }
        let (nt, np) = (nullable(tt, ct)?, nullable(tp, cp)?);
        if nt != np {
            let (yes, no) = if nt { ("{T}", "{P}") } else { ("{P}", "{T}") };
            ev.push(diff(
                "equiv.column-null",
                format!(
                    "{yes} stores NULL in column {}, {no} rejects it",
                    cx.name.text
                ),
                at,
            ));
        }
        // Identity.
        match (&ct.identity, &cp.identity) {
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                let has = if ct.identity.is_some() { "{T}" } else { "{P}" };
                ev.push(diff(
                    "equiv.identity",
                    format!(
                        "{has} numbers column {} itself when an INSERT leaves it out, the other side does not",
                        cx.name.text
                    ),
                    at,
                ));
            }
            (Some(it), Some(ip)) => {
                let ints = |t: &Ty| matches!(t, Ty::Int(_) | Ty::TinyInt);
                if !ints(&ct.ty) || !ints(&cp.ty) {
                    return Err(format!(
                        "column {}: an identity of a non-integer type",
                        cx.name.text
                    ));
                }
                if it.start != ip.start || it.step != ip.step {
                    ev.push(diff(
                        "equiv.identity",
                        format!(
                            "the first rows get {}, {}, … in column {} on {{T}} and {}, {}, … on {{P}}",
                            it.start,
                            it.start + it.step,
                            cx.name.text,
                            ip.start,
                            ip.start + ip.step
                        ),
                        at,
                    ));
                } else {
                    ev.push(Event::Asm(asm::IDENTITY));
                    if !ip.always {
                        wider.push(format!(
                            "{} (an INSERT may give it a value: GENERATED BY DEFAULT)",
                            cx.name.text
                        ));
                    }
                }
            }
        }
        // Default.
        match (&cx.default, &cy.default) {
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                let has = if cx.default.is_some() { na } else { nb };
                ev.push(diff(
                    "equiv.column-default",
                    format!(
                        "an INSERT that leaves out column {} stores the default on {has} and NULL on the other side",
                        cx.name.text
                    ),
                    at,
                ));
            }
            (Some(a), Some(b)) => {
                if matches!((&ct.ty, &cp.ty), (Ty::Bit, Ty::Bool)) {
                    let (dt, dp) = if tsql_a { (a, b) } else { (b, a) };
                    match (bool_literal(dt), bool_literal(dp)) {
                        (Some(v), Some(w)) if v == w => {}
                        (Some(_), Some(_)) => ev.push(diff(
                            "equiv.column-default",
                            format!(
                                "an INSERT that leaves out column {} stores {} on {{T}} and {} on {{P}}",
                                cx.name.text,
                                text(dt),
                                text(dp)
                            ),
                            at,
                        )),
                        _ => ev.push(Event::Unknown(format!(
                            "the defaults of the bit column {} are not the constants 0 and 1",
                            cx.name.text
                        ))),
                    }
                } else if let (Some(sx), Some(sy)) = (sequence(a, da), sequence(b, db)) {
                    // Both take the next value of a sequence, one per row.
                    if table_key(&sx, da)? != table_key(&sy, db)? {
                        ev.push(diff(
                            "equiv.column-default",
                            format!(
                                "column {} takes its default from sequence {} on {na} and {} on {nb}",
                                cx.name.text,
                                text_name(&sx),
                                text_name(&sy)
                            ),
                            at,
                        ));
                    }
                } else if let Err(w) = default_fits(if tsql_a { a } else { b }, &ct.ty)
                    .and_then(|_| default_fits(if tsql_a { b } else { a }, &cp.ty))
                {
                    ev.push(Event::Unknown(format!(
                        "the default of column {} is converted to the column's type on storing, which the dialects do differently: {w}",
                        cx.name.text
                    )));
                } else {
                    ev.push(Event::Default {
                        col: cx.name.text.clone(),
                        a: a.clone(),
                        b: b.clone(),
                        at,
                    });
                }
            }
        }
    }
    if !wider.is_empty() {
        ev.push(Event::Asm(asm::COLUMN_RANGE));
        ev.push(Event::Note(side(format!(
            "columns where {{P}} accepts more than {{T}}: {}",
            wider.join("; ")
        ))));
    }

    // Keys: matched by kind and column set.
    let key_cols = |t: &Table, k: &Key| -> R<Vec<String>> {
        let mut v = keys(&k.cols, t.dialect)?;
        v.sort();
        Ok(v)
    };
    let mut used = vec![false; y.keys.len()];
    for kx in &x.keys {
        let cols = key_cols(x, kx)?;
        let found = y.keys.iter().enumerate().find(|(j, ky)| {
            !used[*j] && ky.primary == kx.primary && key_cols(y, ky).is_ok_and(|c| c == cols)
        });
        let what = if kx.primary {
            "primary key"
        } else {
            "unique constraint"
        };
        let Some((j, ky)) = found else {
            ev.push(diff(
                "equiv.table-key",
                format!(
                    "{na} rejects a second row with the same ({}) by its {what}, {nb} has no such {what} and accepts it",
                    cols.join(", ")
                ),
                ab(kx.off, start.1),
            ));
            continue;
        };
        used[j] = true;
        let at = ab(kx.off, ky.off);
        if !names_match(&kx.name, &ky.name, da, db)? {
            ev.push(diff(
                "equiv.constraint-name",
                format!(
                    "the {what} on ({}) is named {} on {na} and {} on {nb}, so a statement that names it (ALTER TABLE … DROP CONSTRAINT) finds it on one side only",
                    cols.join(", "),
                    kx.name.as_ref().map_or("by the system".into(), |n| n.text.clone()),
                    ky.name.as_ref().map_or("by the system".into(), |n| n.text.clone())
                ),
                at,
            ));
        }
        let (kt, kp, tt, tp) = if tsql_a {
            (kx, ky, x, y)
        } else {
            (ky, kx, y, x)
        };
        for c in &cols {
            let (Some(ct), Some(cp)) = (column_by_key(tt, c), column_by_key(tp, c)) else {
                return Err(format!(
                    "the {what} names column {c}, which the table does not have"
                ));
            };
            if matches!(ct.ty, Ty::Str { .. }) {
                ev.push(Event::Asm(super::canon::asm::TSQL_CI));
                ev.push(diff(
                    "equiv.case-sensitivity",
                    format!(
                        "for values of {c} that differ only in letter case ('A' and 'a'), {{T}} rejects the second row as a duplicate of the {what} and {{P}} accepts it"
                    ),
                    at,
                ));
                if matches!(cp.ty, Ty::Str { fixed: false, .. }) {
                    ev.push(diff(
                        "equiv.trailing-spaces",
                        format!(
                            "for values of {c} that differ only in trailing spaces ('a' and 'a '), {{T}} rejects the second row as a duplicate of the {what} and {{P}} accepts it"
                        ),
                        at,
                    ));
                }
            }
        }
        if !kt.primary && !kp.nulls_not_distinct {
            let mut null_cols = Vec::new();
            for c in &cols {
                if let Some(ct) = column_by_key(tt, c) {
                    if nullable(tt, ct)? {
                        null_cols.push(c.clone());
                    }
                }
            }
            if !null_cols.is_empty() {
                ev.push(diff(
                    "equiv.unique-nulls",
                    format!(
                        "two rows with NULL in {}: {{T}} rejects the second as a duplicate of the unique constraint, {{P}} accepts it (UNIQUE NULLS NOT DISTINCT would reject it)",
                        null_cols.join(", ")
                    ),
                    at,
                ));
            }
        }
    }
    for (j, ky) in y.keys.iter().enumerate().filter(|(j, _)| !used[*j]) {
        let _ = j;
        let what = if ky.primary {
            "primary key"
        } else {
            "unique constraint"
        };
        ev.push(diff(
            "equiv.table-key",
            format!(
                "{nb} rejects a second row with the same ({}) by its {what}, {na} has no such {what} and accepts it",
                key_cols(y, ky)?.join(", ")
            ),
            ab(start.0, ky.off),
        ));
    }

    // Foreign keys: matched by column set.
    let mut used = vec![false; y.fks.len()];
    for fx in &x.fks {
        let cols = keys(&fx.cols, da)?;
        let found = y
            .fks
            .iter()
            .enumerate()
            .find(|(j, fy)| !used[*j] && keys(&fy.cols, db).is_ok_and(|c| c == cols));
        let Some((j, fy)) = found else {
            ev.push(diff(
                "equiv.foreign-key",
                format!(
                    "{na} rejects a row whose ({}) has no match in {}, {nb} has no such foreign key and accepts it",
                    cols.join(", "),
                    text_name(&fx.table)
                ),
                ab(fx.off, start.1),
            ));
            continue;
        };
        used[j] = true;
        let at = ab(fx.off, fy.off);
        if table_key(&fx.table, da)? != table_key(&fy.table, db)? {
            ev.push(diff(
                "equiv.foreign-key",
                format!(
                    "the foreign key on ({}) references {} on {na} and {} on {nb}",
                    cols.join(", "),
                    text_name(&fx.table),
                    text_name(&fy.table)
                ),
                at,
            ));
            continue;
        }
        match (&fx.ref_cols, &fy.ref_cols) {
            (Some(a), Some(b)) if keys(a, da)? != keys(b, db)? => ev.push(diff(
                "equiv.foreign-key",
                format!(
                    "the foreign key on ({}) references ({}) on {na} and ({}) on {nb}",
                    cols.join(", "),
                    keys(a, da)?.join(", "),
                    keys(b, db)?.join(", ")
                ),
                at,
            )),
            (Some(_), None) | (None, Some(_)) => {
                return Err(format!(
                    "the foreign key on ({}) names the referenced columns on one side only",
                    cols.join(", ")
                ))
            }
            _ => {}
        }
        for (what, a, b) in [
            ("deleting", &fx.on_delete, &fy.on_delete),
            ("updating", &fx.on_update, &fy.on_update),
        ] {
            if a != b {
                ev.push(diff(
                    "equiv.foreign-key-action",
                    format!(
                        "{what} a referenced row of {}: {na} does {a}, {nb} does {b}",
                        text_name(&fx.table)
                    ),
                    at,
                ));
            }
        }
        let tt = if tsql_a { x } else { y };
        let tsql_cols = if tsql_a { &fx.cols } else { &fy.cols };
        for c in keys(tsql_cols, Dialect::Tsql)? {
            if column_by_key(tt, &c).is_some_and(|ct| matches!(ct.ty, Ty::Str { .. })) {
                ev.push(Event::Asm(super::canon::asm::TSQL_CI));
                ev.push(diff(
                    "equiv.case-sensitivity",
                    format!(
                        "a value 'a' in {c} matches a referenced 'A' on {{T}}, which accepts the row, and not on {{P}}, which rejects it"
                    ),
                    at,
                ));
            }
        }
        if !names_match(&fx.name, &fy.name, da, db)? {
            ev.push(diff(
                "equiv.constraint-name",
                format!(
                    "the foreign key on ({}) is named differently, so a statement that names it finds it on one side only",
                    cols.join(", ")
                ),
                at,
            ));
        }
    }
    for fy in y
        .fks
        .iter()
        .enumerate()
        .filter(|(j, _)| !used[*j])
        .map(|(_, f)| f)
    {
        ev.push(diff(
            "equiv.foreign-key",
            format!(
                "{nb} rejects a row whose ({}) has no match in {}, {na} has no such foreign key and accepts it",
                keys(&fy.cols, db)?.join(", "),
                text_name(&fy.table)
            ),
            ab(start.0, fy.off),
        ));
    }

    // Checks: by name when both sides name them, otherwise in order.
    if x.checks.len() != y.checks.len() {
        ev.push(diff(
            "equiv.check",
            format!(
                "{na} has {} CHECK constraints and {nb} {}, so one side accepts rows the other rejects",
                x.checks.len(),
                y.checks.len()
            ),
            start,
        ));
        return Ok(());
    }
    // Named checks pair with the same name; the rest pair in order.
    let mut pair: Vec<Option<usize>> = vec![None; x.checks.len()];
    let mut used = vec![false; y.checks.len()];
    for (i, cx) in x.checks.iter().enumerate() {
        if cx.name.is_none() {
            continue;
        }
        if let Some(j) = (0..y.checks.len()).find(|&j| {
            !used[j] && names_match(&cx.name, &y.checks[j].name, da, db).unwrap_or(false)
        }) {
            pair[i] = Some(j);
            used[j] = true;
        }
    }
    let mut rest = (0..y.checks.len()).filter(|&j| !used[j]);
    let mut pairs = Vec::new();
    for (i, cx) in x.checks.iter().enumerate() {
        // As many checks on each side: every one finds a partner.
        let j = pair[i]
            .or_else(|| rest.next())
            .ok_or("unpaired CHECK constraints")?;
        pairs.push((cx, &y.checks[j]));
    }
    for (cx, cy) in pairs {
        let at = ab(cx.off, cy.off);
        if !names_match(&cx.name, &cy.name, da, db)? {
            ev.push(diff(
                "equiv.constraint-name",
                format!(
                    "the CHECK constraint `{}` is named {} on {na} and {} on {nb}, so a statement that names it finds it on one side only",
                    text(&cx.pred),
                    cx.name.as_ref().map_or("by the system".into(), |n| n.text.clone()),
                    cy.name.as_ref().map_or("by the system".into(), |n| n.text.clone())
                ),
                at,
            ));
        }
        ev.push(Event::Check {
            what: text(&cx.pred),
            a: cx.pred.clone(),
            b: cy.pred.clone(),
            at,
        });
    }
    Ok(())
}

pub(super) fn text_name(ns: &[Name]) -> String {
    ns.iter()
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join(".")
}
