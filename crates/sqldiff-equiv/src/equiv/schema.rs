//! The few catalog facts statement equivalence needs: column types,
//! nullability, identity columns and unique keys, read from CREATE TABLE and
//! CREATE UNIQUE INDEX statements in DDL artifacts.
//!
//! Built from the shared catalog (`crate::catalog`) that binding uses.

use std::collections::HashMap;

use sqldiff_syntax::{parse, Dialect};

use super::ast::Type;
use super::literal::Target;
use crate::artifact::Artifact;
use crate::catalog::record::{Query, ReadSink, RecordingView, Side as ReadSide, NO_SINK};
use crate::catalog::{ddl, Catalog, CatalogView, DataType, TableKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fam {
    Int,
    Exact,
    /// T-SQL money: an exact decimal(19, 4) that rounds a product and a
    /// quotient to four decimals, prints two on CAST to a string and raises
    /// an error outside +/-922,337,203,685,477.5808. Only the forms checked
    /// on SQL Server 2022 against PostgreSQL numeric(19, 4) are portable
    /// (eval/builtins/2026-11-02-money); every other use stays per dialect.
    Money,
    Float,
    /// Fixed-length character (blank-padded).
    Char,
    /// Variable-length character; `len` None means unbounded.
    VarChar,
    Date,
    Timestamp,
    TimestampTz,
    Time,
    /// PostgreSQL interval.
    Interval,
    Bool,
    /// Variable-length binary string (T-SQL varbinary, PostgreSQL bytea,
    /// Db2 for i VARBINARY and BLOB); `len` None means unbounded (MAX).
    /// Fixed-length BINARY(n) stays `Other`: T-SQL pads it with zeros.
    Binary,
    /// T-SQL uniqueidentifier, PostgreSQL uuid: a 16-byte identifier. Only
    /// a uuid kept as it is (assigned, stored or cast to uuid) is known to
    /// agree; its text (upper case in T-SQL, lower case in PostgreSQL), its
    /// order and every other conversion are left to the dialects.
    Uuid,
    /// T-SQL geography, PostGIS geography without a type modifier. Only a
    /// value kept as it is (assigned, stored or cast to geography) is known
    /// to agree; SQL Server cannot compare or order one, PostGIS compares
    /// bounding boxes.
    Geography,
    /// T-SQL text and ntext: strings SQL Server cannot compare or sort
    /// (only IS NULL and LIKE work on them, checked on SQL Server 2022).
    Text,
    /// Values one side cannot compare or sort at all: T-SQL image and xml,
    /// PostgreSQL xml. Only a value kept as it is is known to agree.
    NoCompare,
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
        matches!(self.fam, Fam::Char | Fam::VarChar | Fam::Text)
    }

    /// Whether comparing two values of this type (=, IN, GROUP BY, DISTINCT,
    /// set operations) is the dialect's own: SQL Server cannot compare
    /// geography, text, ntext, image or xml (Msg 305, 306, 402, 421, 5335,
    /// 8117), PostGIS compares geography boxes, PostgreSQL has no xml
    /// equality.
    pub fn compares_per_dialect(&self) -> bool {
        matches!(self.fam, Fam::Geography | Fam::Text | Fam::NoCompare)
    }

    /// Whether each dialect orders values of this type its own way: strings
    /// by collation, uuids by byte order (SQL Server compares the last
    /// byte group first), geography not at all in SQL Server.
    pub fn orders_per_dialect(&self) -> bool {
        matches!(
            self.fam,
            Fam::Char | Fam::VarChar | Fam::Uuid | Fam::Geography | Fam::Text | Fam::NoCompare
        )
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
    /// A time type with `p` fractional digits, or the dialect's default:
    /// 7 in T-SQL, 6 in PostgreSQL and in Db2 for i (whose TIME has none).
    fn time_ty(fam: Fam, p: Option<u32>, d: Dialect) -> Ty {
        let default = match (d, fam) {
            (Dialect::Tsql, _) => 7,
            (Dialect::Db2i, Fam::Time) => 0,
            _ => 6,
        };
        Ty {
            fam,
            len: Some(p.unwrap_or(default)),
            scale: None,
        }
    }

    pub fn from_type(t: &Type, d: Dialect, in_cast: bool) -> Ty {
        let n = |i: usize| t.args.get(i).and_then(|a| length(a));
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
            // T-SQL tinyint: unsigned, 0 to 255.
            "TINYINT" if d == Dialect::Tsql => Ty {
                fam: Fam::Int,
                len: Some(8),
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
            "MONEY" if d == Dialect::Tsql => Ty {
                fam: Fam::Money,
                len: Some(19),
                scale: Some(4),
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
            // Floating point: `len` is the significand in bits, 24 (single)
            // or 53 (double). FLOAT(n) is single up to 24 in every dialect.
            "REAL" | "FLOAT4" => Ty {
                fam: Fam::Float,
                len: Some(24),
                scale: None,
            },
            "FLOAT" if n(0).is_some_and(|p| p <= 24) => Ty {
                fam: Fam::Float,
                len: Some(24),
                scale: None,
            },
            "FLOAT" | "DOUBLE PRECISION" | "DOUBLE" | "FLOAT8" => Ty {
                fam: Fam::Float,
                len: Some(53),
                scale: None,
            },
            "VARCHAR" | "NVARCHAR" | "CHARACTER VARYING" | "CHAR VARYING" | "VARCHAR2" => {
                fam_len(Fam::VarChar, tsql_default)
            }
            "TEXT" | "NTEXT" if d == Dialect::Tsql => Ty::of(Fam::Text),
            "IMAGE" | "XML" if d == Dialect::Tsql => Ty::of(Fam::NoCompare),
            "XML" if d == Dialect::Postgres => Ty::of(Fam::NoCompare),
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
            // Times: `len` is the number of fractional second digits. T-SQL
            // datetime counts 1/300 s and smalldatetime whole minutes; `scale`
            // marks those ticks (300, 60), so they equal no other type.
            "DATETIME" if d == Dialect::Tsql => Ty {
                fam: Fam::Timestamp,
                len: Some(3),
                scale: Some(300),
            },
            "SMALLDATETIME" if d == Dialect::Tsql => Ty {
                fam: Fam::Timestamp,
                len: Some(0),
                scale: Some(60),
            },
            // T-SQL timestamp is rowversion, an 8-byte binary(8), not a time.
            "TIMESTAMP" if d == Dialect::Tsql => Ty::of(Fam::Other),
            "DATETIME2" | "TIMESTAMP" | "TIMESTAMP WITHOUT TIME ZONE" => {
                Self::time_ty(Fam::Timestamp, n(0), d)
            }
            "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" | "DATETIMEOFFSET" => {
                Self::time_ty(Fam::TimestampTz, n(0), d)
            }
            "TIME" => Self::time_ty(Fam::Time, n(0), d),
            "INTERVAL" if d == Dialect::Postgres => Ty::of(Fam::Interval),
            "BOOLEAN" | "BOOL" => Ty::of(Fam::Bool),
            "UNIQUEIDENTIFIER" if d == Dialect::Tsql => Ty::of(Fam::Uuid),
            "UUID" if d == Dialect::Postgres => Ty::of(Fam::Uuid),
            "GEOGRAPHY" if d != Dialect::Db2i && t.args.is_empty() => Ty::of(Fam::Geography),
            "VARBINARY" | "BINARY VARYING" => fam_len(Fam::Binary, tsql_default),
            "BYTEA" if d == Dialect::Postgres => Ty::of(Fam::Binary),
            "BLOB" | "BINARY LARGE OBJECT" if d == Dialect::Db2i => Ty {
                fam: Fam::Binary,
                len: if max { None } else { n(0) },
                scale: None,
            },
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
    /// Computed by the database (see `catalog::Column::generated`).
    pub generated: bool,
    /// A period column of a T-SQL system-versioned table (`GENERATED ALWAYS
    /// AS ROW START|END`).
    pub period: bool,
    /// `HIDDEN` (T-SQL): `SELECT *` and `t.*` leave the column out.
    pub hidden: bool,
    /// What a string literal compared with this column converts to.
    pub target: Option<Target>,
    /// A collation the column states.
    pub collation: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    /// Lower case; `None` for no schema or the default one (dbo, public).
    pub schema: Option<String>,
    pub name: String,
    pub columns: Vec<Column>,
    /// Primary key and unique constraints or indexes, column names lower-cased.
    pub keys: Vec<Vec<String>>,
    /// The catalog read every column (`catalog::Table::columns_known`), so
    /// `columns` is the table's full column list in order.
    pub complete: bool,
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
    tables: std::sync::Arc<HashMap<String, Table>>,
    sources: Vec<String>,
    /// PostgreSQL: the system catalogs are known too.
    pg_system: bool,
    /// T-SQL: the `sys` catalog views are known too.
    tsql_system: bool,
    /// The whole catalog the schema was read from, when one was supplied:
    /// routines, schemas, indexes and constraint names for calls and comments.
    view: Option<std::sync::Arc<CatalogView>>,
    /// Unqualified names of user tables in more than one schema.
    ambiguous: std::sync::Arc<std::collections::HashSet<String>>,
    /// Lower-cased unqualified names of views defined with a `*` select
    /// list (`catalog::Table::star_outputs`): which columns of their sources
    /// they return is not modelled.
    star_views: std::sync::Arc<std::collections::HashSet<String>>,
    /// Views that read one table or view directly
    /// (`catalog::Table::types_from_source`): lower-cased view name, then
    /// lower-cased column name, to the column's type family when known.
    /// Used only to tell that a column cannot be `char(n)`; nothing else
    /// about a view column (nullability, values) is relied on.
    view_families: std::sync::Arc<HashMap<String, HashMap<String, Fam>>>,
    /// PostgreSQL: the stored spellings of every catalog name (schemas,
    /// tables and views, columns, keys, indexes, routines), by lower case.
    /// Two spellings under one key are names that differ only in letter case.
    spellings: std::sync::Arc<HashMap<String, std::collections::BTreeSet<String>>>,
    /// A `*` is compared as the columns it returns, listed from this
    /// schema's tables (hidden ones left out), not as one term: set on both
    /// sides of a comparison when either has a table with HIDDEN columns
    /// (`Schema::list_stars_with`).
    list_stars: bool,
    /// The external answers as built, never edited afterwards: the base maps
    /// above are copied on write by `with_images` / `with_local` (and
    /// `flatten`), so what a read asks of the external catalog is answered
    /// from here and recorded as such (performance plan W4.3, PR 2b).
    ext: Ext,
    /// The role and the sink of the check this schema serves; `None` for a
    /// schema outside a recorded check (nothing is recorded).
    probe: Option<Probe>,
    /// `list_stars_with` found every table with HIDDEN columns mirrored on
    /// both sides: the external star views are then plain views. The file's
    /// own star views (layers) stay star views.
    star_cleared: bool,
    /// Star views of layers flattened into the base by `with_images` /
    /// `with_local`; never cleared.
    flat_star_views: std::sync::Arc<std::collections::HashSet<String>>,
    /// Tables added on top of `tables` by `with_ddl` (they replace tables of
    /// the same name). Adding one table must not copy every table of the
    /// catalog, so the added names live in these small layers; readers go
    /// through `table_by_key`, `tables_by_key`, `is_ambiguous`,
    /// `is_star_view` and `spellings_of`.
    extra_tables: std::sync::Arc<HashMap<String, Table>>,
    extra_ambiguous: std::sync::Arc<std::collections::HashSet<String>>,
    extra_star_views: std::sync::Arc<std::collections::HashSet<String>>,
    extra_spellings: std::sync::Arc<HashMap<String, std::collections::BTreeSet<String>>>,
}

/// What the external catalog says, fixed at build.
#[derive(Debug, Clone, Default)]
struct Ext {
    tables: std::sync::Arc<HashMap<String, Table>>,
    ambiguous: std::sync::Arc<std::collections::HashSet<String>>,
    star_views: std::sync::Arc<std::collections::HashSet<String>>,
    view_families: std::sync::Arc<HashMap<String, HashMap<String, Fam>>>,
    spellings: std::sync::Arc<HashMap<String, std::collections::BTreeSet<String>>>,
    /// PostgreSQL: spellings are asked.
    pg: bool,
}

/// The role and the sink of the check a schema serves.
#[derive(Clone)]
struct Probe {
    side: ReadSide,
    sink: std::sync::Arc<dyn ReadSink + Send>,
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Probe({:?})", self.side)
    }
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
    /// The same schema serving one check: its reads are filed under `side`
    /// (the role of this catalog in the check, which only the caller knows)
    /// and go to `sink`. The cached schema of a side carries neither, so it
    /// cannot file a read under the wrong role.
    pub fn for_check(&self, side: ReadSide, sink: std::sync::Arc<dyn ReadSink + Send>) -> Schema {
        let mut s = self.clone();
        s.probe = Some(Probe { side, sink });
        s
    }

    fn rec<T: std::fmt::Debug + ?Sized>(&self, q: impl FnOnce() -> Query, answer: &T) {
        if let Some(p) = &self.probe {
            if p.sink.active() {
                p.sink
                    .record(p.side, &q(), crate::catalog::record::Outcome::of(answer));
            }
        }
    }

    /// Asks the external catalog for the user table under `key` (lower-cased
    /// unqualified name): the table as built and whether the name is in
    /// more than one schema. Asked on every read of a name, whatever a layer
    /// says, because `ambiguous` is computed across both.
    fn ask_table(&self, key: &str) {
        self.rec(
            || Query::SchemaTables(key.to_string()),
            &(self.ext.tables.get(key), self.ext.ambiguous.contains(key)),
        );
    }

    /// The catalog of this side, as a check may read it: every question
    /// goes through a [`RecordingView`] under this check's role and sink.
    pub fn catalog(&self) -> Option<RecordingView<'_>> {
        self.view.as_deref().map(|v| match &self.probe {
            Some(p) => RecordingView::new(v, p.side, &*p.sink),
            None => RecordingView::new(v, ReadSide::Source, &NO_SINK),
        })
    }

    /// The origins of the catalogs the schema was read from.
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Whether the check lists stars (set by `list_stars_with` from the
    /// recorded reads of both sides).
    pub fn lists_stars(&self) -> bool {
        self.list_stars
    }

    /// A user table by its lower-cased unqualified name, the layer added by
    /// `with_ddl` first.
    fn table_by_key(&self, key: &str) -> Option<&Table> {
        self.extra_tables.get(key).or_else(|| self.tables.get(key))
    }

    /// Every user table with its key, once each (the added layer wins).
    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: its consumers need all tables to agree
    fn tables_by_key(&self) -> impl Iterator<Item = (&String, &Table)> {
        self.extra_tables.iter().chain(
            self.tables
                .iter()
                .filter(|(k, _)| !self.extra_tables.contains_key(*k)),
        )
    }

    fn is_ambiguous(&self, key: &str) -> bool {
        self.ambiguous.contains(key) || self.extra_ambiguous.contains(key)
    }

    /// Whether the view `key` was defined with a `*` select list.
    pub fn is_star_view(&self, key: &str) -> bool {
        let ext = self.ext.star_views.contains(key);
        self.rec(|| Query::StarView(key.to_string()), &ext);
        (ext && !self.star_cleared)
            || self.flat_star_views.contains(key)
            || self.extra_star_views.contains(key)
    }

    /// Copies the added layer into the base maps, for the edits that change
    /// the base (`with_images`, `with_local`).
    #[allow(clippy::disallowed_methods, clippy::iter_over_hash_type)] // hash order cannot reach an output: set union / membership; each name gets its own entry
    fn flatten(&mut self) {
        if self.extra_tables.is_empty()
            && self.extra_ambiguous.is_empty()
            && self.extra_star_views.is_empty()
            && self.extra_spellings.is_empty()
        {
            return;
        }
        let (t, a, v, sp) = (
            std::mem::take(&mut self.extra_tables),
            std::mem::take(&mut self.extra_ambiguous),
            std::mem::take(&mut self.extra_star_views),
            std::mem::take(&mut self.extra_spellings),
        );
        std::sync::Arc::make_mut(&mut self.tables)
            .extend(t.iter().map(|(k, v)| (k.clone(), v.clone())));
        std::sync::Arc::make_mut(&mut self.ambiguous).extend(a.iter().cloned());
        std::sync::Arc::make_mut(&mut self.flat_star_views).extend(v.iter().cloned());
        let spellings = std::sync::Arc::make_mut(&mut self.spellings);
        for (k, v) in sp.iter() {
            spellings
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
    }

    /// The types of the columns named `col` (lower case) in the catalog's
    /// tables (system catalogs left out), one per table. The caller needs
    /// them to agree, so their order does not matter.
    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: the consumer needs all types to agree
    pub fn column_types(&self, col: &str) -> Vec<Ty> {
        self.rec(
            || Query::ColumnNamed(col.to_string()),
            &self.sorted_ext(|t| t.column(col).is_some()),
        );
        self.tables_by_key()
            .filter_map(|(_, t)| t.column(col))
            .map(|c| c.ty)
            .collect()
    }

    /// The external tables that satisfy `keep`, by name: the canonical form
    /// of an answer about many tables.
    #[allow(clippy::disallowed_methods)] // sorted before it leaves
    fn sorted_ext(&self, keep: impl Fn(&Table) -> bool) -> Vec<(&String, &Table)> {
        let mut v: Vec<(&String, &Table)> =
            self.ext.tables.iter().filter(|(_, t)| keep(t)).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
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
        self.ask_table(last);
        self.table_by_key(last).or_else(|| {
            // pg_catalog comes first on every search path.
            (self.pg_system && last.starts_with("pg_"))
                .then(|| pg_system().tables.get(last))
                .flatten()
        })
    }

    /// Whether column `col` of the view `name` is known not to be a
    /// character type: the view reads one table directly and its definition
    /// gives the column a known type of another family.
    pub fn view_column_is_not_char(&self, name: &str, col: &str) -> bool {
        let key = name.to_lowercase();
        let key = key.rsplit('.').next().unwrap_or(&key);
        #[allow(clippy::disallowed_methods)] // collected into a BTreeMap: ordered
        let family = self
            .ext
            .view_families
            .get(key)
            .map(|m| m.iter().collect::<std::collections::BTreeMap<_, _>>());
        self.rec(|| Query::ViewFamily(key.to_string()), &family);
        self.view_families
            .get(key)
            .and_then(|c| c.get(&col.to_lowercase()))
            .is_some_and(|f| *f != Fam::Char)
    }

    /// A user table by schema (`None` for the default one) and name, when
    /// no other schema has a table of that name.
    pub fn table_exact(&self, schema: Option<&str>, name: &str) -> Option<&Table> {
        let name = name.to_lowercase();
        self.ask_table(&name);
        if self.is_ambiguous(&name) {
            return None;
        }
        let t = self.table_by_key(&name)?;
        (t.schema.as_deref() == schema.map(str::to_lowercase).as_deref()).then_some(t)
    }

    /// This schema with `images` (a trigger's `inserted` and `deleted`)
    /// as copies of `table`: the same columns and keys. A key holds in an
    /// image because its rows are rows of the table after (`inserted`) or
    /// before (`deleted`) the statement. `None` when the table is unknown.
    pub fn with_images(&self, table: &[String], images: &[&str]) -> Option<Schema> {
        let last = table.last()?;
        let schema = match table {
            [.., s, _] => Some(s.to_lowercase()).filter(|s| s != "dbo" && s != "public"),
            _ => None,
        };
        let t = match schema {
            Some(s) => self.table_exact(Some(&s), last)?,
            None => self.table(last)?,
        }
        .clone();
        let mut out = self.clone();
        out.flatten();
        for &name in images {
            let mut copy = t.clone();
            copy.schema = None;
            copy.name = name.to_string();
            std::sync::Arc::make_mut(&mut out.ambiguous).remove(name);
            std::sync::Arc::make_mut(&mut out.tables).insert(name.to_string(), copy);
        }
        Some(out)
    }

    /// This schema with the tables `ddl` creates added; they replace tables
    /// of the same name.
    #[allow(clippy::disallowed_methods, clippy::iter_over_hash_type)] // hash order cannot reach an output: set union / membership; set union; each name gets its own entry
    pub fn with_ddl(&self, ddl: &str, d: Dialect) -> Schema {
        let mut cat = Catalog::default();
        ddl::load_tree(&mut cat, &parse(ddl, d).syntax(), d);
        let extra = Schema::from_view(&CatalogView::new(d, &[&cat]));
        let mut s = self.clone();
        let mut ambiguous = (*s.extra_ambiguous).clone();
        for n in extra.tables.keys() {
            self.ask_table(n);
            if self
                .table_by_key(n)
                .is_some_and(|t| t.schema != extra.tables[n].schema)
            {
                ambiguous.insert(n.clone());
            }
        }
        ambiguous.extend(extra.ambiguous.iter().cloned());
        let star_views = std::sync::Arc::make_mut(&mut s.extra_star_views);
        star_views.extend(extra.star_views.iter().cloned());
        std::sync::Arc::make_mut(&mut s.extra_tables)
            .extend(extra.tables.iter().map(|(k, v)| (k.clone(), v.clone())));
        s.extra_ambiguous = std::sync::Arc::new(ambiguous);
        let spellings = std::sync::Arc::make_mut(&mut s.extra_spellings);
        for (k, v) in extra.spellings.iter() {
            spellings
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        s
    }

    /// This schema with the one table `ddl` creates added under the key
    /// `key` gives its schema and name (a statement's table key, such as
    /// `temp:t`): a table the unit creates for its own use (a table
    /// variable, a temporary table), which no catalog holds. `None` when
    /// `ddl` is not one table with known columns.
    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: single-table catalog
    pub fn with_local(
        &self,
        ddl: &str,
        d: Dialect,
        key: impl FnOnce(Option<&str>, &str) -> String,
    ) -> Option<Schema> {
        let mut cat = Catalog::default();
        ddl::load_tree(&mut cat, &parse(ddl, d).syntax(), d);
        let [t] = cat.tables.as_slice() else {
            return None;
        };
        if !t.columns_known || t.columns.is_empty() {
            return None;
        }
        let key = key(t.schema.as_deref(), &t.name).to_lowercase();
        let extra = Schema::from_view(&CatalogView::new(d, &[&cat]));
        let table = extra.tables.values().next()?.clone();
        let mut s = self.clone();
        s.flatten();
        std::sync::Arc::make_mut(&mut s.ambiguous).remove(&key);
        std::sync::Arc::make_mut(&mut s.tables).insert(key, table);
        Some(s)
    }

    /// PostgreSQL: whether a name, spelled as PostgreSQL stores it (a quoted
    /// name as written, others in lower case), may be compared with T-SQL
    /// names up to letter case (`ASM-IDENT-FOLD`). Not when the catalog holds
    /// two names that differ only in letter case (a T-SQL name cannot say
    /// which it means), nor when it holds the name only in another case
    /// (PostgreSQL resolves `stored` to none of them). `known`: the name must
    /// be a catalog name.
    pub fn case_check(&self, stored: &str, known: bool) -> Result<(), String> {
        let lower = stored.to_lowercase();
        if self.ext.pg {
            self.rec(
                || Query::Spellings(lower.clone()),
                &self.ext.spellings.get(&lower),
            );
        }
        let merged;
        let found = match (self.spellings.get(&lower), self.extra_spellings.get(&lower)) {
            (Some(a), Some(b)) => {
                merged = a
                    .union(b)
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>();
                Some(&merged)
            }
            (a, b) => a.or(b),
        };
        match found {
            None if known && stored != lower => Err(format!(
                "the quoted name \"{stored}\" is not in the catalog, so how PostgreSQL resolves it is not known"
            )),
            None => Ok(()),
            Some(v) if v.len() > 1 => {
                let v: Vec<&str> = v.iter().map(String::as_str).collect();
                Err(format!(
                    "the PostgreSQL catalog has the names \"{}\", which differ only in letter case, so a T-SQL name does not say which one it means",
                    v.join("\", \"")
                ))
            }
            Some(v) if !v.contains(stored) => Err(format!(
                "PostgreSQL resolves {} to no catalog object: the catalog spells it \"{}\"",
                if stored == lower {
                    stored.to_string()
                } else {
                    format!("\"{stored}\"")
                },
                v.iter().next().map_or("", String::as_str)
            )),
            Some(_) => Ok(()),
        }
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

    /// Whether a table has a HIDDEN column (T-SQL period column).
    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: `any`
    pub fn hides_columns(&self) -> bool {
        self.rec(
            || Query::HiddenTables,
            &self.sorted_ext(|t| t.columns.iter().any(|c| c.hidden)),
        );
        self.tables_by_key()
            .any(|(_, t)| t.columns.iter().any(|c| c.hidden))
    }

    /// Both sides of a comparison list their stars when either side has a
    /// table with HIDDEN columns: T-SQL's `*` leaves them out and
    /// PostgreSQL's returns every column, so the two stars are only the same
    /// when the columns they return are.
    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: `all`
    pub fn list_stars_with(&mut self, other: &mut Schema) {
        // Both sides are asked, never `||`: a short circuit would leave the
        // other side's `HiddenTables` read unrecorded although the mirrored
        // decision below uses it.
        let (hides_a, hides_b) = (self.hides_columns(), other.hides_columns());
        if hides_a || hides_b {
            self.list_stars = true;
            other.list_stars = true;
            // A view defined with `*` returns what its sources do, so it is
            // only the same on both sides when every table with HIDDEN
            // columns has a counterpart on the other side that keeps exactly
            // its visible columns (the historyless conversion). Otherwise a
            // star over such a view is refused (`Canon::visible_star`).
            // Read from the external answers of both sides only (never the
            // layers), each read recorded: the hidden tables of `a` are in
            // its `HiddenTables` answer, their counterparts are
            // `SchemaTables` reads of `b`.
            let mirrored = |a: &Schema, b: &Schema| {
                a.sorted_ext(|t| t.columns.iter().any(|c| c.hidden))
                    .into_iter()
                    .all(|(n, t)| {
                        b.ask_table(n);
                        b.ext.tables.get(n).is_some_and(|o| {
                            o.complete
                                && o.columns.iter().map(|c| &c.name).eq(t
                                    .columns
                                    .iter()
                                    .filter(|c| !c.hidden)
                                    .map(|c| &c.name))
                        })
                    })
            };
            if mirrored(self, other) && mirrored(other, self) {
                self.star_cleared = true;
                other.star_cleared = true;
            }
        }
    }

    /// The facts equivalence needs from one side's catalog (DDL, bundles and
    /// adapter catalogs, as the binder sees them).
    pub fn from_view(view: &CatalogView) -> Schema {
        Self::build(
            view,
            view.supplied().then(|| std::sync::Arc::new(view.clone())),
        )
    }

    /// As `from_view`, keeping the shared view instead of copying it.
    pub fn from_shared_view(view: &std::sync::Arc<CatalogView>) -> Schema {
        Self::build(view, view.supplied().then(|| std::sync::Arc::clone(view)))
    }

    fn build(view: &CatalogView, shared: Option<std::sync::Arc<CatalogView>>) -> Schema {
        let d = view.dialect;
        let mut s = Schema {
            sources: view.origins().to_vec(),
            pg_system: d == Dialect::Postgres,
            tsql_system: d == Dialect::Tsql,
            view: shared,
            ..Schema::default()
        };
        if d == Dialect::Postgres {
            let c = &view.unrecorded_catalog();
            let names = c
                .schemas
                .iter()
                .chain(
                    c.routines
                        .iter()
                        .flat_map(|r| r.schema.iter().chain([&r.name])),
                )
                .chain(c.tables.iter().flat_map(|t| {
                    t.schema
                        .iter()
                        .chain([&t.name])
                        .chain(t.columns.iter().map(|c| &c.name))
                        .chain(t.keys.iter().filter_map(|k| k.name.as_ref()))
                        .chain(t.indexes.iter().map(|i| &i.name))
                }));
            for n in names {
                std::sync::Arc::make_mut(&mut s.spellings)
                    .entry(n.to_lowercase())
                    .or_default()
                    .insert(n.clone());
            }
        }
        std::sync::Arc::make_mut(&mut s.star_views).extend(
            view.unrecorded_catalog()
                .tables
                .iter()
                .filter(|t| t.kind == TableKind::View && t.star_outputs)
                .map(|t| t.name.to_lowercase()),
        );
        for t in view
            .unrecorded_catalog()
            .tables
            .iter()
            .filter(|t| t.kind == TableKind::View && t.types_from_source && !t.star_outputs)
        {
            let name = t.name.to_lowercase();
            // The types were copied when the view was created: they count
            // only while the one base table of that name still has each
            // source column with that very type (an ALTER COLUMN, or a
            // table dropped and created again, leaves the copy stale).
            let sources: Vec<&crate::catalog::Table> = view
                .unrecorded_catalog()
                .tables
                .iter()
                .filter(|b| {
                    b.kind != TableKind::View && b.name.eq_ignore_ascii_case(&t.source_table)
                })
                .collect();
            let current = |col: usize| -> bool {
                let [base] = sources.as_slice() else {
                    return false;
                };
                t.source_columns
                    .get(col)
                    .and_then(|x| x.as_ref())
                    .and_then(|sc| {
                        base.columns
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(sc))
                    })
                    .is_some_and(|c| c.data_type == t.columns[col].data_type)
            };
            let cols: HashMap<String, Fam> = t
                .columns
                .iter()
                .enumerate()
                .filter(|(i, c)| c.data_type.is_known() && current(*i))
                .map(|(_, c)| (c.name.to_lowercase(), ty_of(&c.data_type, d).fam))
                .collect();
            // Two views of one name in different schemas: not trusted.
            match std::sync::Arc::make_mut(&mut s.view_families).entry(name) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.insert(HashMap::new());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(cols);
                }
            }
        }
        // Base and temporary tables only: view columns are derived, and their
        // types and nullability are not facts equivalence may rely on.
        for t in view
            .unrecorded_catalog()
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
                    generated: c.generated,
                    hidden: c.hidden,
                    period: t.versioning.as_ref().is_some_and(|v| {
                        c.name.eq_ignore_ascii_case(&v.start) || c.name.eq_ignore_ascii_case(&v.end)
                    }),
                    target: Target::of(&c.data_type.base, &c.data_type.args, d),
                    collation: c.collation.clone(),
                })
                .collect();
            let name = t.name.to_lowercase();
            let schema = t
                .schema
                .as_ref()
                .map(|x| x.to_lowercase())
                .filter(|x| x != "dbo" && x != "public");
            if s.tables.get(&name).is_some_and(|o| o.schema != schema) {
                std::sync::Arc::make_mut(&mut s.ambiguous).insert(name.clone());
            }
            std::sync::Arc::make_mut(&mut s.tables).insert(
                name.clone(),
                Table {
                    schema,
                    name,
                    columns,
                    keys,
                    complete: t.columns_known,
                },
            );
        }
        s.ext = Ext {
            tables: s.tables.clone(),
            ambiguous: s.ambiguous.clone(),
            star_views: s.star_views.clone(),
            view_families: s.view_families.clone(),
            spellings: s.spellings.clone(),
            pg: d == Dialect::Postgres,
        };
        s
    }
}

pub(super) fn ty_of(t: &DataType, d: Dialect) -> Ty {
    if !t.is_known() {
        return Ty::of(Fam::Other);
    }
    let mut args: Vec<String> = t.args.iter().map(u32::to_string).collect();
    if t.max {
        args.insert(0, "MAX".to_string());
    }
    // `[sys].[geography]`: a T-SQL system type named with its schema.
    let qualified = t.base.replace(['[', ']'], "").replace(" . ", ".");
    let name = match qualified.strip_prefix("SYS.") {
        Some(base) if d == Dialect::Tsql => base.to_string(),
        _ => t.base.clone(),
    };
    Ty::from_type(&Type { name, args }, d, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded digest of an answer is the `Debug` text of derived
    /// `Table`, `Column` and `Ty` values: every field must be in it, and a new
    /// field breaks this build (each type is destructured without `..`) until
    /// it has a line here that changes the text.
    fn digest<T: std::fmt::Debug>(v: &T) -> crate::catalog::record::Outcome {
        crate::catalog::record::Outcome::of(v)
    }

    #[test]
    fn every_field_of_the_derived_types_is_digested() {
        let ty = Ty {
            fam: Fam::Int,
            len: Some(4),
            scale: None,
        };
        let Ty {
            fam: _,
            len: _,
            scale: _,
        } = ty;
        for (name, m) in [
            (
                "fam",
                Ty {
                    fam: Fam::Char,
                    ..ty
                },
            ),
            ("len", Ty { len: Some(8), ..ty }),
            (
                "scale",
                Ty {
                    scale: Some(2),
                    ..ty
                },
            ),
        ] {
            assert_ne!(digest(&ty), digest(&m), "Ty.{name}");
        }
        let col = Column {
            name: "a".into(),
            ty,
            not_null: false,
            identity: false,
            generated: false,
            period: false,
            hidden: false,
            target: None,
            collation: None,
        };
        let Column {
            name: _,
            ty: _,
            not_null: _,
            identity: _,
            generated: _,
            period: _,
            hidden: _,
            target: _,
            collation: _,
        } = &col;
        #[allow(clippy::type_complexity)]
        let muts: Vec<(&str, Box<dyn Fn(&mut Column)>)> = vec![
            ("name", Box::new(|c| c.name = "b".into())),
            ("ty", Box::new(|c| c.ty.len = Some(9))),
            ("not_null", Box::new(|c| c.not_null = true)),
            ("identity", Box::new(|c| c.identity = true)),
            ("generated", Box::new(|c| c.generated = true)),
            ("period", Box::new(|c| c.period = true)),
            ("hidden", Box::new(|c| c.hidden = true)),
            ("target", Box::new(|c| c.target = Some(Target::Bit))),
            ("collation", Box::new(|c| c.collation = Some("x".into()))),
        ];
        for (name, m) in muts {
            let mut c = col.clone();
            m(&mut c);
            assert_ne!(digest(&col), digest(&c), "Column.{name}");
        }
        let tab = Table {
            schema: None,
            name: "t".into(),
            columns: vec![col],
            keys: vec![],
            complete: false,
        };
        let Table {
            schema: _,
            name: _,
            columns: _,
            keys: _,
            complete: _,
        } = &tab;
        #[allow(clippy::type_complexity)]
        let muts: Vec<(&str, Box<dyn Fn(&mut Table)>)> = vec![
            ("schema", Box::new(|t| t.schema = Some("s".into()))),
            ("name", Box::new(|t| t.name = "u".into())),
            ("columns", Box::new(|t| t.columns.clear())),
            ("keys", Box::new(|t| t.keys.push(vec!["a".into()]))),
            ("complete", Box::new(|t| t.complete = true)),
        ];
        for (name, m) in muts {
            let mut t = tab.clone();
            m(&mut t);
            assert_ne!(digest(&tab), digest(&t), "Table.{name}");
        }
    }

    /// A naive model of the layer algebra of the performance plan's 2b note
    /// (R3): tables replace by key; `with_ddl` sets `ambiguous` when the
    /// table currently held under the name has another schema; `with_local`
    /// and `with_images` clear it for the keys they replace.
    #[derive(Clone, Default)]
    struct Model {
        tables: std::collections::BTreeMap<String, (Option<String>, Vec<String>)>,
        ambiguous: std::collections::BTreeSet<String>,
    }

    fn norm(schema: &str) -> Option<String> {
        let s = schema.to_lowercase();
        (s != "dbo" && s != "public").then_some(s)
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    const SCHEMAS: [&str; 4] = ["dbo", "s1", "S2", "public"];
    const NAMES: [&str; 5] = ["a", "b", "C", "A", "d"];
    const COLS: [&str; 3] = ["c1", "c2", "c3"];

    #[allow(clippy::type_complexity)]
    fn ddl_of(r: &mut Rng, n: usize) -> (String, Vec<(String, Option<String>, Vec<String>)>) {
        let mut text = String::new();
        let mut made = Vec::new();
        for _ in 0..n {
            let (sc, nm) = (SCHEMAS[r.next(4)], NAMES[r.next(5)]);
            let cols: Vec<&str> = COLS.iter().copied().filter(|_| r.next(2) == 0).collect();
            let list: String = std::iter::once("id INT NOT NULL".to_string())
                .chain(cols.iter().map(|c| format!("{c} INT NULL")))
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("CREATE TABLE {sc}.{nm} ({list});\n"));
            let mut names = vec!["id".to_string()];
            names.extend(cols.iter().map(|c| c.to_string()));
            made.push((nm.to_lowercase(), norm(sc), names));
        }
        (text, made)
    }

    fn model_layer(m: &mut Model, made: &[(String, Option<String>, Vec<String>)]) {
        // The layer folds on its own first.
        let mut layer: std::collections::BTreeMap<String, (Option<String>, Vec<String>)> =
            Default::default();
        let mut layer_amb = std::collections::BTreeSet::new();
        for (k, sc, cols) in made {
            if layer.get(k).is_some_and(|o| &o.0 != sc) {
                layer_amb.insert(k.clone());
            }
            layer.insert(k.clone(), (sc.clone(), cols.clone()));
        }
        for (k, v) in &layer {
            if m.tables.get(k).is_some_and(|o| o.0 != v.0) {
                m.ambiguous.insert(k.clone());
            }
            m.tables.insert(k.clone(), v.clone());
        }
        m.ambiguous.extend(layer_amb);
    }

    fn base_schema(r: &mut Rng) -> (Schema, Model) {
        let (text, made) = ddl_of(r, 4);
        let mut cat = Catalog::default();
        ddl::load_tree(
            &mut cat,
            &parse(&text, Dialect::Tsql).syntax(),
            Dialect::Tsql,
        );
        let mut m = Model::default();
        model_layer(&mut m, &made);
        (
            Schema::from_view(&CatalogView::new(Dialect::Tsql, &[&cat])),
            m,
        )
    }

    fn agree(s: &Schema, m: &Model, what: &str) {
        for nm in NAMES.iter().chain(&["inserted", "deleted", "temp:a", "zz"]) {
            let k = nm.to_lowercase();
            let want = m.tables.get(&k);
            let got = s.table(nm).map(|t| {
                (
                    t.schema.clone(),
                    t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                )
            });
            assert_eq!(got.as_ref(), want, "{what}: table({nm})");
            for sc in [None, Some("s1"), Some("s2")] {
                let want = want
                    .filter(|_| !m.ambiguous.contains(&k))
                    .filter(|t| t.0.as_deref() == sc)
                    .map(|t| &t.1);
                let got = s
                    .table_exact(sc, nm)
                    .map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
                assert_eq!(got.as_ref(), want, "{what}: table_exact({sc:?}, {nm})");
            }
        }
        for c in COLS {
            let want = m
                .tables
                .values()
                .filter(|t| t.1.iter().any(|x| x == c))
                .count();
            assert_eq!(s.column_types(c).len(), want, "{what}: column_types({c})");
        }
    }

    #[test]
    fn layers_stack_like_the_model_in_any_order() {
        for seed in 1..=60u64 {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (mut s, mut m) = base_schema(&mut r);
            agree(&s, &m, &format!("seed {seed} base"));
            for step in 0..6 {
                match r.next(3) {
                    0 => {
                        let n = 1 + r.next(2);
                        let (text, made) = ddl_of(&mut r, n);
                        s = s.with_ddl(&text, Dialect::Tsql);
                        model_layer(&mut m, &made);
                    }
                    1 => {
                        let (sc, nm) = (SCHEMAS[r.next(4)], NAMES[r.next(5)]);
                        let text =
                            format!("CREATE TABLE {sc}.{nm} (id INT NOT NULL, c1 INT NULL);");
                        if let Some(n) =
                            s.with_local(&text, Dialect::Tsql, |_, n| format!("temp:{n}"))
                        {
                            s = n;
                            let k = format!("temp:{}", nm.to_lowercase());
                            m.tables
                                .insert(k.clone(), (norm(sc), vec!["id".into(), "c1".into()]));
                            m.ambiguous.remove(&k);
                        }
                    }
                    _ => {
                        let nm = NAMES[r.next(5)];
                        if let Some(n) = s.with_images(&[nm.to_string()], &["inserted", "deleted"])
                        {
                            let src = m.tables[&nm.to_lowercase()].clone();
                            s = n;
                            for img in ["inserted", "deleted"] {
                                m.tables.insert(img.into(), (None, src.1.clone()));
                                m.ambiguous.remove(img);
                            }
                        }
                    }
                }
                agree(&s, &m, &format!("seed {seed} step {step}"));
            }
        }
    }

    /// `with_ddl` adds tables in a layer; every reader must see them as if
    /// they had been in the catalog all along, and the base stays unchanged.
    #[test]
    fn with_ddl_layer_reads_like_a_merged_schema() {
        let d = Dialect::Postgres;
        let base = Schema::load(&[Artifact::new(
            "a.sql",
            d,
            "CREATE TABLE t1 (id int, \"Mixed\" int); CREATE TABLE s.t2 (x int);",
        )]);
        let more = base.with_ddl("CREATE TABLE t3 (y int); CREATE TABLE t1 (z int);", d);
        // The layer wins for a table of the same name; the base keeps its own.
        assert_eq!(more.table("t1").unwrap().columns.len(), 1);
        assert_eq!(base.table("t1").unwrap().columns.len(), 2);
        assert!(more.table("t3").is_some() && base.table("t3").is_none());
        assert!(more.table("t2").is_some());
        // Each table once.
        assert_eq!(more.tables_by_key().count(), 3);
        assert_eq!(base.tables_by_key().count(), 2);
        // A layered schema is layered again.
        let again = more.with_ddl("CREATE TABLE t4 (w int);", d);
        assert_eq!(again.tables_by_key().count(), 4);
        assert!(again.table("t3").is_some() && again.table("t4").is_some());
        // Names of the layer take part in the letter-case check.
        assert!(base.case_check("mixed", false).is_err());
        assert!(more.case_check("z", true).is_ok());
    }

    #[test]
    fn tsql_timestamp_is_rowversion_not_a_time() {
        let t = Type {
            name: "TIMESTAMP".into(),
            args: vec![],
        };
        assert_eq!(Ty::from_type(&t, Dialect::Tsql, false).fam, Fam::Other);
        assert_eq!(
            Ty::from_type(&t, Dialect::Postgres, false).fam,
            Fam::Timestamp
        );
        assert_eq!(Ty::from_type(&t, Dialect::Db2i, false).fam, Fam::Timestamp);
    }

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

/// A type length argument: a number, or a Db2 for i LOB length with its
/// multiplier (`64K`, `1M`, `1G`: 1024, 1024², 1024³).
fn length(a: &str) -> Option<u32> {
    let (num, mul) = match a.as_bytes().last()? {
        b'K' => (&a[..a.len() - 1], 1u32 << 10),
        b'M' => (&a[..a.len() - 1], 1 << 20),
        b'G' => (&a[..a.len() - 1], 1 << 30),
        _ => (a, 1),
    };
    num.parse::<u32>().ok()?.checked_mul(mul)
}

#[cfg(test)]
mod length_tests {
    use super::length;

    #[test]
    fn lob_lengths_take_their_multiplier() {
        assert_eq!(length("10"), Some(10));
        assert_eq!(length("64K"), Some(65_536));
        assert_eq!(length("1M"), Some(1_048_576));
        assert_eq!(length("1G"), Some(1_073_741_824));
        assert_eq!(length("2G"), Some(2_147_483_648));
        assert_eq!(length("MAX"), None);
        assert_eq!(length("G"), None);
        assert_eq!(length("8G"), None);
    }
}
