//! The few catalog facts statement equivalence needs: column types,
//! nullability, identity columns and unique keys, read from CREATE TABLE and
//! CREATE UNIQUE INDEX statements in DDL artifacts.
//!
//! Deliberately minimal and private to `equiv`; a full catalog model with
//! binding is separate work and can replace this behind [`Schema::table`].

use std::collections::HashMap;

use sqldiff_syntax::{parse, Dialect, SyntaxKind};

use super::ast::{tokens, Parser, Tok, Type, K};
use crate::artifact::Artifact;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fam {
    Int,
    Exact,
    Float,
    /// Fixed-length character (blank-padded).
    Char,
    /// Variable-length character; `len` None means unbounded.
    VarChar,
    Date,
    Timestamp,
    TimestampTz,
    Time,
    Bool,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ty {
    pub fam: Fam,
    /// Character length, integer width in bits, or decimal precision.
    pub len: Option<u32>,
    pub scale: Option<u32>,
}

impl Ty {
    pub const fn of(fam: Fam) -> Ty {
        Ty {
            fam,
            len: None,
            scale: None,
        }
    }

    pub fn is_string(&self) -> bool {
        matches!(self.fam, Fam::Char | Fam::VarChar)
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self.fam, Fam::Int | Fam::Exact | Fam::Float)
    }

    pub fn is_temporal(&self) -> bool {
        matches!(
            self.fam,
            Fam::Date | Fam::Timestamp | Fam::TimestampTz | Fam::Time
        )
    }

    /// Type from a declared SQL type in a dialect. `in_cast` matters for the
    /// T-SQL default length of `varchar` (30 in CAST, 1 elsewhere).
    pub fn from_type(t: &Type, d: Dialect, in_cast: bool) -> Ty {
        let n = |i: usize| t.args.get(i).and_then(|a| a.parse::<u32>().ok());
        let max = t.args.first().is_some_and(|a| a == "MAX");
        let fam_len = |fam: Fam, default: Option<u32>| Ty {
            fam,
            len: if max { None } else { n(0).or(default) },
            scale: None,
        };
        let tsql_default = if d == Dialect::Tsql {
            Some(if in_cast { 30 } else { 1 })
        } else {
            None
        };
        match t.name.as_str() {
            "INT" | "INTEGER" | "INT4" => Ty {
                fam: Fam::Int,
                len: Some(32),
                scale: None,
            },
            "SMALLINT" | "INT2" => Ty {
                fam: Fam::Int,
                len: Some(16),
                scale: None,
            },
            "BIGINT" | "INT8" => Ty {
                fam: Fam::Int,
                len: Some(64),
                scale: None,
            },
            "DECIMAL" | "NUMERIC" | "DEC" => Ty {
                fam: Fam::Exact,
                len: n(0),
                scale: n(1).or(t.args.first().map(|_| 0)),
            },
            "REAL" | "FLOAT" | "DOUBLE PRECISION" | "DOUBLE" | "FLOAT8" | "FLOAT4" => {
                Ty::of(Fam::Float)
            }
            "VARCHAR" | "NVARCHAR" | "CHARACTER VARYING" | "CHAR VARYING" | "VARCHAR2" => {
                fam_len(Fam::VarChar, tsql_default)
            }
            "TEXT" => Ty::of(Fam::VarChar),
            "CHAR" | "NCHAR" | "CHARACTER" | "BPCHAR" => Ty {
                fam: Fam::Char,
                len: n(0).or(Some(1)),
                scale: None,
            },
            "DATE" => Ty::of(Fam::Date),
            "DATETIME2"
            | "DATETIME"
            | "SMALLDATETIME"
            | "TIMESTAMP"
            | "TIMESTAMP WITHOUT TIME ZONE" => Ty::of(Fam::Timestamp),
            "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" | "DATETIMEOFFSET" => {
                Ty::of(Fam::TimestampTz)
            }
            "TIME" => Ty::of(Fam::Time),
            "BOOLEAN" | "BOOL" => Ty::of(Fam::Bool),
            _ => Ty::of(Fam::Other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub ty: Ty,
    pub not_null: bool,
    pub identity: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    /// Primary key and unique constraints or indexes, column names lower-cased.
    pub keys: Vec<Vec<String>>,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        let n = name.to_lowercase();
        self.columns.iter().find(|c| c.name == n)
    }

    pub fn identity(&self) -> Option<&Column> {
        self.columns.iter().find(|c| c.identity)
    }

    /// Whether the given columns include every column of some unique key.
    pub fn is_unique(&self, cols: &[String]) -> bool {
        self.keys
            .iter()
            .any(|k| !k.is_empty() && k.iter().all(|c| cols.contains(c)))
    }
}

/// Tables by lower-cased unqualified name.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    tables: HashMap<String, Table>,
    pub sources: Vec<String>,
}

impl Schema {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.get(&name.to_lowercase())
    }

    pub fn load(artifacts: &[Artifact]) -> Schema {
        let mut s = Schema::default();
        for a in artifacts {
            s.sources.push(a.path.clone());
            s.add_ddl(&a.text, a.dialect);
        }
        s
    }

    fn add_ddl(&mut self, text: &str, d: Dialect) {
        let root = parse(text, d).syntax();
        for node in root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        {
            let toks = tokens(&node);
            let w = |i: usize| toks.get(i).map(|t| t.up.as_str()).unwrap_or("");
            if w(0) != "CREATE" {
                continue;
            }
            if w(1) == "TABLE" {
                if let Some(t) = create_table(&toks[2..], d) {
                    self.tables.insert(t.name.clone(), t);
                }
            } else if w(1) == "UNIQUE" && w(2) == "INDEX" {
                if let Some((table, cols)) = unique_index(&toks[3..], d) {
                    if let Some(t) = self.tables.get_mut(&table) {
                        t.keys.push(cols);
                    }
                }
            }
        }
    }
}

fn split_top(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Comma if depth == 0 => {
                out.push(&toks[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&toks[start..]);
    out
}

fn paren_names(toks: &[Tok]) -> Vec<String> {
    toks.iter()
        .skip_while(|t| t.kind != K::LParen)
        .take_while(|t| t.kind != K::RParen)
        .filter(|t| matches!(t.kind, K::Word | K::Quoted))
        .map(|t| sqldiff_syntax::ast::unquote(&t.text).to_lowercase())
        .collect()
}

fn create_table(toks: &[Tok], d: Dialect) -> Option<Table> {
    let mut p = Parser::new(toks, d);
    let name = p.qname().ok()?.last()?.to_lowercase();
    let open = toks.iter().position(|t| t.kind == K::LParen)?;
    let close = toks.iter().rposition(|t| t.kind == K::RParen)?;
    let mut t = Table {
        name,
        ..Table::default()
    };
    let mut pk: Vec<String> = Vec::new();
    for el in split_top(&toks[open + 1..close]) {
        let Some(first) = el.first() else { continue };
        match first.up.as_str() {
            "PRIMARY" => pk = paren_names(el),
            "UNIQUE" => t.keys.push(paren_names(el)),
            "CONSTRAINT" | "FOREIGN" | "CHECK" | "INDEX" => {
                if el.iter().any(|x| x.up == "PRIMARY") {
                    pk = paren_names(el);
                } else if el.iter().any(|x| x.up == "UNIQUE") {
                    t.keys.push(paren_names(el));
                }
            }
            _ => {
                let mut p = Parser::new(el, d);
                let Ok(cname) = p.qname() else { continue };
                let ty = p
                    .ty()
                    .map(|ty| Ty::from_type(&ty, d, false))
                    .unwrap_or(Ty::of(Fam::Other));
                let rest = &el[p.i..];
                let has = |a: &str, b: &str| rest.windows(2).any(|w| w[0].up == a && w[1].up == b);
                let name = cname.last().unwrap().to_lowercase();
                let is_pk = has("PRIMARY", "KEY");
                if is_pk {
                    pk = vec![name.clone()];
                }
                if rest.iter().any(|x| x.up == "UNIQUE") {
                    t.keys.push(vec![name.clone()]);
                }
                t.columns.push(Column {
                    not_null: is_pk || has("NOT", "NULL"),
                    identity: rest.iter().any(|x| x.up == "IDENTITY"),
                    name,
                    ty,
                });
            }
        }
    }
    if !pk.is_empty() {
        for c in &mut t.columns {
            if pk.contains(&c.name) {
                c.not_null = true;
            }
        }
        t.keys.insert(0, pk);
    }
    Some(t)
}

fn unique_index(toks: &[Tok], d: Dialect) -> Option<(String, Vec<String>)> {
    let on = toks.iter().position(|t| t.up == "ON")?;
    let mut p = Parser::new(&toks[on + 1..], d);
    let table = p.qname().ok()?.last()?.to_lowercase();
    let cols = paren_names(&toks[on + 1..]);
    // Expression indexes (`lower(email)`) are not column keys.
    if toks[on + 1..]
        .windows(2)
        .any(|w| w[0].kind == K::Word && w[1].kind == K::LParen && w[0].up != "ON")
        && cols.len() > 1
    {
        return None;
    }
    Some((table, cols))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_corpus_catalogs() {
        for (file, d) in [
            ("tsql.sql", Dialect::Tsql),
            ("pg.sql", Dialect::Postgres),
            ("db2i.sql", Dialect::Db2i),
        ] {
            let path =
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/catalog/").to_string() + file;
            let Ok(text) = std::fs::read_to_string(&path) else {
                return;
            };
            let s = Schema::load(&[Artifact::new(path, d, text)]);
            let c = s.table("customer").expect("customer");
            assert!(c.column("last_name").unwrap().not_null, "{file}");
            assert!(!c.column("first_name").unwrap().not_null, "{file}");
            assert!(c.is_unique(&["customer_id".into()]), "{file}");
            assert!(c.is_unique(&["email".into()]), "{file}");
            assert!(c.column("email").unwrap().ty.is_string());
            let o = s.table("orders").unwrap();
            assert_eq!(
                o.identity().map(|c| c.name.as_str()),
                Some("order_id"),
                "{file}"
            );
            assert!(!s
                .table("price_feed")
                .unwrap()
                .is_unique(&["item_id".into()]));
            let ol = s.table("order_line").unwrap();
            assert!(
                ol.is_unique(&["order_id".into(), "line_no".into()]),
                "{file}"
            );
            assert!(!ol.is_unique(&["order_id".into()]), "{file}");
        }
    }
}
