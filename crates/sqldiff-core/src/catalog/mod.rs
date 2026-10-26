//! Catalog facts for binding (design §3 layer 2): tables, columns, keys,
//! indexes and routine signatures, looked up with per-dialect identifier
//! folding.
//!
//! Facts come from DDL (`ddl::load`, e.g. `corpus/catalog/*.sql`), from a
//! source adapter's `get_catalog` or a snapshot bundle (converted by
//! `sqldiff-source`), and from objects an artifact creates itself (temporary
//! tables, table variables, routines defined in the same file).

pub mod builtins;
pub mod ddl;
pub mod types;

use serde::{Deserialize, Serialize};
use sqldiff_syntax::Dialect;

pub use types::DataType;

/// One identifier as written in source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    /// Unquoted text.
    pub text: String,
    pub quoted: bool,
}

impl Ident {
    pub fn new(text: impl Into<String>, quoted: bool) -> Self {
        Ident {
            text: text.into(),
            quoted,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Ident::new(text, false)
    }

    /// The name the engine stores for this identifier.
    pub fn stored(&self, dialect: Dialect) -> String {
        if self.quoted {
            return self.text.clone();
        }
        match dialect {
            Dialect::Postgres => self.text.to_lowercase(),
            Dialect::Db2i => self.text.to_uppercase(),
            Dialect::Tsql => self.text.clone(),
        }
    }

    /// Lookup key in `dialect`.
    pub fn key(&self, dialect: Dialect) -> String {
        key(dialect, &self.stored(dialect))
    }
}

/// Comparison key for a stored name: SQL Server compares by collation (assumed
/// case-insensitive), the others compare stored names exactly.
pub fn key(dialect: Dialect, stored: &str) -> String {
    match dialect {
        Dialect::Tsql => stored.to_lowercase(),
        Dialect::Postgres | Dialect::Db2i => stored.to_string(),
    }
}

/// Schema an unqualified name resolves to when nothing says otherwise.
pub fn default_schema(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::Tsql => Some("dbo"),
        Dialect::Postgres => Some("public"),
        // SQL naming: the current schema (user profile) or the library list.
        Dialect::Db2i => None,
    }
}

/// Schemas every database of the dialect has.
pub fn system_schemas(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::Tsql => &["dbo", "sys", "information_schema", "guest"],
        Dialect::Postgres => &["public", "pg_catalog", "information_schema", "pg_temp"],
        Dialect::Db2i => &[
            "QSYS2",
            "QSYS",
            "SYSIBM",
            "SYSPROC",
            "SYSTOOLS",
            "SYSIBMADM",
            "QTEMP",
            "SESSION",
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Table,
    View,
    /// Session or transaction temporary table (`#t`, `CREATE TEMP TABLE`, `DECLARE GLOBAL TEMPORARY TABLE`).
    Temporary,
    /// T-SQL table variable.
    Variable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: DataType,
    pub nullable: bool,
    #[serde(default)]
    pub identity: bool,
    #[serde(default)]
    pub has_default: bool,
    /// The database computes the value and rejects one written by a
    /// statement: T-SQL computed and GENERATED ALWAYS AS ROW START / END
    /// columns, PostgreSQL GENERATED ALWAYS AS (expr).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub primary: bool,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<Column>,
    /// False when the column list could not be read (`CREATE TABLE ... AS`,
    /// `SELECT ... INTO #t`, views over `*`): columns are then not checked.
    pub columns_known: bool,
    #[serde(default)]
    pub keys: Vec<Key>,
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub triggers: Vec<String>,
}

impl Table {
    pub fn new(schema: Option<String>, name: impl Into<String>, kind: TableKind) -> Self {
        Table {
            schema,
            name: name.into(),
            kind,
            columns: Vec::new(),
            columns_known: true,
            keys: Vec::new(),
            indexes: Vec::new(),
            triggers: Vec::new(),
        }
    }

    pub fn column(&self, dialect: Dialect, name: &Ident) -> Option<&Column> {
        let k = name.key(dialect);
        self.columns.iter().find(|c| key(dialect, &c.name) == k)
    }

    pub fn display(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether a unique key or unique index covers exactly these columns.
    pub fn is_unique_on(&self, dialect: Dialect, cols: &[&str]) -> bool {
        let want: std::collections::BTreeSet<String> =
            cols.iter().map(|c| key(dialect, c)).collect();
        let same = |cs: &[String]| {
            cs.iter()
                .map(|c| key(dialect, c))
                .collect::<std::collections::BTreeSet<_>>()
                == want
        };
        self.keys.iter().any(|k| same(&k.columns))
            || self.indexes.iter().any(|i| i.unique && same(&i.columns))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineKind {
    Procedure,
    Function,
    Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamMode {
    In,
    Out,
    InOut,
    Variadic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    /// Empty for unnamed PostgreSQL parameters. T-SQL names keep their `@`.
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: DataType,
    pub mode: ParamMode,
    pub has_default: bool,
    /// The default expression as written, when the DDL was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub kind: RoutineKind,
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<DataType>,
}

impl Routine {
    pub fn display(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Parameters a caller passes: PostgreSQL functions take only IN, INOUT and VARIADIC.
    pub fn call_params(&self, dialect: Dialect) -> Vec<&Param> {
        self.params
            .iter()
            .filter(|p| {
                !(dialect == Dialect::Postgres
                    && self.kind == RoutineKind::Function
                    && p.mode == ParamMode::Out)
            })
            .collect()
    }

    pub fn signature(&self) -> String {
        let ps: Vec<String> = self
            .params
            .iter()
            .map(|p| {
                let mode = match p.mode {
                    ParamMode::In => "",
                    ParamMode::Out => "OUT ",
                    ParamMode::InOut => "INOUT ",
                    ParamMode::Variadic => "VARIADIC ",
                };
                let def = if p.has_default { " = default" } else { "" };
                format!("{mode}{} {}{def}", p.name, p.data_type.text)
                    .trim()
                    .to_string()
            })
            .collect();
        format!("{}({})", self.display(), ps.join(", "))
    }
}

/// Catalog facts from one source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// Where the facts came from, for witnesses (`corpus/catalog/pg.sql`, `bundle:<id>`).
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub tables: Vec<Table>,
    #[serde(default)]
    pub routines: Vec<Routine>,
    /// Schemas created explicitly.
    #[serde(default)]
    pub schemas: Vec<String>,
}

impl Catalog {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.routines.is_empty() && self.schemas.is_empty()
    }

    /// Adds or replaces a table (same schema and name).
    pub fn put_table(&mut self, dialect: Dialect, t: Table) {
        let k = (
            t.schema.as_deref().map(|s| key(dialect, s)),
            key(dialect, &t.name),
        );
        self.tables.retain(|x| {
            (
                x.schema.as_deref().map(|s| key(dialect, s)),
                key(dialect, &x.name),
            ) != k
        });
        self.tables.push(t);
    }

    pub fn put_routine(&mut self, dialect: Dialect, r: Routine) {
        // Overloads are kept apart by parameter count, and in PostgreSQL by
        // the parameter types too (its routines are identified by them).
        let types = |r: &Routine| -> Vec<String> {
            if dialect != Dialect::Postgres {
                return Vec::new();
            }
            r.call_params(dialect)
                .iter()
                // Lengths, precision and scale are not part of it.
                .map(|p| p.data_type.base.clone())
                .collect()
        };
        let k = (
            r.schema.as_deref().map(|s| key(dialect, s)),
            key(dialect, &r.name),
            r.params.len(),
            types(&r),
        );
        self.routines.retain(|x| {
            (
                x.schema.as_deref().map(|s| key(dialect, s)),
                key(dialect, &x.name),
                x.params.len(),
                types(x),
            ) != k
        });
        self.routines.push(r);
    }

    pub fn extend(&mut self, dialect: Dialect, other: &Catalog) {
        for t in &other.tables {
            self.put_table(dialect, t.clone());
        }
        for r in &other.routines {
            self.put_routine(dialect, r.clone());
        }
        for s in &other.schemas {
            if !self
                .schemas
                .iter()
                .any(|x| key(dialect, x) == key(dialect, s))
            {
                self.schemas.push(s.clone());
            }
        }
    }
}

/// Outcome of looking a name up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<T> {
    Found(T),
    /// The qualifier names a schema the catalog does not know.
    NoSchema(String),
    NotFound,
    /// Three- and four-part names, linked servers: outside what a catalog describes.
    OutOfScope,
}

fn schema_matches(d: Dialect, stored: Option<&str>, want: Option<&Ident>) -> bool {
    match (stored, want) {
        (_, None) => match (stored, default_schema(d)) {
            (None, _) => true,
            (Some(s), Some(def)) => key(d, s) == key(d, &stored_sys(d, def)),
            // Db2 library list: an unqualified name may live in any schema.
            (Some(_), None) => true,
        },
        (Some(s), Some(w)) => key(d, s) == w.key(d),
        (None, Some(w)) => match default_schema(d) {
            Some(def) => w.key(d) == key(d, &stored_sys(d, def)),
            // Db2: unqualified DDL lands in the current schema, which we do not know.
            None => true,
        },
    }
}

/// Index of the table a qualified name (`[schema.]name`) refers to, without
/// building a view (the DDL loader looks tables up on every statement).
pub fn table_position(d: Dialect, cat: &Catalog, parts: &[Ident]) -> Option<usize> {
    let (schema, name) = match parts {
        [n] => (None, n),
        [s, n] => (Some(s), n),
        _ => return None,
    };
    let k = name.key(d);
    cat.tables
        .iter()
        .position(|t| key(d, &t.name) == k && schema_matches(d, t.schema.as_deref(), schema))
}

/// The merged catalog one side binds against.
#[derive(Debug, Clone)]
pub struct CatalogView {
    pub dialect: Dialect,
    pub catalog: Catalog,
    /// Whether any catalog was supplied for this side. Without one, persistent
    /// objects stay unresolved and become assumptions.
    pub supplied: bool,
    /// Origins of the supplied catalogs, for witnesses.
    pub origins: Vec<String>,
}

impl CatalogView {
    pub fn new(dialect: Dialect, sources: &[&Catalog]) -> Self {
        let mut catalog = Catalog::default();
        let mut origins = Vec::new();
        for c in sources {
            catalog.extend(dialect, c);
            if !c.origin.is_empty() && !origins.contains(&c.origin) {
                origins.push(c.origin.clone());
            }
        }
        CatalogView {
            dialect,
            supplied: !sources.is_empty(),
            catalog,
            origins,
        }
    }

    /// Supplied tables are authoritative: a table that is not there does not exist.
    pub fn tables_authoritative(&self) -> bool {
        self.supplied && !self.catalog.tables.is_empty()
    }

    /// Routines of a kind are authoritative only when the catalog describes
    /// some of that kind (DDL catalogs often list procedures but not functions).
    pub fn routines_authoritative(&self, kind: RoutineKind) -> bool {
        self.supplied && self.catalog.routines.iter().any(|r| r.kind == kind)
    }

    pub fn origin(&self) -> String {
        if self.origins.is_empty() {
            "the catalog".to_string()
        } else {
            self.origins.join(", ")
        }
    }

    fn schema_known(&self, schema: &Ident) -> bool {
        let d = self.dialect;
        let k = schema.key(d);
        system_schemas(d)
            .iter()
            .any(|s| key(d, &stored_sys(d, s)) == k)
            || self.catalog.schemas.iter().any(|s| key(d, s) == k)
            || self
                .catalog
                .tables
                .iter()
                .filter_map(|t| t.schema.as_deref())
                .chain(
                    self.catalog
                        .routines
                        .iter()
                        .filter_map(|r| r.schema.as_deref()),
                )
                .any(|s| key(d, s) == k)
    }

    /// Finds a table or view by a qualified name (`[schema.]name`).
    pub fn table(&self, parts: &[Ident]) -> Lookup<&Table> {
        let schema = match parts {
            [_] => None,
            [s, _] => Some(s),
            _ => return Lookup::OutOfScope,
        };
        if let Some(i) = table_position(self.dialect, &self.catalog, parts) {
            return Lookup::Found(&self.catalog.tables[i]);
        }
        match schema {
            Some(s) if !self.schema_known(s) => Lookup::NoSchema(s.text.clone()),
            _ => Lookup::NotFound,
        }
    }

    /// All overloads of a routine.
    pub fn routines(&self, parts: &[Ident]) -> Lookup<Vec<&Routine>> {
        let d = self.dialect;
        let (schema, name) = match parts {
            [n] => (None, n),
            [s, n] => (Some(s), n),
            _ => return Lookup::OutOfScope,
        };
        let k = name.key(d);
        let found: Vec<&Routine> = self
            .catalog
            .routines
            .iter()
            .filter(|r| key(d, &r.name) == k && schema_matches(d, r.schema.as_deref(), schema))
            .collect();
        if !found.is_empty() {
            return Lookup::Found(found);
        }
        match schema {
            Some(s) if !self.schema_known(s) => Lookup::NoSchema(s.text.clone()),
            _ => Lookup::NotFound,
        }
    }
}

fn stored_sys(d: Dialect, s: &str) -> String {
    Ident::plain(s).stored(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(d: Dialect, ddl: &str) -> CatalogView {
        let c = ddl::load(ddl, d, "test");
        CatalogView::new(d, &[&c])
    }

    #[test]
    fn folding_per_dialect() {
        let pg = view(Dialect::Postgres, "CREATE TABLE Stock (Qty int);");
        assert!(matches!(
            pg.table(&[Ident::plain("STOCK")]),
            Lookup::Found(_)
        ));
        assert!(matches!(
            pg.table(&[Ident::new("Stock", true)]),
            Lookup::NotFound
        ));
        assert!(matches!(
            pg.table(&[Ident::plain("public"), Ident::plain("stock")]),
            Lookup::Found(_)
        ));
        assert_eq!(
            pg.table(&[Ident::plain("dbo"), Ident::plain("stock")]),
            Lookup::NoSchema("dbo".into())
        );
        let db2 = view(Dialect::Db2i, "CREATE TABLE stock (qty INTEGER);");
        assert!(matches!(
            db2.table(&[Ident::new("STOCK", true)]),
            Lookup::Found(_)
        ));
        let ts = view(Dialect::Tsql, "CREATE TABLE dbo.Stock (qty int);");
        assert!(matches!(
            ts.table(&[Ident::plain("stock")]),
            Lookup::Found(_)
        ));
        assert!(matches!(
            ts.table(&[Ident::plain("sales"), Ident::plain("stock")]),
            Lookup::NoSchema(_)
        ));
    }
}
