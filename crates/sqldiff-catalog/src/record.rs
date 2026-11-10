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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Side {
    /// The catalog the input (source dialect) binds against.
    Source,
    /// The catalog the converted output (target dialect) is compared with.
    Target,
    /// The target catalog read for the output's own check.
    OwnTarget,
    /// The source catalog as pass 1 (`sqldiff-draft`) reads it, through
    /// [`SourceReads`]. Its `Table` answer is `table_position`'s, not
    /// [`CatalogView::table`]'s `Lookup`: same key, different question, so
    /// a pass-1 read can never be replayed as a pass-2 one or the other way
    /// round ([`RecordingView::ask`] and [`SourceReads::ask`] each refuse
    /// the other's side).
    Draft,
}

/// One part of a name as written: its text and whether it was quoted.
pub type Part = (String, bool);

/// A question to a catalog. Closed on purpose: a read that is not one of
/// these is not a recorded read. `Ord` is the order of a read set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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
    /// Pass 1 only (`Side::Draft`): the distinct column types of the whole
    /// catalog as `(base, arguments)`, sorted. What the bit-comparison rules
    /// need to know whether any column could be a `bit`; it changes only when
    /// a type shape appears or disappears, not on every column edit.
    ColumnTypes,
    /// Pass 1 only (`Side::Draft`): every claim of the tree's table
    /// variables on a temporary table name (`Claims`), as one digest. Asked
    /// of the tree's claims, not of a catalog, so it is replayed by the
    /// draft crate. The wide form of the one question pass 1 asks
    /// (`Claims::clash`).
    ClaimsAll,
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

    /// The digest [`hex`](Self::hex) wrote; `None` for any other text.
    pub fn from_hex(text: &str) -> Option<Outcome> {
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(Outcome(out))
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

    /// Whether this sink already holds an answer to the question. A reader
    /// of an immutable catalog (pass 1) then skips the digest: the answer
    /// cannot differ. Defaults to `false`, so the digest is always made.
    fn holds(&self, _: Side, _: &Query) -> bool {
        false
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
    /// The reads so far and the questions that got two answers, leaving the
    /// recorder as it is (a recorder shared through an `Arc` cannot be
    /// consumed).
    pub fn snapshot(&self) -> (Reads, Vec<(Side, Query)>) {
        (
            self.reads.lock().unwrap().clone(),
            self.conflicts.lock().unwrap().clone(),
        )
    }

    pub fn finish(self) -> (Reads, Vec<(Side, Query)>) {
        (
            self.reads.into_inner().unwrap(),
            self.conflicts.into_inner().unwrap(),
        )
    }
}

impl ReadSink for Recorder {
    fn holds(&self, side: Side, query: &Query) -> bool {
        self.reads
            .lock()
            .unwrap()
            .0
            .contains_key(&(side, query.clone()))
    }

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

/// Test switch over the call sites of catalog reads (the `site-switch`
/// feature): a read is attributed to the place in a check crate that asked
/// for it (`#[track_caller]` carries the place through the reading methods
/// to the first caller that is not one), and the switch can list the places
/// seen or skip recording at one of them. Without the feature
/// [`site_skipped`] is `false` and nothing is stored.
#[cfg(feature = "site-switch")]
pub mod site_switch {
    use std::collections::BTreeSet;
    use std::panic::Location;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Mutex;

    /// A place in the sources: file and line.
    pub type Site = (String, u32);

    const OFF: u8 = 0;
    const DISCOVER: u8 = 1;
    const SKIP: u8 = 2;
    static MODE: AtomicU8 = AtomicU8::new(OFF);
    static SEEN: Mutex<BTreeSet<Site>> = Mutex::new(BTreeSet::new());
    static SKIPPED: Mutex<Option<Site>> = Mutex::new(None);

    /// Records every site that reads from now on (`seen` lists them).
    pub fn discover() {
        SEEN.lock().unwrap().clear();
        MODE.store(DISCOVER, Ordering::SeqCst);
    }

    /// Skips recording the reads asked for at `site`.
    pub fn skip(site: Site) {
        *SKIPPED.lock().unwrap() = Some(site);
        MODE.store(SKIP, Ordering::SeqCst);
    }

    /// Back to recording everything, nothing noted.
    pub fn off() {
        MODE.store(OFF, Ordering::SeqCst);
        *SKIPPED.lock().unwrap() = None;
    }

    /// The sites seen since [`discover`], in order.
    pub fn seen() -> Vec<Site> {
        SEEN.lock().unwrap().iter().cloned().collect()
    }

    pub(super) fn skipped(at: &Location<'static>) -> bool {
        match MODE.load(Ordering::Relaxed) {
            OFF => false,
            DISCOVER => {
                SEEN.lock()
                    .unwrap()
                    .insert((at.file().replace('\\', "/"), at.line()));
                false
            }
            _ => SKIPPED
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|(f, l)| *l == at.line() && *f == at.file().replace('\\', "/")),
        }
    }
}

/// Whether recording is switched off for the read being asked for (see
/// [`site_switch`]). Always `false` without the `site-switch` feature.
#[track_caller]
#[inline(always)]
pub fn site_skipped() -> bool {
    #[cfg(feature = "site-switch")]
    {
        site_switch::skipped(std::panic::Location::caller())
    }
    #[cfg(not(feature = "site-switch"))]
    {
        false
    }
}

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

    #[track_caller]
    fn rec<T: std::fmt::Debug + ?Sized>(&self, q: impl FnOnce() -> Query, answer: &T) {
        if self.sink.active() && !site_skipped() {
            self.sink.record(self.side, &q(), Outcome::of(answer));
        }
    }

    pub fn dialect(&self) -> Dialect {
        self.view.dialect
    }

    #[track_caller]
    pub fn supplied(&self) -> bool {
        let a = self.view.supplied();
        self.rec(|| Query::Supplied, &a);
        a
    }

    #[track_caller]
    pub fn origin(&self) -> String {
        let a = self.view.origin();
        self.rec(|| Query::Origin, &a);
        a
    }

    #[track_caller]
    pub fn tables_authoritative(&self) -> bool {
        let a = self.view.tables_authoritative();
        self.rec(|| Query::TablesAuthoritative, &a);
        a
    }

    #[track_caller]
    pub fn routines_authoritative(&self, kind: RoutineKind) -> bool {
        let a = self.view.routines_authoritative(kind);
        self.rec(|| Query::RoutinesAuthoritative(format!("{kind:?}")), &a);
        a
    }

    #[track_caller]
    pub fn table(&self, parts: &[Ident]) -> Lookup<&'a Table> {
        let a = self.view.table(parts);
        self.rec(|| Query::Table(parts_of(parts)), &a);
        a
    }

    #[track_caller]
    pub fn routines(&self, parts: &[Ident]) -> Lookup<Vec<&'a Routine>> {
        let a = self.view.routines(parts);
        self.rec(|| Query::Routines(parts_of(parts)), &a);
        a
    }

    /// Replay: asks the question `q` through the same method a check used,
    /// so the answer is recorded (to this view's sink) by the very code the
    /// check ran. `false` for a question this view cannot be asked: a
    /// replay that meets one rejects the stored result.
    pub fn ask(&self, q: &Query) -> bool {
        if self.side == Side::Draft {
            return false;
        }
        let idents = |parts: &[Part]| -> Vec<Ident> {
            parts
                .iter()
                .map(|(t, q)| Ident::new(t.as_str(), *q))
                .collect()
        };
        match q {
            Query::Supplied => {
                self.supplied();
            }
            Query::Origin => {
                self.origin();
            }
            Query::TablesAuthoritative => {
                self.tables_authoritative();
            }
            Query::RoutinesAuthoritative(kind) => {
                let kind = match kind.as_str() {
                    "Procedure" => RoutineKind::Procedure,
                    "Function" => RoutineKind::Function,
                    "Trigger" => RoutineKind::Trigger,
                    _ => return false,
                };
                self.routines_authoritative(kind);
            }
            Query::Table(parts) => {
                self.table(&idents(parts));
            }
            Query::Routines(parts) => {
                self.routines(&idents(parts));
            }
            Query::WholeCatalog => {
                self.whole();
            }
            _ => return false,
        }
        true
    }

    /// The whole catalog, for a read that has no narrow query yet. Recorded
    /// as `WholeCatalog` with the digest of everything, so a file that calls
    /// it is re-checked on any edit of this side's catalog: sound, and the
    /// price of not having a narrow form. Each call site is in the inventory.
    #[track_caller]
    pub fn whole(&self) -> &'a Catalog {
        let c = self.view.unrecorded_catalog();
        self.rec(|| Query::WholeCatalog, c);
        c
    }
}

/// The digests of one source catalog, made once and shared by every
/// [`SourceReads`] over it (a table never changes once the catalog is built;
/// pass 1 asks about the same table thousands of times). Without it a
/// recording read digests the whole table text on every call, the cost
/// recording had in pass 2 before the table digest was cached.
pub struct SourceDigests {
    /// The catalog these caches were built for (its address): another one is
    /// refused by [`SourceReads::recording`], because a cache paired with a
    /// catalog it was not made for would hand out stale digests.
    owner: usize,
    tables: Vec<std::sync::OnceLock<Outcome>>,
    whole: std::sync::OnceLock<Outcome>,
    types: std::sync::OnceLock<Vec<(String, Vec<u32>)>>,
}

impl SourceDigests {
    /// Empty caches sized for `cat`. Use them with this catalog only.
    pub fn new(cat: &Catalog) -> Self {
        SourceDigests {
            owner: cat as *const Catalog as usize,
            tables: cat.tables.iter().map(|_| Default::default()).collect(),
            whole: Default::default(),
            types: Default::default(),
        }
    }
}

/// The distinct column types of a catalog, sorted: the answer of
/// [`Query::ColumnTypes`].
fn column_types_of(cat: &Catalog) -> Vec<(String, Vec<u32>)> {
    let mut v: Vec<(String, Vec<u32>)> = cat
        .tables
        .iter()
        .flat_map(|t| t.columns.iter())
        .map(|c| (c.data_type.base.clone(), c.data_type.args.clone()))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The source catalog as pass 1 (`sqldiff-draft`) reads it. Pass 1 holds this
/// and never a `Catalog`, so every read of the tree's catalog goes through
/// one of these methods (the compiler enforces it; the door scan in
/// `tests/catalog_sites.rs` counts the calls). Reads go to a sink under
/// [`Side::Draft`]; [`new`](Self::new) gives the sink that keeps nothing (the
/// file's own text and every run that does not record).
#[derive(Clone, Copy)]
pub struct SourceReads<'a> {
    cat: &'a Catalog,
    dialect: Dialect,
    sink: &'a dyn ReadSink,
    digests: Option<&'a SourceDigests>,
}

impl<'a> SourceReads<'a> {
    /// Wraps the catalog a draft is made against, recording nothing. The
    /// local catalog of a file (`type_catalog` without a tree) is always
    /// made this way: its input is the file's text, which is in the key.
    pub fn new(cat: &'a Catalog, dialect: Dialect) -> Self {
        SourceReads {
            cat,
            dialect,
            sink: &NO_SINK,
            digests: None,
        }
    }

    /// The same, recording every read to `sink` as [`Side::Draft`], with the
    /// digests of `digests` (built for `cat`) shared across the readers.
    pub fn recording(
        cat: &'a Catalog,
        dialect: Dialect,
        sink: &'a dyn ReadSink,
        digests: &'a SourceDigests,
    ) -> Self {
        assert!(
            digests.owner == cat as *const Catalog as usize,
            "SourceDigests used with a catalog it was not built for"
        );
        SourceReads {
            cat,
            dialect,
            sink,
            digests: Some(digests),
        }
    }

    /// Records `q` with the digest `d` makes, unless the sink holds it or the
    /// site switch skips this call site (a narrow read under test).
    #[track_caller]
    fn rec_with(&self, q: impl FnOnce() -> Query, d: impl FnOnce() -> Outcome) {
        if site_skipped() {
            return;
        }
        self.rec_always(q, d);
    }

    /// [`rec_with`](Self::rec_with) for a read that is never skipped: the
    /// wide fallback, whose omission no edit is meant to show.
    fn rec_always(&self, q: impl FnOnce() -> Query, d: impl FnOnce() -> Outcome) {
        if !self.sink.active() {
            return;
        }
        let q = q();
        if self.sink.holds(Side::Draft, &q) {
            return;
        }
        self.sink.record(Side::Draft, &q, d());
    }

    #[track_caller]
    fn rec<T: std::fmt::Debug + ?Sized>(&self, q: impl FnOnce() -> Query, answer: &T) {
        self.rec_with(q, || Outcome::of(answer));
    }

    /// The digest of the whole catalog, made once per [`SourceDigests`].
    fn whole_digest(&self) -> Outcome {
        match self.digests {
            Some(d) => *d.whole.get_or_init(|| Outcome::of(self.cat)),
            None => Outcome::of(self.cat),
        }
    }

    /// A table by name. Records the narrow `Table(parts)` query: only a site
    /// whose recording a per-site test proves (T3) may call this; any other
    /// uses [`table_wide`](Self::table_wide).
    #[track_caller]
    pub fn table(&self, parts: &[Ident]) -> Option<&'a Table> {
        let at = crate::table_position(self.dialect, self.cat, parts);
        let a = at.map(|i| &self.cat.tables[i]);
        self.rec_with(
            || Query::Table(parts_of(parts)),
            || {
                // The answer is the table's digest (cached) or none.
                let t = at.map(|i| match self.digests {
                    Some(d) => *d.tables[i].get_or_init(|| Outcome::of(&self.cat.tables[i])),
                    None => Outcome::of(&self.cat.tables[i]),
                });
                Outcome::of(&t)
            },
        );
        a
    }

    /// A table by name, recording `WholeCatalog` instead of the table: the
    /// sound fallback for a site no per-site test proves yet. A draft that
    /// read it is built again on any edit of the catalog.
    pub fn table_wide(&self, parts: &[Ident]) -> Option<&'a Table> {
        let a = crate::table_position(self.dialect, self.cat, parts).map(|i| &self.cat.tables[i]);
        self.rec_always(|| Query::WholeCatalog, || self.whole_digest());
        a
    }

    /// [`triggers_named`](Self::triggers_named), recording `WholeCatalog`.
    pub fn triggers_named_wide(&self, schema: Option<&str>, name: &str) -> Vec<&'a crate::Trigger> {
        let a = self.cat.triggers_named(self.dialect, schema, name);
        self.rec_always(|| Query::WholeCatalog, || self.whole_digest());
        a
    }

    /// The triggers a name matches, as [`Catalog::triggers_named`].
    #[track_caller]
    pub fn triggers_named(&self, schema: Option<&str>, name: &str) -> Vec<&'a crate::Trigger> {
        let a = self.cat.triggers_named(self.dialect, schema, name);
        self.rec(
            || Query::TriggersNamed(schema.map(str::to_string), name.to_string()),
            &a,
        );
        a
    }

    /// The distinct column types of the catalog, sorted (`(base, arguments)`).
    /// Records [`Query::ColumnTypes`], not the whole catalog.
    #[track_caller]
    pub fn column_types(&self) -> std::borrow::Cow<'a, [(String, Vec<u32>)]> {
        let types = match self.digests {
            Some(d) => std::borrow::Cow::Borrowed(
                d.types.get_or_init(|| column_types_of(self.cat)).as_slice(),
            ),
            None => std::borrow::Cow::Owned(column_types_of(self.cat)),
        };
        self.rec(|| Query::ColumnTypes, &*types);
        types
    }

    /// [`column_types`](Self::column_types), recording `WholeCatalog` instead
    /// of `ColumnTypes`: the sound fallback until a per-site test proves the
    /// narrow query.
    pub fn column_types_wide(&self) -> std::borrow::Cow<'a, [(String, Vec<u32>)]> {
        let types = match self.digests {
            Some(d) => std::borrow::Cow::Borrowed(
                d.types.get_or_init(|| column_types_of(self.cat)).as_slice(),
            ),
            None => std::borrow::Cow::Owned(column_types_of(self.cat)),
        };
        self.rec_always(|| Query::WholeCatalog, || self.whole_digest());
        types
    }

    /// Every table: records `WholeCatalog`.
    pub fn tables(&self) -> &'a [Table] {
        self.rec_always(|| Query::WholeCatalog, || self.whole_digest());
        &self.cat.tables
    }

    /// Every routine: records `WholeCatalog`.
    pub fn routines(&self) -> &'a [Routine] {
        self.rec_always(|| Query::WholeCatalog, || self.whole_digest());
        &self.cat.routines
    }

    /// Replay: asks the question `q` of this catalog through the very method
    /// pass 1 used, so the answer is recorded (to this reader's sink) by the
    /// code that made the draft. `false` for a question pass 1 never asks, or
    /// one that belongs to a [`RecordingView`] (pass 2): the stored draft is
    /// then not taken.
    pub fn ask(&self, q: &Query) -> bool {
        let idents = |parts: &[Part]| -> Vec<Ident> {
            parts
                .iter()
                .map(|(t, q)| Ident::new(t.as_str(), *q))
                .collect()
        };
        match q {
            Query::Table(parts) => {
                self.table(&idents(parts));
            }
            Query::TriggersNamed(schema, name) => {
                self.triggers_named(schema.as_deref(), name);
            }
            Query::ColumnTypes => {
                self.column_types();
            }
            Query::WholeCatalog => {
                self.tables();
            }
            _ => return false,
        }
        true
    }
}
