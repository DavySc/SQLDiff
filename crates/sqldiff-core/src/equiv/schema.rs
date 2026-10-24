//! The few catalog facts statement equivalence needs: column types,
//! nullability, identity columns and unique keys, read from CREATE TABLE and
//! CREATE UNIQUE INDEX statements in DDL artifacts.
//!
//! Built from the shared catalog (`crate::catalog`) that binding uses.

use std::collections::HashMap;

use sqldiff_syntax::{parse, Dialect};

use super::ast::Type;
use crate::artifact::Artifact;
use crate::catalog::{ddl, Catalog, CatalogView, DataType, TableKind};

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
            // T-SQL bit: an integer that is 0 or 1.
            "BIT" if d == Dialect::Tsql => Ty {
                fam: Fam::Int,
                len: Some(1),
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
            "DECIMAL" | "NUMERIC" | "DEC" => {
                // Without precision: T-SQL (18,0), Db2 for i (5,0), PostgreSQL unbounded.
                let default = match d {
                    Dialect::Tsql => Some(18),
                    Dialect::Db2i => Some(5),
                    Dialect::Postgres => None,
                };
                let len = n(0).or(if t.args.is_empty() { default } else { None });
                Ty {
                    fam: Fam::Exact,
                    len,
                    scale: n(1).or(len.map(|_| 0)),
                }
            }
            "REAL" | "FLOAT" | "DOUBLE PRECISION" | "DOUBLE" | "FLOAT8" | "FLOAT4" => {
                Ty::of(Fam::Float)
            }
            "VARCHAR" | "NVARCHAR" | "CHARACTER VARYING" | "CHAR VARYING" | "VARCHAR2" => {
                fam_len(Fam::VarChar, tsql_default)
            }
            "TEXT" => Ty::of(Fam::VarChar),
            // T-SQL sysname is nvarchar(128) NOT NULL.
            "SYSNAME" if d == Dialect::Tsql => Ty {
                fam: Fam::VarChar,
                len: Some(128),
                scale: None,
            },
            // Large and double-byte character types compare as strings too
            // (collation applies); CITEXT is left out on purpose (case-insensitive).
            "CLOB" | "NCLOB" | "DBCLOB" | "NTEXT" | "VARGRAPHIC" | "LONG VARCHAR"
            | "LONG VARGRAPHIC" => Ty {
                fam: Fam::VarChar,
                len: if max { None } else { n(0) },
                scale: None,
            },
            "GRAPHIC" => Ty {
                fam: Fam::Char,
                len: n(0).or(Some(1)),
                scale: None,
            },
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
    /// PostgreSQL: the system catalogs are known too.
    pg_system: bool,
    /// T-SQL: the `sys` catalog views are known too.
    tsql_system: bool,
}

/// The SQL Server catalog views in `sys_catalog.sql`.
fn tsql_system() -> &'static Schema {
    static SYSTEM: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
    SYSTEM.get_or_init(|| {
        let text = include_str!("sys_catalog.sql");
        let mut cat = Catalog::default();
        ddl::load_tree(
            &mut cat,
            &parse(text, Dialect::Tsql).syntax(),
            Dialect::Tsql,
        );
        let mut s = Schema::from_view(&CatalogView::new(Dialect::Tsql, &[&cat]));
        s.tsql_system = false;
        s
    })
}

/// The PostgreSQL system catalogs in `pg_catalog.sql`.
fn pg_system() -> &'static Schema {
    static SYSTEM: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
    SYSTEM.get_or_init(|| {
        let text = include_str!("pg_catalog.sql");
        let mut cat = Catalog::default();
        ddl::load_tree(
            &mut cat,
            &parse(text, Dialect::Postgres).syntax(),
            Dialect::Postgres,
        );
        let mut s = Schema::from_view(&CatalogView::new(Dialect::Postgres, &[&cat]));
        s.pg_system = false;
        s
    })
}

impl Schema {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// The tables of the catalog (system catalogs left out).
    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.values()
    }

    /// A table by name, qualified or not; user tables are found by their
    /// unqualified name.
    pub fn table(&self, name: &str) -> Option<&Table> {
        let name = name.to_lowercase();
        let parts: Vec<&str> = name.split('.').collect();
        let last = *parts.last()?;
        // SQL Server catalog views: `sys.name`.
        if parts.len() >= 2 && parts[parts.len() - 2] == "sys" {
            return self
                .tsql_system
                .then(|| tsql_system().tables.get(last))
                .flatten();
        }
        self.tables.get(last).or_else(|| {
            // pg_catalog comes first on every search path.
            (self.pg_system && last.starts_with("pg_"))
                .then(|| pg_system().tables.get(last))
                .flatten()
        })
    }

    /// Reads DDL artifacts through the shared catalog loader; files fold in order.
    pub fn load(artifacts: &[Artifact]) -> Schema {
        let Some(first) = artifacts.first() else {
            return Schema::default();
        };
        let mut cat = Catalog::default();
        for a in artifacts {
            let root = parse(&a.text, a.dialect).syntax();
            ddl::load_tree(&mut cat, &root, a.dialect);
            cat.origin = a.path.clone();
        }
        let mut s = Schema::from_view(&CatalogView::new(first.dialect, &[&cat]));
        s.sources = artifacts.iter().map(|a| a.path.clone()).collect();
        s
    }

    /// The facts equivalence needs from one side's catalog (DDL, bundles and
    /// adapter catalogs, as the binder sees them).
    pub fn from_view(view: &CatalogView) -> Schema {
        let d = view.dialect;
        let mut s = Schema {
            sources: view.origins.clone(),
            pg_system: d == Dialect::Postgres,
            tsql_system: d == Dialect::Tsql,
            ..Schema::default()
        };
        // Base and temporary tables only: view columns are derived, and their
        // types and nullability are not facts equivalence may rely on.
        for t in view
            .catalog
            .tables
            .iter()
            .filter(|t| matches!(t.kind, TableKind::Table | TableKind::Temporary))
        {
            let lower = |v: &[String]| v.iter().map(|c| c.to_lowercase()).collect::<Vec<_>>();
            let mut keys: Vec<Vec<String>> = t.keys.iter().map(|k| lower(&k.columns)).collect();
            keys.extend(
                t.indexes
                    .iter()
                    .filter(|i| i.unique)
                    .map(|i| lower(&i.columns)),
            );
            let columns = t
                .columns
                .iter()
                .map(|c| Column {
                    name: c.name.to_lowercase(),
                    ty: ty_of(&c.data_type, d),
                    not_null: !c.nullable,
                    identity: c.identity,
                })
                .collect();
            let name = t.name.to_lowercase();
            s.tables.insert(
                name.clone(),
                Table {
                    name,
                    columns,
                    keys,
                },
            );
        }
        s
    }
}

fn ty_of(t: &DataType, d: Dialect) -> Ty {
    if !t.is_known() {
        return Ty::of(Fam::Other);
    }
    let mut args: Vec<String> = t.args.iter().map(u32::to_string).collect();
    if t.max {
        args.insert(0, "MAX".to_string());
    }
    Ty::from_type(
        &Type {
            name: t.base.clone(),
            args,
        },
        d,
        false,
    )
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

    #[test]
    fn unique_indexes_if_not_exists_and_decimal_defaults() {
        let ddl =
            "CREATE TABLE IF NOT EXISTS t (a int NOT NULL, b int NOT NULL, c int, amt decimal);
                   CREATE UNIQUE INDEX ux_ab ON t (a, b DESC);
                   CREATE UNIQUE INDEX ux_c ON t (c) WHERE c IS NOT NULL;
                   CREATE UNIQUE INDEX ux_expr ON t (lower(a));";
        let s = Schema::load(&[Artifact::new("t.sql", Dialect::Postgres, ddl)]);
        let t = s.table("t").expect("IF NOT EXISTS table");
        assert!(t.is_unique(&["a".into(), "b".into()]));
        assert!(!t.is_unique(&["a".into()]));
        assert!(!t.is_unique(&["c".into()]), "partial index is not a key");
        assert_eq!(t.column("amt").unwrap().ty.len, None);
        let ty = |d: Dialect| {
            let s = Schema::load(&[Artifact::new("t.sql", d, "CREATE TABLE t (amt decimal);")]);
            let c = s.table("t").unwrap().column("amt").unwrap().ty;
            (c.len, c.scale)
        };
        assert_eq!(ty(Dialect::Tsql), (Some(18), Some(0)));
        assert_eq!(ty(Dialect::Db2i), (Some(5), Some(0)));
        assert_eq!(ty(Dialect::Postgres), (None, None));
    }

    #[test]
    fn large_character_types_are_strings() {
        for (ddl, d) in [
            ("CREATE TABLE t (a ntext, b nvarchar(max));", Dialect::Tsql),
            ("CREATE TABLE t (a clob(1M), b dbclob(100));", Dialect::Db2i),
            (
                "CREATE TABLE t (a vargraphic(20), b graphic(4));",
                Dialect::Db2i,
            ),
        ] {
            let s = Schema::load(&[Artifact::new("t.sql", d, ddl)]);
            let t = s.table("t").unwrap();
            for c in &t.columns {
                assert!(c.ty.is_string(), "{ddl}: {} is {:?}", c.name, c.ty);
            }
        }
    }
}
