//! Recorded catalog reads (performance plan W4.3, `perf-plan/w4-3-recorded-
//! catalog-reads-design.md`): the vocabulary of what a check may ask of a
//! catalog, the digest of an answer, and the sink that collects the
//! questions.
//!
//! PR 1 of the series: types only. Nothing in the engine records yet and no
//! key reads them; they exist so the inventory (`docs/catalog-read-sites.md`)
//! can name the query each read site becomes, and so the digest is tested
//! field by field before anything depends on it.
//!
//! A [`Query`] is a function of the merged catalog of one side and nothing
//! else, posed as the caller wrote it (a name part keeps its text and its
//! quoting, because replay must pose the very same question). An answer is
//! summarised by an [`Outcome`], the SHA-256 of the *whole* answer object's
//! `Debug` text, never of a summary of it: `Debug` is derived for every
//! catalog type, so a new field is digested without anyone remembering to,
//! and `tests/record.rs` fails the build when a field is added without a test
//! that it changes the digest.

use std::collections::BTreeMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// Which catalog a read went to. The same catalog can be read through
/// different views; replay must ask the same side again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    /// The catalog the input (source dialect) binds against.
    Source,
    /// The catalog the converted output (target dialect) is compared with.
    Target,
    /// The target catalog read for the output's own check.
    OwnTarget,
}

/// One part of a name as written: its text and whether it was quoted.
pub type Part = (String, bool);

/// A question to a catalog. Closed on purpose: a read that is not one of
/// these is not a recorded read. `Ord` is the order of a read set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Query {
    /// Whether any catalog was supplied for the side (`CatalogView::supplied`).
    Supplied,
    /// The origins of the supplied catalogs (they reach reasons).
    Origin,
    /// Supplied and holding some table (`tables_authoritative`).
    TablesAuthoritative,
    /// Supplied and holding some routine of the kind, `Debug` of the kind
    /// (`routines_authoritative`).
    RoutinesAuthoritative(String),
    /// `CatalogView::table`: found (the whole table), not found, no such
    /// schema (with its text), out of scope.
    Table(Vec<Part>),
    /// `CatalogView::routines`: every overload, in catalog order.
    Routines(Vec<Part>),
    /// Whether a schema is known: system, created, or named by an object.
    SchemaKnown(Part),
    /// `Schema` tables by lower-case unqualified name: the derived table the
    /// last base or temporary table of that name in catalog order yields
    /// (last wins), plus the sticky `ambiguous` flag (two schemas held the
    /// name). The answer is the equivalence schema's view, not `CatalogView`'s.
    SchemaTables(String),
    /// Every table (as the equivalence schema has them) with a column of
    /// this lower-case name, with that column's type (`typed_class`).
    ColumnNamed(String),
    /// Every derived table of this one side that has a HIDDEN column, in
    /// full (all columns), sorted by name. One side only: the counterpart on
    /// the other side is a separate `SchemaTables` read (`hides_columns`,
    /// `mirrored`).
    HiddenTables,
    /// The computed family map of a view typed from its source: only views
    /// with `kind == View && types_from_source && !star_outputs` count, and
    /// the map is empty when two such views carry the name.
    ViewFamily(String),
    /// Whether a lower-case view name is a view whose `*` is read through
    /// (`Schema::star_views`, with `list_stars`: `visible_star`).
    StarView(String),
    /// The PostgreSQL spellings of a lower-case name over schemas, routines,
    /// tables, columns, keys and indexes.
    Spellings(String),
    /// Triggers by schema and name (`triggers_named`).
    TriggersNamed(Option<String>, String),
    /// Triggers on a table, as stored parts.
    TriggersOn(Vec<String>),
    /// Triggers that run a function, as stored parts.
    TriggersRunning(Vec<String>),
    /// The sound fallback for a read that has no narrow form: the whole
    /// catalog. A file that records it re-checks on any edit, as today.
    WholeCatalog,
}

/// The digest of an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Outcome(pub [u8; 32]);

impl Outcome {
    /// The digest of an answer's `Debug` text. Every catalog type derives
    /// `Debug`, so every field is in it.
    pub fn of<T: std::fmt::Debug + ?Sized>(answer: &T) -> Outcome {
        Outcome(Sha256::digest(format!("{answer:?}").as_bytes()).into())
    }

    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Where reads go. A check holds a sink; the default drops them.
pub trait ReadSink: Sync {
    fn record(&self, side: Side, query: &Query, outcome: Outcome);

    /// Whether anything is kept. A view digests an answer only when it is:
    /// the digest of a table is the `Debug` text of it, which no unrecorded
    /// run pays for.
    fn active(&self) -> bool {
        true
    }
}

/// Records nothing (every run until the series' PR 3).
pub struct NoSink;

impl ReadSink for NoSink {
    fn record(&self, _: Side, _: &Query, _: Outcome) {}

    fn active(&self) -> bool {
        false
    }
}

/// The reads of one check: sorted by side and query, each once.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reads(pub BTreeMap<(Side, Query), Outcome>);

impl Reads {
    /// A canonical text of the read set, for hashing and for an entry id.
    pub fn canonical(&self) -> String {
        format!("{:?}", self.0)
    }
}

/// Collects the reads of one check. Asking the same question twice and
/// getting two answers means the catalog changed under the check, which a
/// check never allows: it is kept (the later answer) and flagged.
#[derive(Default)]
pub struct Recorder {
    reads: Mutex<Reads>,
    conflicts: Mutex<Vec<(Side, Query)>>,
}

impl Recorder {
    pub fn finish(self) -> (Reads, Vec<(Side, Query)>) {
        (
            self.reads.into_inner().unwrap(),
            self.conflicts.into_inner().unwrap(),
        )
    }
}

impl ReadSink for Recorder {
    fn record(&self, side: Side, query: &Query, outcome: Outcome) {
        let mut r = self.reads.lock().unwrap();
        if let Some(old) = r.0.insert((side, query.clone()), outcome) {
            if old != outcome {
                self.conflicts.lock().unwrap().push((side, query.clone()));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The doors a check is allowed to use (PR 2a).
// ---------------------------------------------------------------------------

use crate::{Catalog, CatalogView, Dialect, Ident, Lookup, Routine, RoutineKind, Table};

/// A sink that drops everything, for a view not under a recorded check.
pub static NO_SINK: NoSink = NoSink;

fn parts_of(parts: &[Ident]) -> Vec<Part> {
    parts.iter().map(|p| (p.text.clone(), p.quoted)).collect()
}

/// The external catalog of one side as a check sees it: every question goes
/// through the sink with the digest of its answer. It holds the
/// [`CatalogView`] by reference and has no accessor for it, so the only reads
/// a check can make are the methods below (and [`whole`](Self::whole), which
/// records the sound fallback `WholeCatalog`).
#[derive(Clone, Copy)]
pub struct RecordingView<'a> {
    view: &'a CatalogView,
    side: Side,
    sink: &'a dyn ReadSink,
}

impl<'a> RecordingView<'a> {
    /// Only the engine, where a side is built, opens a view (see the scan in
    /// `sqldiff-core/tests/catalog_sites.rs`).
    pub fn new(view: &'a CatalogView, side: Side, sink: &'a dyn ReadSink) -> Self {
        RecordingView { view, side, sink }
    }

    fn rec<T: std::fmt::Debug + ?Sized>(&self, q: impl FnOnce() -> Query, answer: &T) {
        if self.sink.active() {
            self.sink.record(self.side, &q(), Outcome::of(answer));
        }
    }

    pub fn dialect(&self) -> Dialect {
        self.view.dialect
    }

    pub fn supplied(&self) -> bool {
        let a = self.view.supplied();
        self.rec(|| Query::Supplied, &a);
        a
    }

    pub fn origin(&self) -> String {
        let a = self.view.origin();
        self.rec(|| Query::Origin, &a);
        a
    }

    pub fn tables_authoritative(&self) -> bool {
        let a = self.view.tables_authoritative();
        self.rec(|| Query::TablesAuthoritative, &a);
        a
    }

    pub fn routines_authoritative(&self, kind: RoutineKind) -> bool {
        let a = self.view.routines_authoritative(kind);
        self.rec(|| Query::RoutinesAuthoritative(format!("{kind:?}")), &a);
        a
    }

    pub fn table(&self, parts: &[Ident]) -> Lookup<&'a Table> {
        let a = self.view.table(parts);
        self.rec(|| Query::Table(parts_of(parts)), &a);
        a
    }

    pub fn routines(&self, parts: &[Ident]) -> Lookup<Vec<&'a Routine>> {
        let a = self.view.routines(parts);
        self.rec(|| Query::Routines(parts_of(parts)), &a);
        a
    }

    /// The whole catalog, for a read that has no narrow query yet. Recorded
    /// as `WholeCatalog` with the digest of everything, so a file that calls
    /// it is re-checked on any edit of this side's catalog: sound, and the
    /// price of not having a narrow form. Each call site is in the inventory.
    pub fn whole(&self) -> &'a Catalog {
        let c = self.view.unrecorded_catalog();
        self.rec(|| Query::WholeCatalog, c);
        c
    }
}

/// The source catalog as pass 1 (`sqldiff-draft`) reads it. Pass 1 holds this
/// and never a `Catalog`, so every read of the tree's catalog goes through
/// one of these methods (the compiler enforces it; the door scan in
/// `tests/catalog_sites.rs` counts the calls). The sandbox of Phase A: reads
/// go to a sink that keeps nothing today, and become `SourceReads` of a
/// stored draft only if Phase B is built.
#[derive(Clone, Copy)]
pub struct SourceReads<'a> {
    cat: &'a Catalog,
    dialect: Dialect,
    sink: &'a dyn ReadSink,
}

impl<'a> SourceReads<'a> {
    /// Wraps the catalog a draft is made against (the driver's one call).
    pub fn new(cat: &'a Catalog, dialect: Dialect) -> Self {
        SourceReads {
            cat,
            dialect,
            sink: &NO_SINK,
        }
    }

    fn rec<T: std::fmt::Debug + ?Sized>(&self, q: impl FnOnce() -> Query, answer: &T) {
        if self.sink.active() {
            self.sink.record(Side::Source, &q(), Outcome::of(answer));
        }
    }

    /// A table by name.
    pub fn table(&self, parts: &[Ident]) -> Option<&'a Table> {
        let a = crate::table_position(self.dialect, self.cat, parts).map(|i| &self.cat.tables[i]);
        self.rec(|| Query::Table(parts_of(parts)), &a);
        a
    }

    /// The triggers a name matches, as [`Catalog::triggers_named`].
    pub fn triggers_named(&self, schema: Option<&str>, name: &str) -> Vec<&'a crate::Trigger> {
        let a = self.cat.triggers_named(self.dialect, schema, name);
        self.rec(
            || Query::TriggersNamed(schema.map(str::to_string), name.to_string()),
            &a,
        );
        a
    }

    /// Every table: records `WholeCatalog`.
    pub fn tables(&self) -> &'a [Table] {
        self.rec(|| Query::WholeCatalog, self.cat);
        &self.cat.tables
    }

    /// Every routine: records `WholeCatalog`.
    pub fn routines(&self) -> &'a [Routine] {
        self.rec(|| Query::WholeCatalog, self.cat);
        &self.cat.routines
    }
}
