//! Catalog facts for binding (design §3 layer 2): tables, columns, keys,
//! indexes and routine signatures, looked up with per-dialect identifier
//! folding.
//!
//! Facts come from DDL (`ddl::load`, e.g. `corpus/catalog/*.sql`), from a
//! source adapter's `get_catalog` or a snapshot bundle (converted by
//! `sqldiff-source`), and from objects an artifact creates itself (temporary
//! tables, table variables, routines defined in the same file).

// Determinism guard (perf plan W0): iterating a hash map or set in a `for` loop is
// as order-dependent as `.iter()`; each site needs a reasoned `#[allow]`.
#![warn(clippy::iter_over_hash_type)]

pub mod builtins;
pub mod ddl;
pub mod record;
pub mod tokens;
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
    /// T-SQL `GENERATED ALWAYS AS ROW START|END HIDDEN`: `SELECT *`, `t.*`
    /// and an INSERT without a column list leave the column out; naming it
    /// reads it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
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
    /// A view whose select list the catalog could not list (it has a `*`):
    /// its column names may be known (an explicit list) while which columns
    /// of its sources they stand for is not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub star_outputs: bool,
    /// A view whose column types are those of a single base table or view
    /// read directly (no join, set operation, CTE or derived table), so
    /// that a column listed with a known type has that type. Only such
    /// types may be relied on; any other view's column types are a guess.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub types_from_source: bool,
    /// With `types_from_source`: the stored name of the base table read, and
    /// per column the source column its type was copied from (`None` for an
    /// expression or an unresolved one). Lets a consumer check the types
    /// against the table as it is *now*: a later ALTER COLUMN or a dropped
    /// and re-created table leaves the view's copy stale.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_table: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_columns: Vec<Option<String>>,
    #[serde(default)]
    pub keys: Vec<Key>,
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub triggers: Vec<String>,
    /// T-SQL system versioning declared by the CREATE TABLE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning: Option<Versioning>,
}

/// `PERIOD FOR SYSTEM_TIME (start, end)` with `SYSTEM_VERSIONING = ON
/// (HISTORY_TABLE = history)`, as a T-SQL CREATE TABLE declares them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioning {
    /// The period's start and end columns, as stored.
    pub start: String,
    pub end: String,
    /// The history table's name parts, as stored; `None` when the CREATE
    /// TABLE names none (SQL Server makes one up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<String>>,
}

impl Table {
    pub fn new(schema: Option<String>, name: impl Into<String>, kind: TableKind) -> Self {
        Table {
            schema,
            name: name.into(),
            kind,
            columns: Vec::new(),
            columns_known: true,
            star_outputs: false,
            types_from_source: false,
            source_table: String::new(),
            source_columns: Vec::new(),
            keys: Vec::new(),
            indexes: Vec::new(),
            triggers: Vec::new(),
            versioning: None,
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
    /// A T-SQL procedure whose body has a `RETURN expression`: its status is
    /// not always 0, so the conversion gives it a last `INOUT return_code`.
    /// Only read from T-SQL DDL.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub returns_status: bool,
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
    /// DML triggers, each definition as read (two definitions of one name
    /// are both kept, so a lookup can tell the name is not unique).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
}

/// A DML trigger: T-SQL `CREATE TRIGGER name ON table {FOR|AFTER|INSTEAD
/// OF} events`, PostgreSQL `CREATE TRIGGER name … events ON table …
/// EXECUTE FUNCTION f()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// The schema part of the trigger's name as written, if any. A T-SQL
    /// trigger lives in its table's schema (SQL Server refuses another,
    /// Msg 2103); a PostgreSQL trigger has no schema of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    /// The table's name parts, as stored.
    pub table: Vec<String>,
    /// `INSERT`, `UPDATE`, `DELETE` (and PostgreSQL `TRUNCATE`), in the
    /// order written.
    pub events: Vec<String>,
    /// `INSTEAD OF` (`BEFORE` and `AFTER` are not told apart).
    #[serde(default)]
    pub instead: bool,
    /// PostgreSQL: the function the trigger executes, name parts as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<Vec<String>>,
}

impl Trigger {
    /// The schema the trigger is found in by name: its own, or its table's.
    pub fn home(&self) -> Option<&str> {
        self.schema.as_deref().or(match self.table.as_slice() {
            [.., s, _] => Some(s.as_str()),
            _ => None,
        })
    }
}

/// Positions of a catalog's tables and routines by name key, so a name is
/// found without scanning (and without a `String` per element) and a bulk
/// load is linear. Valid only while the catalog changes through this index
/// (`put_table`, `put_routine`) or in ways that keep every name; the list
/// order is the catalog's own, the index never reorders anything.
#[derive(Debug, Clone, Default)]
pub struct NameIndex {
    tables: std::collections::BTreeMap<String, Vec<usize>>,
    routines: std::collections::BTreeMap<String, Vec<usize>>,
}

impl NameIndex {
    /// The index of `cat` as it is now.
    pub fn new(dialect: Dialect, cat: &Catalog) -> NameIndex {
        let mut ix = NameIndex::default();
        for (i, t) in cat.tables.iter().enumerate() {
            ix.tables.entry(key(dialect, &t.name)).or_default().push(i);
        }
        for (i, r) in cat.routines.iter().enumerate() {
            ix.routines
                .entry(key(dialect, &r.name))
                .or_default()
                .push(i);
        }
        ix
    }

    /// [`Catalog::put_table`], through the index.
    pub fn put_table(&mut self, dialect: Dialect, cat: &mut Catalog, t: Table) {
        let k = table_identity(dialect, &t);
        let same: Vec<usize> = self
            .tables
            .get(&k.1)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| table_identity(dialect, &cat.tables[i]) == k)
            .collect();
        if same.is_empty() {
            self.tables.entry(k.1).or_default().push(cat.tables.len());
            cat.tables.push(t);
            return;
        }
        // A replaced table moves to the end, as `Catalog::put_table` does.
        remove_at(&same, &mut cat.tables);
        shift_after_removal(&mut self.tables, &same);
        self.tables.entry(k.1).or_default().push(cat.tables.len());
        cat.tables.push(t);
    }

    /// [`Catalog::put_routine`], through the index.
    pub fn put_routine(&mut self, dialect: Dialect, cat: &mut Catalog, r: Routine) {
        let k = routine_identity(dialect, &r);
        let same: Vec<usize> = self
            .routines
            .get(&k.1)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| routine_identity(dialect, &cat.routines[i]) == k)
            .collect();
        if same.is_empty() {
            self.routines
                .entry(k.1)
                .or_default()
                .push(cat.routines.len());
            cat.routines.push(r);
            return;
        }
        remove_at(&same, &mut cat.routines);
        shift_after_removal(&mut self.routines, &same);
        self.routines
            .entry(k.1)
            .or_default()
            .push(cat.routines.len());
        cat.routines.push(r);
    }

    /// [`table_position`] without the scan.
    pub fn table_position(&self, d: Dialect, cat: &Catalog, parts: &[Ident]) -> Option<usize> {
        let (schema, name) = match parts {
            [n] => (None, n),
            [s, n] => (Some(s), n),
            _ => return None,
        };
        self.tables
            .get(&name.key(d))?
            .iter()
            .copied()
            .find(|&i| schema_matches(d, cat.tables[i].schema.as_deref(), schema))
    }

    /// Positions of the routines named `name` (a key), in list order.
    fn routines_named(&self, name: &str) -> &[usize] {
        self.routines.get(name).map_or(&[], Vec::as_slice)
    }
}

/// Removes the elements at `gone` (ascending positions) from `v`.
fn remove_at<T>(gone: &[usize], v: &mut Vec<T>) {
    let mut at = 0;
    let mut next = gone.iter().peekable();
    v.retain(|_| {
        let keep = next.peek() != Some(&&at);
        if !keep {
            next.next();
        }
        at += 1;
        keep
    });
}

/// Brings positions up to date after the elements at `gone` (ascending) were
/// removed from the list: a position moves down by the removed ones before it.
fn shift_after_removal(map: &mut std::collections::BTreeMap<String, Vec<usize>>, gone: &[usize]) {
    for list in map.values_mut() {
        list.retain(|i| gone.binary_search(i).is_err());
        for i in list.iter_mut() {
            *i -= gone.partition_point(|&g| g < *i);
        }
    }
    map.retain(|_, l| !l.is_empty());
}

/// What makes two tables the same one: the schema and name keys.
fn table_identity(dialect: Dialect, t: &Table) -> (Option<String>, String) {
    (
        t.schema.as_deref().map(|s| key(dialect, s)),
        key(dialect, &t.name),
    )
}

/// What makes two routines the same one: schema and name keys, parameter
/// count, and in PostgreSQL the parameter types too (its routines are
/// identified by them; lengths, precision and scale are not part of it).
fn routine_identity(dialect: Dialect, r: &Routine) -> (Option<String>, String, usize, Vec<String>) {
    let types = if dialect == Dialect::Postgres {
        r.call_params(dialect)
            .iter()
            .map(|p| p.data_type.base.clone())
            .collect()
    } else {
        Vec::new()
    };
    (
        r.schema.as_deref().map(|s| key(dialect, s)),
        key(dialect, &r.name),
        r.params.len(),
        types,
    )
}

impl Catalog {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
            && self.routines.is_empty()
            && self.schemas.is_empty()
            && self.triggers.is_empty()
    }

    /// Adds a trigger definition, unless the same one is there already.
    pub fn put_trigger(&mut self, t: Trigger) {
        if !self.triggers.contains(&t) {
            self.triggers.push(t);
        }
    }

    /// The definitions of the trigger that `schema.name` names in
    /// `dialect`: T-SQL finds a trigger in its table's schema, and an
    /// unqualified name in the default schema (dbo). Every definition read
    /// for that name, so the caller can tell whether they agree.
    pub fn triggers_named(
        &self,
        dialect: Dialect,
        schema: Option<&str>,
        name: &str,
    ) -> Vec<&Trigger> {
        let def = default_schema(dialect).unwrap_or_default();
        let want = key(dialect, schema.unwrap_or(def));
        self.triggers
            .iter()
            .filter(|t| {
                key(dialect, &t.name) == key(dialect, name)
                    && key(dialect, t.home().unwrap_or(def)) == want
            })
            .collect()
    }

    /// Adds or replaces a table (same schema and name).
    pub fn put_table(&mut self, dialect: Dialect, t: Table) {
        let k = table_identity(dialect, &t);
        self.tables.retain(|x| table_identity(dialect, x) != k);
        self.tables.push(t);
    }

    /// Adds or replaces a routine. Overloads are kept apart by parameter
    /// count, and in PostgreSQL by the parameter types too.
    pub fn put_routine(&mut self, dialect: Dialect, r: Routine) {
        let k = routine_identity(dialect, &r);
        self.routines.retain(|x| routine_identity(dialect, x) != k);
        self.routines.push(r);
    }

    pub fn extend(&mut self, dialect: Dialect, other: &Catalog) {
        let mut ix = NameIndex::new(dialect, self);
        for t in &other.tables {
            ix.put_table(dialect, self, t.clone());
        }
        for r in &other.routines {
            ix.put_routine(dialect, self, r.clone());
        }
        for t in &other.triggers {
            self.put_trigger(t.clone());
        }
        let mut known: std::collections::HashSet<String> =
            self.schemas.iter().map(|x| key(dialect, x)).collect();
        for s in &other.schemas {
            if known.insert(key(dialect, s)) {
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

/// A file's own objects as a binder builds them (temporary tables, routines
/// created so far). Not an input: it is built from the file's text, so reading
/// it is not a catalog read. A distinct type from [`CatalogView`] so that a
/// scan can keep `CatalogView` out of the check crates, where only a
/// [`record::RecordingView`] reaches an external catalog.
#[derive(Debug, Clone)]
pub struct LocalView(CatalogView);

impl LocalView {
    pub fn new(dialect: Dialect) -> LocalView {
        LocalView(CatalogView::scratch(dialect))
    }

    pub fn put_table(&mut self, t: Table) {
        self.0.put_table(t);
    }

    pub fn put_routine(&mut self, r: Routine) {
        self.0.put_routine(r);
    }

    pub fn apply_statement(&mut self, t: &[crate::tokens::Tok]) {
        self.0.apply_statement(t);
    }

    pub fn table(&self, parts: &[Ident]) -> Lookup<&Table> {
        self.0.table(parts)
    }

    pub fn routines(&self, parts: &[Ident]) -> Lookup<Vec<&Routine>> {
        self.0.routines(parts)
    }
}

/// The merged catalog one side binds against.
#[derive(Debug, Clone)]
pub struct CatalogView {
    pub dialect: Dialect,
    catalog: Catalog,
    /// Whether any catalog was supplied for this side. Without one, persistent
    /// objects stay unresolved and become assumptions.
    supplied: bool,
    /// Origins of the supplied catalogs, for witnesses.
    origins: Vec<String>,
    /// Name positions in `catalog` as built, and the sizes they were built
    /// for: a catalog edited afterwards is searched by scanning instead.
    index: NameIndex,
    indexed: (usize, usize),
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
        let index = NameIndex::new(dialect, &catalog);
        let indexed = (catalog.tables.len(), catalog.routines.len());
        CatalogView {
            dialect,
            supplied: !sources.is_empty(),
            catalog,
            origins,
            index,
            indexed,
        }
    }

    /// The merged catalog, unrecorded. Read-only on purpose: the name index
    /// is built for it, so every change goes through `put_table`,
    /// `put_routine` or `apply_statement`, which keep the index current.
    /// A check reads through a [`record::RecordingView`] instead; this is
    /// for the engine's own side construction (`Schema::build`, until PR 2b
    /// makes that lazy) and for [`record::RecordingView::whole`].
    pub fn unrecorded_catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// An empty view that objects are added to as they are created (a
    /// binder's temporary tables and routines): looked up through the same
    /// index, which each addition keeps current.
    pub fn scratch(dialect: Dialect) -> CatalogView {
        CatalogView::new(dialect, &[&Catalog::default()])
    }

    fn reindexed(&mut self) {
        self.indexed = (self.catalog.tables.len(), self.catalog.routines.len());
    }

    /// Adds or replaces a table, as [`Catalog::put_table`].
    pub fn put_table(&mut self, t: Table) {
        if self.is_indexed() {
            self.index.put_table(self.dialect, &mut self.catalog, t);
        } else {
            self.catalog.put_table(self.dialect, t);
            self.index = NameIndex::new(self.dialect, &self.catalog);
        }
        self.reindexed();
    }

    /// Adds or replaces a routine, as [`Catalog::put_routine`].
    pub fn put_routine(&mut self, r: Routine) {
        if self.is_indexed() {
            self.index.put_routine(self.dialect, &mut self.catalog, r);
        } else {
            self.catalog.put_routine(self.dialect, r);
            self.index = NameIndex::new(self.dialect, &self.catalog);
        }
        self.reindexed();
    }

    /// Applies one DDL statement ([`ddl::apply_statement`]) to the view.
    pub fn apply_statement(&mut self, t: &[crate::tokens::Tok]) {
        if !self.is_indexed() {
            self.index = NameIndex::new(self.dialect, &self.catalog);
        }
        ddl::apply_indexed(&mut self.catalog, &mut self.index, t, self.dialect);
        self.reindexed();
    }

    fn is_indexed(&self) -> bool {
        self.indexed == (self.catalog.tables.len(), self.catalog.routines.len())
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

    /// Whether any catalog was supplied for this side.
    pub fn supplied(&self) -> bool {
        self.supplied
    }

    /// The origins of the supplied catalogs, in the order they were merged.
    pub fn origins(&self) -> &[String] {
        &self.origins
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
        let found = if self.indexed.0 == self.catalog.tables.len() {
            self.index
                .table_position(self.dialect, &self.catalog, parts)
        } else {
            table_position(self.dialect, &self.catalog, parts)
        };
        if let Some(i) = found {
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
        let found: Vec<&Routine> = if self.indexed.1 == self.catalog.routines.len() {
            self.index
                .routines_named(&k)
                .iter()
                .map(|&i| &self.catalog.routines[i])
                .filter(|r| schema_matches(d, r.schema.as_deref(), schema))
                .collect()
        } else {
            self.catalog
                .routines
                .iter()
                .filter(|r| key(d, &r.name) == k && schema_matches(d, r.schema.as_deref(), schema))
                .collect()
        };
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

    /// Putting through the index gives the same catalog, in the same order,
    /// as the plain puts: a replaced table moves to the end, overloads stay.
    #[test]
    fn indexed_puts_equal_plain_puts() {
        for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
            let order = [
                (None, "a"),
                (Some("s"), "b"),
                (None, "A"),
                (None, "c"),
                (Some("s"), "b"),
                (None, "a"),
                (Some("S"), "b"),
                (None, "c"),
            ];
            let table = |n: usize| {
                let (s, name) = order[n];
                let mut t = Table::new(s.map(String::from), name.to_string(), TableKind::Table);
                t.star_outputs = n % 2 == 0; // tells the puts apart
                t
            };
            let (mut plain, mut via) = (Catalog::default(), Catalog::default());
            let mut ix = NameIndex::new(d, &via);
            for n in 0..order.len() {
                plain.put_table(d, table(n));
                ix.put_table(d, &mut via, table(n));
                assert_eq!(
                    format!("{:?}", plain.tables),
                    format!("{:?}", via.tables),
                    "{d:?} {n}"
                );
            }
            let f1 = ddl::load("CREATE FUNCTION f(p int) RETURNS int AS 'x'; CREATE FUNCTION f(p text) RETURNS int AS 'x'; CREATE FUNCTION f(p int) RETURNS int AS 'y'; CREATE FUNCTION f(a int, b int) RETURNS int AS 'z';", d, "t");
            assert!(!f1.routines.is_empty(), "{d:?}: the functions load");
            let (mut plain, mut via) = (Catalog::default(), Catalog::default());
            let mut ix = NameIndex::new(d, &via);
            for r in f1.routines.iter().chain(&f1.routines) {
                plain.put_routine(d, r.clone());
                ix.put_routine(d, &mut via, r.clone());
                assert_eq!(
                    format!("{:?}", plain.routines),
                    format!("{:?}", via.routines),
                    "{d:?}"
                );
            }
            // Lookups through the view agree with a scan.
            let v = CatalogView::new(d, &[&plain]);
            for r in &plain.routines {
                let Lookup::Found(found) = v.routines(&[Ident::new(r.name.clone(), true)]) else {
                    panic!("{d:?} {}: the routine is found", r.name);
                };
                let scan: Vec<&Routine> = plain
                    .routines
                    .iter()
                    .filter(|x| key(d, &x.name) == key(d, &r.name))
                    .collect();
                assert_eq!(found.len(), scan.len(), "{d:?} {}", r.name);
            }
        }
    }

    /// A catalog of thousands of tables loads in linear time: the old
    /// replace-by-scan made this quadratic (and allocated per element).
    #[test]
    fn a_large_catalog_loads_and_looks_up_fast() {
        let n = 20_000;
        let ddl: String = (0..n)
            .map(|i| format!("CREATE TABLE s{}.t{i} (id int);\n", i % 7))
            .collect();
        let c = ddl::load(&ddl, Dialect::Tsql, "big");
        assert_eq!(c.tables.len(), n);
        let v = CatalogView::new(Dialect::Tsql, &[&c]);
        for i in (0..n).step_by(997) {
            let parts = [
                Ident::plain(format!("s{}", i % 7)),
                Ident::plain(format!("t{i}")),
            ];
            assert!(matches!(v.table(&parts), Lookup::Found(_)), "t{i}");
        }
        assert!(matches!(
            v.table(&[Ident::plain("s0"), Ident::plain("nope")]),
            Lookup::NotFound
        ));
    }
}
