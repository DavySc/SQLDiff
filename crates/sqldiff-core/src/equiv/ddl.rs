//! `CREATE INDEX`, `CREATE SEQUENCE` and `CREATE SCHEMA` compared across
//! dialects: T-SQL against PostgreSQL or Db2 for i.
//!
//! Like `table.rs`, each statement is read completely, and a clause the
//! model does not know leaves the statement not compared. Clauses that
//! change nothing but storage or how the index is built (fill factor,
//! filegroups, `ONLINE`, `MAXDOP`, PostgreSQL `TABLESPACE`) are passed over
//! as normalizations; nothing else is skipped.
//!
//! What an index changes in results: a unique index rejects rows, and which
//! rows it rejects depends on its key columns, its filter, how it treats
//! NULLs and how the key columns compare strings. Everything else about an
//! index (column order, sort direction, included columns, clustering)
//! shapes only how fast statements run. Two indexes are equal when both
//! agree; a different shape is not proven, since the engine does not judge
//! speed, and a different set of rejected rows is a difference with a case.
//!
//! Facts (SQL Server 2022 and PostgreSQL 18, checked on both): see
//! `docs/cross-dialect-proofs.md`, "Indexes, sequences and schemas".

use sqldiff_syntax::Dialect;

use super::ast::{Tok, K};
use super::table::{close, dotted, filegroup, key, name_of, number, split, text, word, Name};

type R<T> = Result<T, String>;

/// One key column of an index.
#[derive(Debug, Clone)]
pub struct KeyCol {
    pub name: Name,
    pub desc: bool,
    /// PostgreSQL `NULLS FIRST` (`Some(true)`) or `NULLS LAST`.
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Index {
    pub dialect: Dialect,
    pub name: Name,
    pub table: Vec<Name>,
    pub table_toks: Vec<Tok>,
    pub unique: bool,
    /// T-SQL `CLUSTERED`.
    pub clustered: bool,
    pub keys: Vec<KeyCol>,
    pub include: Vec<Name>,
    /// The filter predicate's tokens (T-SQL filtered, PostgreSQL partial index).
    pub filter: Option<Vec<Tok>>,
    /// PostgreSQL `NULLS NOT DISTINCT`.
    pub nulls_not_distinct: bool,
    /// Storage and build clauses passed over, as written.
    pub storage: Vec<String>,
    /// Byte offsets of the statement and of its filter.
    pub off: usize,
    pub filter_off: usize,
}

#[derive(Debug, Clone)]
pub struct Sequence {
    pub dialect: Dialect,
    pub name: Vec<Name>,
    /// Upper case, arguments dropped: `BIGINT`, `INTEGER`, `TINYINT`, ...
    pub ty: Option<String>,
    pub start: Option<i128>,
    pub inc: Option<i128>,
    pub min: Option<i128>,
    pub max: Option<i128>,
    pub cycle: Option<bool>,
    /// `Some(None)` for NO CACHE, `Some(Some(n))` for CACHE n.
    pub cache: Option<Option<i128>>,
    pub storage: Vec<String>,
    pub off: usize,
}

#[derive(Debug, Clone)]
pub struct SchemaDef {
    pub dialect: Dialect,
    pub name: Name,
    pub owner: Option<Name>,
    pub off: usize,
}

#[derive(Debug, Clone)]
pub enum Ddl {
    Index(Index),
    Sequence(Sequence),
    Schema(SchemaDef),
}

impl Ddl {
    pub fn what(&self) -> &'static str {
        match self {
            Ddl::Index(_) => "CREATE INDEX",
            Ddl::Sequence(_) => "CREATE SEQUENCE",
            Ddl::Schema(_) => "CREATE SCHEMA",
        }
    }
}

/// Whether a statement's first words make it one of the statements this
/// module reads (`CREATE … INDEX`, `CREATE SEQUENCE`, `CREATE SCHEMA`).
pub fn is_ddl(words: &[String]) -> bool {
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    if w(0) != "CREATE" {
        return false;
    }
    let mut i = 1;
    if w(1) == "OR" && w(2) == "REPLACE" {
        i = 3;
    }
    matches!(w(i), "SEQUENCE" | "SCHEMA") || (i..words.len().min(i + 4)).any(|j| w(j) == "INDEX")
}

/// The name a part resolves to (see `table::key`); a quoted Db2 for i name
/// that is not all capitals matches no unquoted name.
fn fold(n: &Name, d: Dialect) -> R<String> {
    if d == Dialect::Db2i && n.quoted && n.text != n.text.to_uppercase() {
        return Err(format!(
            "the quoted name \"{}\" keeps its lower-case letters in Db2 for i",
            n.text
        ));
    }
    key(n, d)
}

/// Schema (`None` for the default one) and object name.
pub fn object_key(parts: &[Name], d: Dialect) -> R<(Option<String>, String)> {
    let ks = parts.iter().map(|n| fold(n, d)).collect::<R<Vec<_>>>()?;
    let default = match d {
        Dialect::Tsql => Some("dbo"),
        Dialect::Postgres => Some("public"),
        Dialect::Db2i => None,
    };
    match ks.as_slice() {
        [t] => Ok((None, t.clone())),
        [s, t] if Some(s.as_str()) == default => Ok((None, t.clone())),
        [s, t] => Ok((Some(s.clone()), t.clone())),
        _ => Err("a name with a database or server part".into()),
    }
}

/// What a statement is paired by: kind, schema and name (an index also by
/// its table, since T-SQL index names are per table).
pub fn pair_key(x: &Ddl) -> R<String> {
    Ok(match x {
        Ddl::Index(i) => {
            let (s, t) = object_key(&i.table, i.dialect)?;
            format!(
                "index {}.{t}.{}",
                s.unwrap_or_default(),
                fold(&i.name, i.dialect)?
            )
        }
        Ddl::Sequence(q) => {
            let (s, n) = object_key(&q.name, q.dialect)?;
            format!("sequence {}.{n}", s.unwrap_or_default())
        }
        Ddl::Schema(s) => format!("schema {}", fold(&s.name, s.dialect)?),
    })
}

/// Reads a whole statement (terminator dropped).
pub fn parse(ts: &[Tok], d: Dialect) -> R<Ddl> {
    if !word(ts.first(), "CREATE") {
        return Err("not a CREATE statement".into());
    }
    if word(ts.get(1), "OR") {
        return Err("CREATE OR REPLACE (it replaces an existing object)".into());
    }
    if word(ts.get(1), "SEQUENCE") {
        return sequence(ts, d).map(Ddl::Sequence);
    }
    if word(ts.get(1), "SCHEMA") {
        return schema(ts, d).map(Ddl::Schema);
    }
    index(ts, d).map(Ddl::Index)
}

/// Options in `WITH ( … )` of a T-SQL index that only shape it or how it is
/// built. `IGNORE_DUP_KEY = ON` (an INSERT drops duplicates with a warning
/// instead of failing) and `DROP_EXISTING = ON` (replaces an index) change
/// behaviour, so only their OFF values are among them.
const TSQL_INDEX_OPTIONS: &[&str] = &[
    "PAD_INDEX",
    "FILLFACTOR",
    "SORT_IN_TEMPDB",
    "STATISTICS_NORECOMPUTE",
    "STATISTICS_INCREMENTAL",
    "ONLINE",
    "RESUMABLE",
    "MAX_DURATION",
    "ALLOW_ROW_LOCKS",
    "ALLOW_PAGE_LOCKS",
    "OPTIMIZE_FOR_SEQUENTIAL_KEY",
    "MAXDOP",
    "DATA_COMPRESSION",
    "XML_COMPRESSION",
];

/// `WITH ( name = value, … )` at `i` of a T-SQL index: the index after it.
fn tsql_index_options(ts: &[Tok], i: usize, storage: &mut Vec<String>) -> R<usize> {
    let open = i + 1;
    if !ts.get(open).is_some_and(|t| t.kind == K::LParen) {
        return Err(format!(
            "the index option `{}` (WITH without parentheses)",
            text(&ts[i..ts.len().min(i + 4)])
        ));
    }
    let end = close(ts, open)?;
    for opt in split(&ts[open + 1..end]) {
        let ok = match opt {
            [n, eq, v] if eq.kind == K::Op && eq.text == "=" => {
                TSQL_INDEX_OPTIONS.contains(&n.up.as_str())
                    || (matches!(n.up.as_str(), "IGNORE_DUP_KEY" | "DROP_EXISTING")
                        && v.up == "OFF")
            }
            // `ONLINE = ON (WAIT_AT_LOW_PRIORITY (…))`, `DATA_COMPRESSION = PAGE ON PARTITIONS (…)`.
            [n, eq, ..] if eq.kind == K::Op && eq.text == "=" => {
                matches!(n.up.as_str(), "ONLINE")
            }
            _ => false,
        };
        if !ok {
            return Err(format!("the index option `{}`", text(opt)));
        }
    }
    storage.push(text(&ts[i..=end]));
    Ok(end + 1)
}

/// `( a [ASC|DESC] [NULLS FIRST|LAST], … )` at `i`.
fn key_cols(ts: &[Tok], i: usize, d: Dialect) -> R<(Vec<KeyCol>, usize)> {
    if !ts.get(i).is_some_and(|t| t.kind == K::LParen) {
        return Err("an index without a column list".into());
    }
    let end = close(ts, i)?;
    let mut cols = Vec::new();
    for item in split(&ts[i + 1..end]) {
        let what = || format!("the index key `{}`", text(item));
        let name = item
            .first()
            .filter(|t| matches!(t.kind, K::Word | K::Quoted))
            .and_then(name_of)
            .ok_or_else(|| format!("{} (an expression key)", what()))?;
        let mut j = 1;
        let mut desc = false;
        if word(item.get(j), "ASC") || word(item.get(j), "DESC") {
            desc = item[j].up == "DESC";
            j += 1;
        }
        let mut nulls_first = None;
        if d == Dialect::Postgres && word(item.get(j), "NULLS") {
            nulls_first = match item.get(j + 1).map(|t| t.up.as_str()) {
                Some("FIRST") => Some(true),
                Some("LAST") => Some(false),
                _ => return Err(what()),
            };
            j += 2;
        }
        if j != item.len() {
            // A collation, operator class, or an expression.
            return Err(what());
        }
        cols.push(KeyCol {
            name,
            desc,
            nulls_first,
        });
    }
    Ok((cols, end + 1))
}

fn name_list(ts: &[Tok], i: usize, what: &str) -> R<(Vec<Name>, usize)> {
    if !ts.get(i).is_some_and(|t| t.kind == K::LParen) {
        return Err(format!("{what} without a column list"));
    }
    let end = close(ts, i)?;
    let mut out = Vec::new();
    for item in split(&ts[i + 1..end]) {
        match item {
            [n] => out.push(name_of(n).ok_or_else(|| format!("{what} item `{}`", text(item)))?),
            _ => return Err(format!("{what} item `{}`", text(item))),
        }
    }
    Ok((out, end + 1))
}

/// Where a filter predicate starting at `i` ends: at the first top-level
/// word in `stops`.
fn filter_end(ts: &[Tok], i: usize, stops: &[&str]) -> usize {
    let mut depth = 0usize;
    for (j, t) in ts.iter().enumerate().skip(i) {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            K::Word if depth == 0 && stops.contains(&t.up.as_str()) => return j,
            _ => {}
        }
    }
    ts.len()
}

fn index(ts: &[Tok], d: Dialect) -> R<Index> {
    let mut i = 1;
    let mut unique = false;
    let mut clustered = false;
    let mut storage = Vec::new();
    if word(ts.get(i), "UNIQUE") {
        unique = true;
        i += 1;
        if d == Dialect::Db2i && word(ts.get(i), "WHERE") {
            return Err(
                "UNIQUE WHERE NOT NULL (Db2 for i: how it treats NULLs is not verified, fact V29)"
                    .into(),
            );
        }
    }
    if d == Dialect::Tsql && (word(ts.get(i), "CLUSTERED") || word(ts.get(i), "NONCLUSTERED")) {
        clustered = ts[i].up == "CLUSTERED";
        storage.push(ts[i].up.clone());
        i += 1;
    }
    if !word(ts.get(i), "INDEX") {
        let kind = ts
            .get(i)
            .map(|t| t.up.clone())
            .unwrap_or_default()
            .to_lowercase();
        return Err(match kind.as_str() {
            "columnstore" => {
                "a columnstore index (column-wise storage and batch execution: PostgreSQL and Db2 for i have no counterpart; a plain index or none changes no results, but it is a decision)".into()
            }
            "fulltext" => {
                "a full-text index (CONTAINS and FREETEXT need it; PostgreSQL text search or Db2 for i OmniFind are different features)".into()
            }
            "xml" | "primary" | "spatial" => format!("a {kind} index (no counterpart)"),
            "encoded" => "an encoded vector index (Db2 for i: no T-SQL counterpart)".into(),
            _ => format!("CREATE {} …", text(&ts[1..ts.len().min(i + 1)])),
        });
    }
    i += 1;
    if d == Dialect::Postgres && word(ts.get(i), "CONCURRENTLY") {
        return Err("CREATE INDEX CONCURRENTLY (it cannot run inside a transaction block, T-SQL's CREATE INDEX can)".into());
    }
    if word(ts.get(i), "IF") {
        return Err("CREATE INDEX IF NOT EXISTS (it does nothing when the name is taken)".into());
    }
    if word(ts.get(i), "ON") {
        return Err("an index without a name (the system names it)".into());
    }
    let (iname, j) = dotted(ts, i)?;
    let name = match iname.as_slice() {
        [n] => n.clone(),
        // Db2 for i and PostgreSQL may qualify the index; the index then
        // lives in that schema, which must be the table's.
        [_, n] if d != Dialect::Tsql => n.clone(),
        _ => return Err("an index name with several parts".into()),
    };
    i = j;
    if !word(ts.get(i), "ON") {
        return Err("CREATE INDEX without ON".into());
    }
    i += 1;
    if d == Dialect::Postgres && word(ts.get(i), "ONLY") {
        return Err("CREATE INDEX ON ONLY (a partitioned table's parent)".into());
    }
    let (table, j) = dotted(ts, i)?;
    if table
        .iter()
        .any(|n| n.text.starts_with('#') || n.text.starts_with('@'))
    {
        return Err("an index on a temporary table or table variable".into());
    }
    if iname.len() == 2 {
        let (si, _) = object_key(&iname, d)?;
        let (st, _) = object_key(&table, d)?;
        if si != st {
            return Err("the index is in another schema than its table".into());
        }
    }
    let table_toks = ts[i..j].to_vec();
    i = j;
    if d == Dialect::Postgres && word(ts.get(i), "USING") {
        let m = ts.get(i + 1).map(|t| t.up.clone()).unwrap_or_default();
        if m != "BTREE" {
            return Err(format!(
                "an index USING {} (only btree has the order and uniqueness T-SQL's indexes have)",
                m.to_lowercase()
            ));
        }
        storage.push("USING btree".into());
        i += 2;
    }
    let (keys, j) = key_cols(ts, i, d)?;
    i = j;
    let mut include = Vec::new();
    let mut filter = None;
    let mut filter_off = 0;
    let mut nnd = false;
    while i < ts.len() {
        let w = ts[i].up.clone();
        i = match (d, w.as_str()) {
            (Dialect::Tsql | Dialect::Postgres, "INCLUDE") if include.is_empty() => {
                let (v, j) = name_list(ts, i + 1, "INCLUDE")?;
                include = v;
                j
            }
            (Dialect::Postgres, "NULLS") => {
                match (word(ts.get(i + 1), "NOT"), word(ts.get(i + 2), "DISTINCT")) {
                    (true, true) => {
                        nnd = true;
                        i + 3
                    }
                    _ if word(ts.get(i + 1), "DISTINCT") => i + 2,
                    _ => return Err("NULLS without DISTINCT".into()),
                }
            }
            (Dialect::Tsql | Dialect::Postgres, "WHERE") if filter.is_none() => {
                let stops: &[&str] = if d == Dialect::Tsql {
                    &["WITH", "ON", "FILESTREAM_ON"]
                } else {
                    &[]
                };
                let end = filter_end(ts, i + 1, stops);
                if end == i + 1 {
                    return Err("WHERE without a predicate".into());
                }
                filter = Some(ts[i + 1..end].to_vec());
                filter_off = ts[i].off;
                end
            }
            (Dialect::Tsql, "WITH") => tsql_index_options(ts, i, &mut storage)?,
            // `ON scheme (column)`: partitioning places rows, nothing else.
            (Dialect::Tsql, "ON") if ts.get(i + 2).is_some_and(|t| t.kind == K::LParen) => {
                let end = close(ts, i + 2)?;
                storage.push(text(&ts[i..=end]));
                end + 1
            }
            (Dialect::Tsql, "ON") => filegroup(ts, i, &mut storage)?,
            (Dialect::Postgres, "WITH") => super::table::storage_options(
                ts,
                i,
                &["FILLFACTOR", "DEDUPLICATE_ITEMS"],
                &mut storage,
            )?,
            (Dialect::Postgres, "TABLESPACE") => {
                let n = ts
                    .get(i + 1)
                    .and_then(name_of)
                    .ok_or("TABLESPACE without a name")?;
                storage.push(format!("TABLESPACE {}", n.text));
                i + 2
            }
            _ => return Err(format!("the index clause `{}`", text(&ts[i..]))),
        };
    }
    if unique && clustered && keys.is_empty() {
        return Err("an index without key columns".into());
    }
    Ok(Index {
        dialect: d,
        name,
        table,
        table_toks,
        unique,
        clustered,
        keys,
        include,
        filter,
        nulls_not_distinct: nnd,
        storage,
        off: ts[0].off,
        filter_off,
    })
}

/// The integer type a sequence counts in, and its range.
fn seq_type(ty: &str, d: Dialect) -> R<(&'static str, i128, i128)> {
    Ok(match (d, ty) {
        (_, "SMALLINT") | (Dialect::Postgres, "INT2") => ("smallint", -32768, 32767),
        (_, "INT" | "INTEGER") | (Dialect::Postgres, "INT4") => {
            ("integer", -2147483648, 2147483647)
        }
        (_, "BIGINT") | (Dialect::Postgres, "INT8") => {
            ("bigint", -9223372036854775808, 9223372036854775807)
        }
        (Dialect::Tsql, "TINYINT") => ("tinyint", 0, 255),
        _ => return Err(format!("a sequence AS {}", ty.to_lowercase())),
    })
}

fn sequence(ts: &[Tok], d: Dialect) -> R<Sequence> {
    let mut i = 2;
    if word(ts.get(i), "IF") {
        return Err(
            "CREATE SEQUENCE IF NOT EXISTS (it does nothing when the name is taken)".into(),
        );
    }
    let (name, j) = dotted(ts, i)?;
    if name.len() > 2 {
        return Err("a name with a database or server part".into());
    }
    let mut q = Sequence {
        dialect: d,
        name,
        ty: None,
        start: None,
        inc: None,
        min: None,
        max: None,
        cycle: None,
        cache: None,
        storage: Vec::new(),
        off: ts[0].off,
    };
    i = j;
    let once = |set: bool, what: &str| -> R<()> {
        if set {
            Err(format!("{what} given twice"))
        } else {
            Ok(())
        }
    };
    while i < ts.len() {
        let w = ts[i].up.clone();
        i = match w.as_str() {
            "AS" => {
                once(q.ty.is_some(), "AS")?;
                let t = ts
                    .get(i + 1)
                    .filter(|t| t.kind == K::Word)
                    .ok_or("AS without a type")?;
                if ts.get(i + 2).is_some_and(|t| t.kind == K::LParen) {
                    return Err(format!("a sequence AS {}(…)", t.text));
                }
                q.ty = Some(t.up.clone());
                i + 2
            }
            "START" | "INCREMENT" | "RESTART" => {
                if w == "RESTART" {
                    return Err("RESTART in CREATE SEQUENCE".into());
                }
                let k = if word(ts.get(i + 1), "WITH") || word(ts.get(i + 1), "BY") {
                    i + 2
                } else {
                    i + 1
                };
                let (n, j) = number(ts, k)?;
                if w == "START" {
                    once(q.start.is_some(), "START WITH")?;
                    q.start = Some(n);
                } else {
                    once(q.inc.is_some(), "INCREMENT BY")?;
                    if n == 0 {
                        return Err("INCREMENT BY 0".into());
                    }
                    q.inc = Some(n);
                }
                j
            }
            "MINVALUE" | "MAXVALUE" => {
                let (n, j) = number(ts, i + 1)?;
                if w == "MINVALUE" {
                    once(q.min.is_some(), "MINVALUE")?;
                    q.min = Some(n);
                } else {
                    once(q.max.is_some(), "MAXVALUE")?;
                    q.max = Some(n);
                }
                j
            }
            "NO" => match ts.get(i + 1).map(|t| t.up.as_str()) {
                // NO MINVALUE / NO MAXVALUE: the default bound.
                Some("MINVALUE" | "MAXVALUE") => i + 2,
                Some("CYCLE") => {
                    once(q.cycle.is_some(), "CYCLE")?;
                    q.cycle = Some(false);
                    i + 2
                }
                Some("CACHE") => {
                    once(q.cache.is_some(), "CACHE")?;
                    q.cache = Some(None);
                    i + 2
                }
                Some("ORDER") if d == Dialect::Db2i => {
                    q.storage.push("NO ORDER".into());
                    i + 2
                }
                _ => return Err(format!("the sequence option `{}`", text(&ts[i..]))),
            },
            "NOMINVALUE" | "NOMAXVALUE" if d == Dialect::Db2i => i + 1,
            "NOCYCLE" if d == Dialect::Db2i => {
                once(q.cycle.is_some(), "CYCLE")?;
                q.cycle = Some(false);
                i + 1
            }
            "NOCACHE" if d == Dialect::Db2i => {
                once(q.cache.is_some(), "CACHE")?;
                q.cache = Some(None);
                i + 1
            }
            "CYCLE" => {
                once(q.cycle.is_some(), "CYCLE")?;
                q.cycle = Some(true);
                i + 1
            }
            "CACHE" => {
                once(q.cache.is_some(), "CACHE")?;
                match number(ts, i + 1) {
                    Ok((n, j)) => {
                        q.cache = Some(Some(n));
                        j
                    }
                    // T-SQL `CACHE` alone: the server's cache size.
                    Err(_) if d == Dialect::Tsql => {
                        q.cache = Some(Some(0));
                        i + 1
                    }
                    Err(e) => return Err(e),
                }
            }
            "ORDER" if d == Dialect::Db2i => {
                q.storage.push("ORDER".into());
                i + 1
            }
            _ => return Err(format!("the sequence option `{}`", text(&ts[i..]))),
        };
    }
    Ok(q)
}

fn schema(ts: &[Tok], d: Dialect) -> R<SchemaDef> {
    let mut i = 2;
    if word(ts.get(i), "IF") {
        return Err("CREATE SCHEMA IF NOT EXISTS (it does nothing when the schema exists)".into());
    }
    if word(ts.get(i), "AUTHORIZATION") {
        return Err(
            "CREATE SCHEMA AUTHORIZATION without a schema name (named after the owner)".into(),
        );
    }
    let name = ts
        .get(i)
        .filter(|t| matches!(t.kind, K::Word | K::Quoted))
        .and_then(name_of)
        .ok_or("CREATE SCHEMA without a name")?;
    i += 1;
    let mut owner = None;
    if word(ts.get(i), "AUTHORIZATION") {
        owner = Some(
            ts.get(i + 1)
                .filter(|t| matches!(t.kind, K::Word | K::Quoted))
                .and_then(name_of)
                .ok_or("AUTHORIZATION without a name")?,
        );
        i += 2;
    }
    if i != ts.len() {
        return Err(format!(
            "CREATE SCHEMA with `{}` (schema elements or options)",
            text(&ts[i..])
        ));
    }
    if d == Dialect::Db2i && name.text.chars().count() > 10 {
        return Err(format!(
            "the Db2 for i schema name {} is longer than 10 characters, so its library gets another, generated system name (fact V29)",
            name.text
        ));
    }
    Ok(SchemaDef {
        dialect: d,
        name,
        owner,
        off: ts[0].off,
    })
}

/// A key column's facts, from its table.
#[derive(Debug, Clone)]
pub struct ColFacts {
    pub nullable: bool,
    /// `Some(fixed)` for a character string column (blank-padded when fixed).
    pub string: Option<bool>,
}

/// What comparing two statements found. Byte offsets are (side A, side B).
#[derive(Debug)]
pub enum Event {
    Asm((&'static str, &'static str)),
    Diff {
        rule: &'static str,
        witness: String,
        at: (usize, usize),
    },
    Unknown(String),
    Note(String),
    /// Two index filters: the rows each index covers are those
    /// `SELECT 1 FROM t WHERE filter` selects. Different rows decide
    /// results only for a unique index.
    Filter {
        unique: bool,
        a: Vec<Tok>,
        b: Vec<Tok>,
        at: (usize, usize),
    },
}

pub mod asm {
    pub const CLUSTERED: (&str, &str) = (
        "ASM-CLUSTERED",
        "A clustered index becomes a plain index: no query relies on rows coming back in the clustered key's order without ORDER BY",
    );
    pub const INDEX_NAMESPACE: (&str, &str) = (
        "ASM-INDEX-NAME-SCHEMA",
        "No other index, table, view or sequence in the schema has the index's name on the target (T-SQL index names are only unique per table; PostgreSQL and Db2 for i index names are schema objects)",
    );
    pub const FILTERED_SET_OPTIONS: (&str, &str) = (
        "ASM-TSQL-FILTERED-INDEX-SET-OPTIONS",
        "Sessions that change the table run with ANSI_NULLS, QUOTED_IDENTIFIER, ANSI_PADDING, ANSI_WARNINGS, ARITHABORT and CONCAT_NULL_YIELDS_NULL ON and NUMERIC_ROUNDABORT OFF; otherwise T-SQL rejects the change (Msg 1934) because of the filtered index",
    );
    pub const SEQUENCE_TYPE: (&str, &str) = (
        "ASM-SEQUENCE-TYPE",
        "Expressions that use the sequence's next value do not depend on its data type beyond the values it hands out (T-SQL tinyint becomes smallint)",
    );
    pub const SCHEMA_OWNER: (&str, &str) = (
        "ASM-SCHEMA-OWNER",
        "The schema's owner on the target holds the privileges its owner holds on the source: the role it names, or the user running the script, which stands for T-SQL dbo (the database owner)",
    );
}

fn side_text(s: String, tn: &str, on: &str) -> String {
    s.replace("{T}", tn).replace("{O}", on)
}

/// Compares statement `x` (side A) with `y` (side B), one of them T-SQL.
/// `cols(is_a, column)` gives a key column's facts from its table, when
/// known.
pub fn compare(
    x: &Ddl,
    y: &Ddl,
    na: &str,
    nb: &str,
    cols: &dyn Fn(bool, &str) -> Option<ColFacts>,
) -> Vec<Event> {
    let mut ev = Vec::new();
    let r = match (x, y) {
        (Ddl::Index(a), Ddl::Index(b)) => indexes(a, b, na, nb, cols, &mut ev),
        (Ddl::Sequence(a), Ddl::Sequence(b)) => sequences(a, b, na, nb, &mut ev),
        (Ddl::Schema(a), Ddl::Schema(b)) => schemas(a, b, na, nb, &mut ev),
        _ => Err(format!("{} against {}", x.what(), y.what())),
    };
    if let Err(e) = r {
        ev.push(Event::Unknown(e));
    }
    ev
}

/// Which side is T-SQL; the other must be PostgreSQL or Db2 for i.
fn tsql_side(da: Dialect, db: Dialect) -> R<bool> {
    match (da, db) {
        (Dialect::Tsql, Dialect::Postgres | Dialect::Db2i) => Ok(true),
        (Dialect::Postgres | Dialect::Db2i, Dialect::Tsql) => Ok(false),
        _ => Err("only T-SQL against PostgreSQL or Db2 for i is compared".into()),
    }
}

/// The column names a predicate requires to be NOT NULL: its top-level
/// `AND`ed `c IS NOT NULL` terms.
fn not_null_in(filter: &[Tok], d: Dialect) -> Vec<String> {
    let mut out = Vec::new();
    let mut ts = filter;
    while ts.len() >= 2 && ts[0].kind == K::LParen && close(ts, 0).ok() == Some(ts.len() - 1) {
        ts = &ts[1..ts.len() - 1];
    }
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (j, t) in ts.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            K::Word if depth == 0 && t.up == "OR" => return Vec::new(),
            K::Word if depth == 0 && t.up == "AND" => {
                parts.push(&ts[start..j]);
                start = j + 1;
            }
            _ => {}
        }
    }
    parts.push(&ts[start..]);
    for p in parts {
        if let [c, is, not, null] = p {
            if word(Some(is), "IS") && word(Some(not), "NOT") && word(Some(null), "NULL") {
                if let Some(Ok(k)) = name_of(c).map(|n| fold(&n, d)) {
                    out.push(k);
                }
            }
        }
    }
    out
}

fn indexes(
    x: &Index,
    y: &Index,
    na: &str,
    nb: &str,
    cols: &dyn Fn(bool, &str) -> Option<ColFacts>,
    ev: &mut Vec<Event>,
) -> R<()> {
    let (da, db) = (x.dialect, y.dialect);
    let tsql_a = tsql_side(da, db)?;
    let (tn, on) = if tsql_a { (na, nb) } else { (nb, na) };
    let (t, o) = if tsql_a { (x, y) } else { (y, x) };
    let side = |s: String| side_text(s, tn, on);
    let at = (x.off, y.off);
    let diff = |rule: &'static str, witness: String| Event::Diff {
        rule,
        witness: side(witness),
        at,
    };
    let (ka, kb) = (object_key(&x.table, da)?, object_key(&y.table, db)?);
    ev.push(Event::Asm(super::canon::asm::IDENT_FOLD));
    if ka.0.is_none() && kb.0.is_none() {
        ev.push(Event::Asm(super::canon::asm::DEFAULT_SCHEMA));
    }
    if ka != kb {
        ev.push(diff(
            "equiv.index-table",
            format!(
                "{na} indexes {} and {nb} indexes {}",
                text(&x.table_toks),
                text(&y.table_toks)
            ),
        ));
        return Ok(());
    }
    let (ix, iy) = (fold(&x.name, da)?, fold(&y.name, db)?);
    if ix != iy {
        ev.push(diff(
            "equiv.index-name",
            format!(
                "the index is named {} on {na} and {} on {nb}, so a statement that names it (DROP INDEX, ALTER INDEX, an index hint) finds it on one side only",
                x.name.text, y.name.text
            ),
        ));
    }
    ev.push(Event::Asm(asm::INDEX_NAMESPACE));
    for (ix, n) in [(x, na), (y, nb)] {
        for s in &ix.storage {
            ev.push(Event::Note(format!(
                "index clause passed over on {n}: {s} (storage or how the index is built)"
            )));
        }
    }
    if t.clustered {
        ev.push(Event::Asm(asm::CLUSTERED));
    }
    if x.unique != y.unique {
        let (u, n) = if x.unique { (na, nb) } else { (nb, na) };
        ev.push(diff(
            "equiv.index-unique",
            format!(
                "two rows with the same ({}): {u} rejects the second by its unique index, {n} has a plain index and accepts it",
                key_names(x, da)?.join(", ")
            ),
        ));
        return Ok(());
    }
    let kx = key_names(x, da)?;
    let ky = key_names(y, db)?;
    let (mut sx, mut sy) = (kx.clone(), ky.clone());
    sx.sort();
    sy.sort();
    if x.unique && sx != sy {
        let only_x: Vec<&String> = sx.iter().filter(|c| !sy.contains(c)).collect();
        let only_y: Vec<&String> = sy.iter().filter(|c| !sx.contains(c)).collect();
        let w = match (only_x.is_empty(), only_y.is_empty()) {
            (true, _) => format!(
                "two rows with the same ({}) and a different {}: {na} rejects the second, {nb} accepts it",
                kx.join(", "),
                only_y[0]
            ),
            (_, true) => format!(
                "two rows with the same ({}) and a different {}: {nb} rejects the second, {na} accepts it",
                ky.join(", "),
                only_x[0]
            ),
            _ => format!(
                "the unique index covers ({}) on {na} and ({}) on {nb}: two rows that agree on one set and not the other are accepted on one side only",
                kx.join(", "),
                ky.join(", ")
            ),
        };
        ev.push(diff("equiv.index-key", w));
        return Ok(());
    }
    // The shape: key order, sort directions, included columns.
    let dir = |i: &Index| i.keys.iter().map(|k| k.desc).collect::<Vec<_>>();
    let incl = |i: &Index, d: Dialect| -> R<Vec<String>> {
        let mut v = i
            .include
            .iter()
            .map(|n| fold(n, d))
            .collect::<R<Vec<_>>>()?;
        v.sort();
        Ok(v)
    };
    let (inx, iny) = (incl(x, da)?, incl(y, db)?);
    let mut shape = Vec::new();
    if kx != ky {
        shape.push(format!(
            "key columns ({}) against ({})",
            kx.join(", "),
            ky.join(", ")
        ));
    } else if dir(x) != dir(y) {
        let show = |i: &Index, k: &[String]| {
            k.iter()
                .zip(&i.keys)
                .map(|(c, kc)| format!("{c} {}", if kc.desc { "DESC" } else { "ASC" }))
                .collect::<Vec<_>>()
                .join(", ")
        };
        shape.push(format!(
            "sort order ({}) against ({})",
            show(x, &kx),
            show(y, &ky)
        ));
    }
    if inx != iny {
        shape.push(format!(
            "included columns ({}) against ({})",
            inx.join(", "),
            iny.join(", ")
        ));
    }
    if !shape.is_empty() {
        ev.push(Event::Unknown(format!(
            "the indexes differ in {}: this changes how fast statements run, not their results, and is not judged",
            shape.join(" and ")
        )));
    }
    // NULLs sort first in a T-SQL index and last in an ascending PostgreSQL
    // or Db2 for i index; this places them in the index, nothing else.
    if o.keys.iter().any(|k| k.nulls_first.is_none()) {
        ev.push(Event::Note(format!(
            "NULL position in index keys: {tn} sorts NULLs first, {on} last in ascending keys (first in descending ones); statements with ORDER BY are compared on their own"
        )));
    }
    match (&x.filter, &y.filter) {
        (None, None) => {}
        (Some(a), Some(b)) => ev.push(Event::Filter {
            unique: x.unique,
            a: a.clone(),
            b: b.clone(),
            at: (x.filter_off, y.filter_off),
        }),
        _ => {
            let (f, n) = if x.filter.is_some() {
                (na, nb)
            } else {
                (nb, na)
            };
            ev.push(Event::Unknown(format!(
                "only {f} filters the index; {n} indexes every row{}",
                if x.unique {
                    ", so the unique index rejects rows outside the filter only there"
                } else {
                    ""
                }
            )));
        }
    }
    if t.filter.is_some() {
        ev.push(Event::Asm(asm::FILTERED_SET_OPTIONS));
    }
    if !x.unique {
        return Ok(());
    }
    // What a unique index rejects, column by column.
    let (kt, ko) = if tsql_a { (&kx, &ky) } else { (&ky, &kx) };
    let _ = ko;
    if o.dialect == Dialect::Db2i {
        ev.push(Event::Unknown(
            "a UNIQUE index on Db2 for i: how it treats NULLs and how it compares strings (case, trailing blanks) under the job's sort sequence is not verified on a real system (fact V29)".into(),
        ));
        return Ok(());
    }
    let skip_t = t
        .filter
        .as_ref()
        .map_or(Vec::new(), |f| not_null_in(f, t.dialect));
    let skip_o = o
        .filter
        .as_ref()
        .map_or(Vec::new(), |f| not_null_in(f, o.dialect));
    let mut null_cols = Vec::new();
    for c in kt {
        let ft = cols(tsql_a, c);
        let fo = cols(!tsql_a, c);
        let (Some(ft), Some(fo)) = (ft, fo) else {
            ev.push(Event::Unknown(format!(
                "the unique index's column {c} is not in a table both sides know (CREATE TABLE in the file or the catalog), so its NULLs and its string comparison are not known"
            )));
            return Ok(());
        };
        if ft.nullable && fo.nullable && !skip_t.contains(c) && !skip_o.contains(c) {
            null_cols.push(c.clone());
        }
        if ft.string.is_some() {
            ev.push(Event::Asm(super::canon::asm::TSQL_CI));
            ev.push(diff(
                "equiv.case-sensitivity",
                format!(
                    "for values of {c} that differ only in letter case ('A' and 'a'), {{T}} rejects the second row as a duplicate of the unique index and {{O}} accepts it"
                ),
            ));
            if fo.string == Some(false) {
                ev.push(diff(
                    "equiv.trailing-spaces",
                    format!(
                        "for values of {c} that differ only in trailing spaces ('a' and 'a '), {{T}} rejects the second row as a duplicate of the unique index and {{O}} accepts it"
                    ),
                ));
            }
        }
    }
    if !null_cols.is_empty() && !o.nulls_not_distinct {
        ev.push(diff(
            "equiv.unique-nulls",
            format!(
                "two rows with NULL in {} and the same other key values: {{T}} rejects the second as a duplicate of the unique index, {{O}} accepts it (NULLS NOT DISTINCT would reject it)",
                null_cols.join(", ")
            ),
        ));
    }
    Ok(())
}

fn key_names(i: &Index, d: Dialect) -> R<Vec<String>> {
    i.keys.iter().map(|k| fold(&k.name, d)).collect()
}

/// A sequence with every default filled in: type, start, step, bounds,
/// cycling.
#[derive(Debug, PartialEq, Eq)]
struct Values {
    ty: &'static str,
    start: i128,
    inc: i128,
    min: i128,
    max: i128,
    cycle: bool,
}

/// T-SQL (SQL Server 2022, checked in `sys.sequences`): bigint; bounds the
/// type's range; start at the minimum going up, the maximum going down.
/// PostgreSQL 18 (checked in `pg_sequences`): bigint; going up, minimum 1
/// and maximum the type's; going down, minimum the type's and maximum -1;
/// start at the minimum going up, the maximum going down. Db2 for i: no
/// default is relied on (fact V29), every value must be written.
fn values(q: &Sequence) -> R<Values> {
    let d = q.dialect;
    if d == Dialect::Db2i {
        let mut missing = Vec::new();
        for (set, what) in [
            (q.ty.is_some(), "AS"),
            (q.start.is_some(), "START WITH"),
            (q.inc.is_some(), "INCREMENT BY"),
            (q.min.is_some(), "MINVALUE"),
            (q.max.is_some(), "MAXVALUE"),
            (q.cycle.is_some(), "CYCLE or NO CYCLE"),
        ] {
            if !set {
                missing.push(what);
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "the Db2 for i sequence leaves {} to the defaults, which are not verified on a real system (fact V29)",
                missing.join(", ")
            ));
        }
    }
    let ty = q.ty.clone().unwrap_or_else(|| "BIGINT".into());
    let (ty, lo, hi) = seq_type(&ty, d)?;
    let inc = q.inc.unwrap_or(1);
    let up = inc > 0;
    let min = q.min.unwrap_or(match (d, up) {
        (Dialect::Postgres, true) => 1,
        _ => lo,
    });
    let max = q.max.unwrap_or(match (d, up) {
        (Dialect::Postgres, false) => -1,
        _ => hi,
    });
    let start = q.start.unwrap_or(if up { min } else { max });
    if min < lo || max > hi || min > max || start < min || start > max {
        return Err("a sequence whose bounds or start lie outside its type or each other (its CREATE fails)".into());
    }
    Ok(Values {
        ty,
        start,
        inc,
        min,
        max,
        cycle: q.cycle.unwrap_or(false),
    })
}

fn sequences(x: &Sequence, y: &Sequence, na: &str, nb: &str, ev: &mut Vec<Event>) -> R<()> {
    let (da, db) = (x.dialect, y.dialect);
    let tsql_a = tsql_side(da, db)?;
    let (tn, on) = if tsql_a { (na, nb) } else { (nb, na) };
    let (t, o) = if tsql_a { (x, y) } else { (y, x) };
    let at = (x.off, y.off);
    let (ka, kb) = (object_key(&x.name, da)?, object_key(&y.name, db)?);
    ev.push(Event::Asm(super::canon::asm::IDENT_FOLD));
    if ka.0.is_none() && kb.0.is_none() {
        ev.push(Event::Asm(super::canon::asm::DEFAULT_SCHEMA));
    }
    if ka != kb {
        ev.push(Event::Diff {
            rule: "equiv.sequence-name",
            witness: format!(
                "{na} creates the sequence {} and {nb} {}, so statements naming one find the other missing",
                super::table::text_name(&x.name),
                super::table::text_name(&y.name)
            ),
            at,
        });
        return Ok(());
    }
    let (vx, vy) = (values(x)?, values(y)?);
    let diff = |ev: &mut Vec<Event>, w: String| {
        ev.push(Event::Diff {
            rule: "equiv.sequence",
            witness: w,
            at,
        })
    };
    if vx.start != vy.start {
        diff(
            ev,
            format!(
                "the first value is {} on {na} and {} on {nb}",
                vx.start, vy.start
            ),
        );
    } else if vx.inc != vy.inc {
        diff(
            ev,
            format!(
                "the second value is {} on {na} and {} on {nb}",
                vx.start + vx.inc,
                vy.start + vy.inc
            ),
        );
    } else if vx.min != vy.min || vx.max != vy.max || vx.cycle != vy.cycle {
        let up = vx.inc > 0;
        let end = |v: &Values| if up { v.max } else { v.min };
        let after = |v: &Values, n: &str| {
            if v.cycle {
                format!("{n} restarts at {}", if up { v.min } else { v.max })
            } else {
                format!("{n} fails (the sequence is exhausted)")
            }
        };
        let w = if end(&vx) != end(&vy) {
            let (first, v, n) = if (end(&vx) < end(&vy)) == up {
                (end(&vx), &vx, na)
            } else {
                (end(&vy), &vy, nb)
            };
            let other = if n == na { nb } else { na };
            format!(
                "after the value {first}, {} and {other} goes on",
                after(v, n)
            )
        } else if vx.cycle != vy.cycle {
            format!(
                "after the last value {}, {} and {}",
                end(&vx),
                after(&vx, na),
                after(&vy, nb)
            )
        } else {
            format!(
                "after the last value {}, {na} restarts at {} and {nb} at {}",
                end(&vx),
                if up { vx.min } else { vx.max },
                if up { vy.min } else { vy.max }
            )
        };
        diff(ev, w);
    }
    if vx.ty != vy.ty {
        let (vt, vo) = if tsql_a { (&vx, &vy) } else { (&vy, &vx) };
        let (_, lo, hi) = seq_type(&vo.ty.to_uppercase(), o.dialect)?;
        if vt.ty == "tinyint" && vo.ty == "smallint" && lo <= vt.min && vt.max <= hi {
            ev.push(Event::Asm(asm::SEQUENCE_TYPE));
        } else {
            ev.push(Event::Unknown(format!(
                "the sequence counts in {} on {tn} and {} on {on}",
                vt.ty, vo.ty
            )));
        }
    }
    // The cache: T-SQL hands out values in order across sessions whatever
    // its cache; PostgreSQL does with CACHE 1 (the default), and with a
    // larger cache each session takes its own block of values.
    let cache_o = o.cache.flatten().unwrap_or(1);
    match o.dialect {
        Dialect::Postgres if cache_o > 1 => ev.push(Event::Unknown(format!(
            "{on} caches {cache_o} values per session, so values are not handed out in call order across sessions as on {tn}"
        ))),
        Dialect::Db2i if o.cache != Some(None) => ev.push(Event::Unknown(
            "the Db2 for i sequence caches values: whether they are handed out in call order across jobs is not verified (fact V29); NO CACHE is"
                .to_string(),
        )),
        _ => {}
    }
    ev.push(Event::Note(format!(
        "sequence cache ({}): values are handed out in call order on both sides; values a crash or a rollback skips are gaps on both",
        match t.cache {
            Some(None) => format!("{tn} NO CACHE"),
            Some(Some(0)) | None => format!("{tn} server cache"),
            Some(Some(n)) => format!("{tn} CACHE {n}"),
        }
    )));
    for (q, n) in [(x, na), (y, nb)] {
        for s in &q.storage {
            ev.push(Event::Note(format!(
                "sequence clause passed over on {n}: {s}"
            )));
        }
    }
    Ok(())
}

fn schemas(x: &SchemaDef, y: &SchemaDef, na: &str, nb: &str, ev: &mut Vec<Event>) -> R<()> {
    let (da, db) = (x.dialect, y.dialect);
    let tsql_a = tsql_side(da, db)?;
    let (t, o) = if tsql_a { (x, y) } else { (y, x) };
    let (on, tn) = if tsql_a { (nb, na) } else { (na, nb) };
    let at = (x.off, y.off);
    ev.push(Event::Asm(super::canon::asm::IDENT_FOLD));
    if fold(&x.name, da)? != fold(&y.name, db)? {
        ev.push(Event::Diff {
            rule: "equiv.schema-name",
            witness: format!(
                "{na} creates the schema {} and {nb} {}, so statements naming one find the other missing",
                x.name.text, y.name.text
            ),
            at,
        });
        return Ok(());
    }
    let owner_t = t.owner.as_ref().map(|n| fold(n, t.dialect)).transpose()?;
    let owner_o = o.owner.as_ref().map(|n| fold(n, o.dialect)).transpose()?;
    match (owner_t.as_deref(), owner_o.as_deref()) {
        (a, b) if a == b => ev.push(Event::Asm(asm::SCHEMA_OWNER)),
        (Some("dbo"), None) => ev.push(Event::Asm(asm::SCHEMA_OWNER)),
        (a, b) => ev.push(Event::Unknown(format!(
            "the schema is owned by {} on {tn} and by {} on {on}; privileges are not compared",
            a.unwrap_or("the user running the script"),
            b.unwrap_or("the user running the script")
        ))),
    }
    if o.dialect == Dialect::Db2i {
        ev.push(Event::Note(format!(
            "CREATE SCHEMA on {on} also creates a library with a journal and SQL catalog views"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqldiff_syntax::{parse as cst_parse, SyntaxKind};

    fn stmt(sql: &str, d: Dialect) -> Ddl {
        let root = cst_parse(sql, d).syntax();
        let n = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::STATEMENT)
            .unwrap();
        parse(&super::super::ast::tokens(&n), d).unwrap()
    }

    fn err(sql: &str, d: Dialect) -> String {
        let root = cst_parse(sql, d).syntax();
        let n = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::STATEMENT)
            .unwrap();
        parse(&super::super::ast::tokens(&n), d).unwrap_err()
    }

    #[test]
    fn reads_tsql_index() {
        let Ddl::Index(i) = stmt(
            "CREATE UNIQUE NONCLUSTERED INDEX IX_A ON dbo.T (A ASC, B DESC) INCLUDE (C) WHERE A IS NOT NULL WITH (FILLFACTOR = 90, ONLINE = ON) ON [PRIMARY];",
            Dialect::Tsql,
        ) else {
            panic!()
        };
        assert!(i.unique && !i.clustered);
        assert_eq!(i.keys.len(), 2);
        assert!(i.keys[1].desc);
        assert_eq!(i.include.len(), 1);
        assert_eq!(text(i.filter.as_ref().unwrap()), "A IS NOT NULL");
        assert_eq!(i.storage.len(), 3);
        assert_eq!(
            not_null_in(i.filter.as_ref().unwrap(), Dialect::Tsql),
            ["a"]
        );
    }

    #[test]
    fn refuses_what_changes_behaviour() {
        for (sql, d) in [
            (
                "CREATE UNIQUE INDEX i ON t (a) WITH (IGNORE_DUP_KEY = ON);",
                Dialect::Tsql,
            ),
            (
                "CREATE INDEX i ON t (a) WITH (DROP_EXISTING = ON);",
                Dialect::Tsql,
            ),
            ("CREATE CLUSTERED COLUMNSTORE INDEX i ON t;", Dialect::Tsql),
            ("CREATE INDEX CONCURRENTLY i ON t (a);", Dialect::Postgres),
            ("CREATE INDEX i ON t USING gin (a);", Dialect::Postgres),
            ("CREATE INDEX i ON t ((lower(a)));", Dialect::Postgres),
            ("CREATE INDEX i ON t (a COLLATE \"C\");", Dialect::Postgres),
            ("CREATE INDEX ON t (a);", Dialect::Postgres),
            ("CREATE INDEX i ON t (a) FILESTREAM_ON fs;", Dialect::Tsql),
            ("CREATE SEQUENCE s AS decimal(5, 0);", Dialect::Tsql),
            ("CREATE SCHEMA s CREATE TABLE t (a int);", Dialect::Postgres),
            ("CREATE SCHEMA DATALOADSIMULATION;", Dialect::Db2i),
        ] {
            let e = err(sql, d);
            assert!(!e.is_empty(), "{sql}");
        }
    }

    #[test]
    fn sequence_defaults() {
        let v = |sql: &str, d| match stmt(sql, d) {
            Ddl::Sequence(q) => values(&q).unwrap(),
            _ => panic!(),
        };
        let t = v("CREATE SEQUENCE s AS int;", Dialect::Tsql);
        assert_eq!(
            (t.start, t.min, t.max),
            (-2147483648, -2147483648, 2147483647)
        );
        let t = v("CREATE SEQUENCE s AS int INCREMENT BY -1;", Dialect::Tsql);
        assert_eq!(t.start, 2147483647);
        let p = v("CREATE SEQUENCE s AS integer;", Dialect::Postgres);
        assert_eq!((p.start, p.min, p.max), (1, 1, 2147483647));
        let p = v(
            "CREATE SEQUENCE s AS integer INCREMENT BY -1;",
            Dialect::Postgres,
        );
        assert_eq!((p.start, p.min, p.max), (-1, -2147483648, -1));
        let p = v("CREATE SEQUENCE s;", Dialect::Postgres);
        assert_eq!(p.ty, "bigint");
    }
}
