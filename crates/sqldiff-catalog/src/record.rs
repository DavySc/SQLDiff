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
    /// `Schema` tables by lower-case unqualified name: every base or
    /// temporary table of that name in catalog order, and whether two
    /// schemas share it (the equivalence schema's view, not `CatalogView`'s).
    SchemaTables(String),
    /// Every table (as the equivalence schema has them) with a column of
    /// this lower-case name, with that column's type (`typed_class`).
    ColumnNamed(String),
    /// Every table that has a HIDDEN column, with its counterpart on the
    /// other side (`hides_columns`, `mirrored`).
    HiddenTables,
    /// The family a view typed from its source belongs to: the view and
    /// every non-view table its `source_table` names, with their columns.
    ViewFamily(String),
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
}

/// Records nothing (every run until the series' PR 3).
pub struct NoSink;

impl ReadSink for NoSink {
    fn record(&self, _: Side, _: &Query, _: Outcome) {}
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
