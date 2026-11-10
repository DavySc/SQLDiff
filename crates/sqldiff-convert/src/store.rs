//! The per-file result store of `sqldiff convert` (design review of
//! incremental large conversions, step 2): a file's checked conversion is
//! kept under a key that hashes everything the check read, and a later run
//! with the same key reuses it instead of checking the file again.
//!
//! A stored result is trusted only under an exact key. The key is the hash
//! of a [`CheckInput`], the value that holds every argument of
//! [`crate::check`]; a new input has to be added there to be read at all,
//! and adding it changes every key. That the check reads nothing else
//! (environment, files, clocks) is guarded by the `check_purity` test in
//! `sqldiff-core`. Keys are SHA-256.
//!
//! Since format 5 the key no longer holds the catalogs. A key names the file,
//! its draft and everything else a check reads except the catalogs' content,
//! and below it live up to [`MAX_ENTRIES`] entries, one per set of catalog
//! reads the check made (`<key>/<id>.json`, with the reads in
//! `<key>/<id>.reads`; the id is the hash of the read set). An entry is taken
//! only if every read in it has, against the catalogs of this run, the answer
//! it had when the entry was made ([`sqldiff_core::Replay`]); an edit that
//! no read of a file sees leaves its entry valid. Entries are written to a
//! temporary name and renamed into place, so a run killed at any moment
//! leaves only whole results. Two runs writing the same entry write the same
//! bytes (no timings are stored), so no lock is needed for the results
//! themselves.
//!
//! Every run re-checks a sample of the results it would reuse and compares
//! them byte for byte with the stored ones (`pick`, `compare`): a mismatch
//! writes a diagnostic bundle and empties the store before the run fails.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_core::catalog::builtins::Helper;
use sqldiff_core::catalog::record::{Outcome, Query, Reads, Side as ReadSide};
use sqldiff_core::catalog::Catalog;
use sqldiff_core::Dialect;

use crate::Conversion;

/// Where results live, below the output directory.
pub const DIR: &str = ".sqldiff/results";
/// Where a mismatch writes its diagnostic bundle.
/// The state of the whole tree that the per-file checks start from.
pub const TREE_DIR: &str = ".sqldiff/tree";
/// The rules' drafts of the files of one tree (the layout before drafts were
/// stored by their read sets): never written now, removed when found.
pub const DRAFTS_DIR: &str = ".sqldiff/drafts";
/// The rules' drafts, one entry per file text and read set:
/// `<draft key>/<id>.json` (the draft) and `<id>.reads` (what pass 1 read of
/// the tree to make it). Written as pass 1 goes, so a run killed in pass 1
/// keeps its work, and kept across edits: a draft is taken again when its
/// text is the same and everything it read still has its answer.
pub const DRAFT_ENTRIES_DIR: &str = ".sqldiff/draft-entries";

/// The stand-in flags of each stored result: `<result key>/<flags key>.json`.
pub const FLAGS_DIR: &str = ".sqldiff/flags";

pub const MISMATCH_DIR: &str = ".sqldiff/mismatch";

/// A 256-bit content hash, in hex.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Hashes named, length-prefixed fields, so two inputs never run together
/// into the same bytes.
struct KeyBuilder(Sha256);

impl KeyBuilder {
    fn new() -> Self {
        KeyBuilder(Sha256::new())
    }

    fn field(&mut self, name: &str, bytes: &[u8]) {
        for part in [name.as_bytes(), bytes] {
            self.0.update((part.len() as u64).to_le_bytes());
            self.0.update(part);
        }
    }

    fn finish(self) -> Key {
        Key(hex(&self.0.finalize()))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a catalog as it serialises (its fields are vectors, so the
/// order is the same on every run). Computed once per run and side: the
/// whole catalog is part of every file's key (stage 1 of the design).
pub fn catalog_digest(c: &Catalog) -> String {
    hex(&Sha256::digest(
        serde_json::to_vec(c).expect("a catalog serialises"),
    ))
}

/// Everything [`crate::check`] reads for one file of a tree: the only
/// argument a stored result's key is made from.
pub struct CheckInput<'a> {
    /// The check stage's fingerprint (`CHECK_FINGERPRINT`): another check may decide differently.
    pub build: &'a str,
    pub from: Dialect,
    pub to: Dialect,
    /// The reports are trimmed to what a review reads
    /// ([`crate::trim_reports`]).
    pub trim: bool,
    pub helpers: &'a [Helper],
    /// The file's text, and the rules' draft of it (with hand-written output
    /// from the ledger already applied).
    pub text: &'a str,
    pub draft: &'a crate::Draft,
    /// Whether the run supplies a source and a target catalog. What the
    /// catalogs hold is not in the key: it is recorded as the reads of each
    /// result (`Reads`), and a result is taken only when they still hold.
    pub source: bool,
    pub target: bool,
}

impl CheckInput<'_> {
    pub fn key(&self) -> Key {
        let mut k = KeyBuilder::new();
        k.field("format", b"5");
        k.field("build", self.build.as_bytes());
        k.field("from", self.from.id().as_bytes());
        k.field("to", self.to.id().as_bytes());
        k.field("trim", &[self.trim as u8]);
        k.field("helpers", format!("{:?}", self.helpers).as_bytes());
        k.field("text", self.text.as_bytes());
        // Also inside the draft's canonical text; a result checked from a
        // draft of the other mode is never reused.
        k.field("rules", format!("{:?}", self.draft.options()).as_bytes());
        k.field("draft", self.draft.canonical().as_bytes());
        k.field("source", &[self.source as u8]);
        k.field("target", &[self.target as u8]);
        k.finish()
    }
}

/// Everything [`crate::check_template`] reads for one job of a template
/// tree (a template, or a routine's opening and closing templates): the only
/// argument a stored template result's key is made from, like
/// [`CheckInput`] for a plain file.
pub struct TemplateInput<'a> {
    /// The check stage's fingerprint (`CHECK_FINGERPRINT`).
    pub build: &'a str,
    pub from: Dialect,
    pub to: Dialect,
    pub helpers: &'a [Helper],
    /// [`crate::job_material`]: the template and how it was read.
    pub job: &'a str,
    /// `TemplateDraft::canonical`: the draft the check starts from.
    pub draft: &'a str,
    /// [`catalog_digest`] of the source and the target catalog (in stand-in
    /// terms) the check binds against, when the run uses catalogs.
    pub source: Option<&'a str>,
    pub target: Option<&'a str>,
}

impl TemplateInput<'_> {
    pub fn key(&self) -> Key {
        let mut k = KeyBuilder::new();
        k.field("format", b"template-1");
        k.field("build", self.build.as_bytes());
        k.field("from", self.from.id().as_bytes());
        k.field("to", self.to.id().as_bytes());
        k.field("helpers", format!("{:?}", self.helpers).as_bytes());
        k.field("job", self.job.as_bytes());
        k.field("draft", self.draft.as_bytes());
        k.field("source", self.source.unwrap_or("none").as_bytes());
        k.field("target", self.target.unwrap_or("none").as_bytes());
        k.finish()
    }
}

/// Everything a tree run builds before the first file is checked: the source
/// catalog, the rules' draft of every file (hand-written output applied),
/// the target catalog with the stand-in tables, and the stand-ins. Each of
/// them is a function of the whole tree, so the state is stored under one
/// key made from all of it ([`TreeKey`]): a tree with the same key has the
/// same state, and any edit anywhere gives another key.
#[derive(Serialize, Deserialize)]
pub struct TreeState {
    pub key: String,
    pub source: Option<Catalog>,
    pub drafts: Vec<crate::Draft>,
    pub target: Option<Catalog>,
    pub stand_ins: crate::refused::StandIns,
}

impl TreeState {
    /// Whether two states serialise to the same bytes. A state that does not
    /// serialise equals nothing, not even itself.
    pub fn same_as(&self, other: &TreeState) -> bool {
        match (serde_json::to_vec(self), serde_json::to_vec(other)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// The key of a [`TreeState`]: the exact SHA-256 of everything the state is
/// built from, as named, length-prefixed fields.
pub struct TreeKey(String);

impl TreeKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Builds a [`TreeKey`]; the caller names every input of the state.
pub struct TreeKeyBuilder(KeyBuilder);

impl Default for TreeKeyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeKeyBuilder {
    pub fn new() -> Self {
        let mut k = KeyBuilder::new();
        k.field("format", b"tree-2");
        TreeKeyBuilder(k)
    }

    pub fn field(&mut self, name: &str, bytes: &[u8]) {
        self.0.field(name, bytes);
    }

    pub fn finish(self) -> TreeKey {
        TreeKey(self.0.finish().0)
    }
}

/// What a result file holds. `conversion` is the serialised conversion with
/// every long string that occurs more than once replaced by a marker into
/// `pool` (see [`intern`]): the notes of a tree repeat the same few texts in
/// every unit, and written out in full they made a result several times the
/// size of its input.
#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    /// The id of the entry: the hash of its read set and `mix_tried`
    /// ([`entry_id`]).
    id: String,
    pool: Vec<String>,
    conversion: serde_json::Value,
    /// The mix of the two variants of the draft that was built for this
    /// result, whether or not it won ([`crate::check::Mix`]). The draft code
    /// is not part of a result's fingerprint, so a reuse builds the mix again
    /// and compares.
    mix: Option<crate::check::Mix>,
    /// Whether a mix could be tried when the result was made: not in a run
    /// under a review ledger, where the draft is not built again. A result
    /// stored without a try says nothing about a mix, so a run that can try
    /// one does not take it.
    mix_tried: bool,
}

/// The `conversion` of a result file read without its `units` and
/// `note_table`: those two keys are skipped while parsing
/// (`serde::de::IgnoredAny`, so their elements are never built), every other
/// key becomes a `Value` as in [`Entry`]. The flags say which of the two keys
/// were present, so a file without them is still a miss as it is for `get`.
struct Lean {
    rest: serde_json::Value,
    units: bool,
    note_table: bool,
}

impl<'de> Deserialize<'de> for Lean {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Lean, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Lean;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a conversion object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Lean, A::Error> {
                let mut rest = serde_json::Map::new();
                let (mut units, mut note_table) = (false, false);
                while let Some(k) = m.next_key::<String>()? {
                    match k.as_str() {
                        "units" => {
                            m.next_value::<serde::de::IgnoredAny>()?;
                            units = true;
                        }
                        NOTE_TABLE => {
                            m.next_value::<serde::de::IgnoredAny>()?;
                            note_table = true;
                        }
                        _ => {
                            rest.insert(k, m.next_value()?);
                        }
                    }
                }
                Ok(Lean {
                    rest: serde_json::Value::Object(rest),
                    units,
                    note_table,
                })
            }
        }
        d.deserialize_map(V)
    }
}

/// [`Entry`] as [`Store::get_without_units`] reads it.
#[derive(Deserialize)]
struct LeanEntry {
    key: String,
    id: String,
    pool: Vec<String>,
    conversion: Lean,
    mix: Option<crate::check::Mix>,
    mix_tried: bool,
}

/// [`Entry`] as [`Store::peek`] reads it: the conversion and the pool are
/// parsed for their syntax only and never built.
#[derive(Deserialize)]
struct PeekEntry {
    key: String,
    id: String,
    #[allow(dead_code)]
    pool: serde::de::IgnoredAny,
    #[allow(dead_code)]
    conversion: serde::de::IgnoredAny,
    mix: Option<crate::check::Mix>,
    mix_tried: bool,
}

/// The body of a result file: the file is the SHA-256 of its body in hex, a
/// newline, then the body. A file edited after it was written (even into
/// other valid JSON) does not match its digest and is a miss, so it is
/// converted again instead of trusted.
fn verified_body(raw: &[u8]) -> Option<&[u8]> {
    let (digest, body) = (raw.get(..64)?, raw.get(65..)?);
    if raw[64] != b'\n' || digest != hex(&Sha256::digest(body)).as_bytes() {
        return None;
    }
    Some(body)
}

/// A stored file as written: the SHA-256 of `body` in hex, a newline, then
/// the body. Every file of the store has this frame (see [`verified_body`]).
pub(crate) fn framed(body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(body.len() + 65);
    bytes.extend_from_slice(hex(&Sha256::digest(body)).as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(body);
    bytes
}

/// A result file is named by its key: 64 hex digits.
fn is_key(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A temporary result file older than this belongs to a write that was
/// killed, not to one in flight.
const STALE_TMP: std::time::Duration = std::time::Duration::from_secs(3600);

/// A string at least this long that occurs twice goes to the pool.
const POOL_MIN: usize = 40;
/// Starts a marker. A real string starting with it is written with one more.
const MARK: char = '\u{1}';

fn count(v: &serde_json::Value, seen: &mut std::collections::BTreeMap<String, usize>) {
    match v {
        serde_json::Value::String(s) if s.len() >= POOL_MIN => {
            *seen.entry(s.clone()).or_default() += 1;
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| count(x, seen)),
        serde_json::Value::Object(o) => o.values().for_each(|x| count(x, seen)),
        _ => {}
    }
}

fn replace(v: &mut serde_json::Value, index: &std::collections::BTreeMap<&str, usize>) {
    match v {
        serde_json::Value::String(s) => {
            if let Some(i) = index.get(s.as_str()) {
                *s = format!("{MARK}{i}");
            } else if s.starts_with(MARK) {
                s.insert(0, MARK);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| replace(x, index)),
        serde_json::Value::Object(o) => o.values_mut().for_each(|x| replace(x, index)),
        _ => {}
    }
}

/// Moves the strings that repeat into a pool. Object keys are not touched.
/// The streamed `put` does the same one element at a time (`Plan`); this is
/// the in-memory original, kept as the oracle of its tests.
#[cfg(test)]
fn intern(mut v: serde_json::Value) -> (Vec<String>, serde_json::Value) {
    let mut seen = std::collections::BTreeMap::new();
    count(&v, &mut seen);
    let mut pool: Vec<String> = seen
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(s, _)| s)
        .collect();
    pool.sort();
    let index: std::collections::BTreeMap<&str, usize> = pool
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    replace(&mut v, &index);
    (pool, v)
}

/// Key of the table of distinct unit notes in a stored conversion.
const NOTE_TABLE: &str = "note_table";

/// Units of a file carry the file-level assumptions as notes of their own
/// (one file of 1,200 units held 200,000 equal notes, 94 % of its stored
/// result). In the stored form every distinct note is written once, in
/// order of first use, and a unit's `notes` are indices into that table.
#[cfg(test)]
fn share_notes(v: &mut serde_json::Value) {
    let Some(units) = v.get_mut("units").and_then(|u| u.as_array_mut()) else {
        return;
    };
    let mut table: Vec<serde_json::Value> = Vec::new();
    let mut at: std::collections::BTreeMap<String, usize> = Default::default();
    for u in units.iter_mut() {
        let Some(notes) = u.get_mut("notes").and_then(|n| n.as_array_mut()) else {
            continue;
        };
        for n in notes.iter_mut() {
            let key = n.to_string();
            let i = *at.entry(key).or_insert_with(|| {
                table.push(n.clone());
                table.len() - 1
            });
            *n = serde_json::Value::from(i);
        }
    }
    v[NOTE_TABLE] = serde_json::Value::Array(table);
}

/// The inverse of [`share_notes`]; `None` when an index is outside the table.
fn unshare_notes(v: &mut serde_json::Value) -> Option<()> {
    let table = match v.as_object_mut()?.remove(NOTE_TABLE)? {
        serde_json::Value::Array(t) => t,
        _ => return None,
    };
    if let Some(units) = v.get_mut("units").and_then(|u| u.as_array_mut()) {
        for u in units.iter_mut() {
            let Some(notes) = u.get_mut("notes").and_then(|n| n.as_array_mut()) else {
                continue;
            };
            for n in notes.iter_mut() {
                *n = table.get(usize::try_from(n.as_u64()?).ok()?)?.clone();
            }
        }
    }
    Some(())
}

/// The inverse of [`intern`]; `None` when a marker points outside the pool.
fn expand(v: &mut serde_json::Value, pool: &[String]) -> Option<()> {
    match v {
        serde_json::Value::String(s) if s.starts_with(MARK) => {
            let rest = &s[MARK.len_utf8()..];
            if rest.starts_with(MARK) {
                *s = rest.to_string();
            } else {
                *s = pool.get(rest.parse::<usize>().ok()?)?.clone();
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                expand(x, pool)?;
            }
        }
        serde_json::Value::Object(o) => {
            for x in o.values_mut() {
                expand(x, pool)?;
            }
        }
        _ => {}
    }
    Some(())
}

/// Most entries one key keeps. A file's entries differ only when catalogs
/// changed in a way it reads; a file seen under more catalog states than
/// this keeps the entries the last run used, then the first by id.
pub const MAX_ENTRIES: usize = 8;

/// One stored result: the key of the file's check and the id of the read
/// set it was made under.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntryRef {
    pub key: Key,
    pub id: String,
}

/// What [`Store::put`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    /// A new entry.
    Written,
    /// The entry was stored already, with the same bytes.
    Same,
    /// An entry of the same id holds other bytes. Nothing was changed. A
    /// check is a function of its reads, so two results under one id mean
    /// a recorder or determinism bug: the caller reports a mismatch.
    Differs,
}

/// The id of an entry: the SHA-256 of its read set (as `Reads::canonical`
/// writes it) and whether a mix could be tried, in hex.
pub fn entry_id(reads: &Reads, mix_tried: bool) -> String {
    let mut k = KeyBuilder::new();
    k.field("reads", reads.canonical().as_bytes());
    k.field("mix-tried", &[mix_tried as u8]);
    k.finish().0
}

/// The read set of an entry as it is written: a map keyed by a pair has no
/// JSON form, so it is a list, in the order of the set.
#[derive(Serialize, Deserialize)]
struct ReadsFile {
    key: String,
    id: String,
    mix_tried: bool,
    reads: Vec<(ReadSide, Query, String)>,
}

impl ReadsFile {
    fn of(key: &str, id: &str, mix_tried: bool, reads: &Reads) -> ReadsFile {
        ReadsFile {
            key: key.to_string(),
            id: id.to_string(),
            mix_tried,
            reads: reads
                .0
                .iter()
                .map(|((s, q), o)| (*s, q.clone(), o.hex()))
                .collect(),
        }
    }

    /// The read set, if this file is the one for `key` and `id` and the id
    /// is the hash of what it holds.
    fn open(self, key: &str, id: &str) -> Option<Reads> {
        if self.key != key || self.id != id {
            return None;
        }
        let mut reads = Reads::default();
        for (s, q, o) in self.reads {
            reads.0.insert((s, q), Outcome::from_hex(&o)?);
        }
        (entry_id(&reads, self.mix_tried) == id).then_some(reads)
    }
}

/// Writes `bytes` to a temporary name beside `dest` and renames it into
/// place, retrying a few times because a virus scanner on Windows can hold
/// a file for a moment.
fn write_atomically(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let tmp = dest.with_extension(format!(
        "tmp{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut last = String::new();
    for attempt in 0..5u64 {
        match std::fs::rename(&tmp, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = format!("{}: {e}", dest.display());
                std::thread::sleep(std::time::Duration::from_millis(40 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last)
}

/// Moves the finished temporary file `tmp` to `dest`, retried a few times
/// because a virus scanner on Windows can hold the file for a moment.
fn rename_into_place(tmp: &Path, dest: &Path) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..5u64 {
        match std::fs::rename(tmp, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = format!("{}: {e}", dest.display());
                std::thread::sleep(std::time::Duration::from_millis(40 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(tmp);
    Err(last)
}

/// The digest a result file names, if the file passes the frame check (its
/// body is read in pieces and hashed, never held whole).
fn file_digest(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 65];
    f.read_exact(&mut head).ok()?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    let digest = hex(&hash.finalize());
    (head[64] == b'\n' && head[..64] == *digest.as_bytes()).then_some(digest)
}

/// The conversion `c` with its three big arrays moved out; they go back into
/// `c` when this is dropped, whatever happened in between.
struct Taken<'a> {
    c: &'a mut Conversion,
    units: Vec<crate::Unit>,
    report: Vec<sqldiff_model::model::Finding>,
    output: Vec<sqldiff_model::model::Finding>,
}

impl<'a> Taken<'a> {
    fn new(c: &'a mut Conversion) -> Self {
        Taken {
            units: std::mem::take(&mut c.units),
            report: std::mem::take(&mut c.report.findings),
            output: std::mem::take(&mut c.output_check.findings),
            c,
        }
    }
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        self.c.units = std::mem::take(&mut self.units);
        self.c.report.findings = std::mem::take(&mut self.report);
        self.c.output_check.findings = std::mem::take(&mut self.output);
    }
}

/// What pass 1 of a put found: the small rest of the conversion as a
/// `Value` (without the three arrays, with the table of notes), the index of
/// every note in that table by its text, and the pool.
struct Plan {
    rest: serde_json::Value,
    note_at: std::collections::BTreeMap<String, usize>,
    pool: Vec<String>,
}

impl Plan {
    /// Builds the table of notes exactly as [`share_notes`] does (order of
    /// first use over the units) and counts the long strings exactly as
    /// [`intern`] does over the note-shared form (the table once, the units
    /// with their notes as indices), one element at a time.
    fn new(t: &Taken) -> Result<Plan, String> {
        let err = |e: serde_json::Error| e.to_string();
        let mut rest = serde_json::to_value(&*t.c).map_err(err)?;
        let mut seen = std::collections::BTreeMap::new();
        let mut table: Vec<serde_json::Value> = Vec::new();
        let mut note_at: std::collections::BTreeMap<String, usize> = Default::default();
        for u in &t.units {
            let mut v = serde_json::to_value(u).map_err(err)?;
            if let Some(notes) = v.get_mut("notes").and_then(|n| n.as_array_mut()) {
                for n in notes.iter_mut() {
                    let i = *note_at.entry(n.to_string()).or_insert_with(|| {
                        table.push(n.clone());
                        table.len() - 1
                    });
                    *n = serde_json::Value::from(i);
                }
            }
            count(&v, &mut seen);
        }
        for f in t.report.iter().chain(&t.output) {
            count(&serde_json::to_value(f).map_err(err)?, &mut seen);
        }
        rest[NOTE_TABLE] = serde_json::Value::Array(table);
        count(&rest, &mut seen);
        let mut pool: Vec<String> = seen
            .into_iter()
            .filter(|(_, n)| *n >= 2)
            .map(|(s, _)| s)
            .collect();
        pool.sort();
        Ok(Plan {
            rest,
            note_at,
            pool,
        })
    }
}

/// A member of an object of the conversion: a small value, or one of the
/// three big arrays, which are streamed.
enum Part {
    Val(serde_json::Value),
    Units,
    ReportFindings,
    OutputFindings,
    Obj(std::collections::BTreeMap<String, Part>),
}

/// Splits the small rest of the conversion into [`Part`]s. A `Map` is a
/// `BTreeMap`, so its keys come in byte order (a test pins that), and so do
/// those of a `BTreeMap`.
fn parts(rest: serde_json::Value) -> Option<Part> {
    let serde_json::Value::Object(map) = rest else {
        return None;
    };
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in map {
        let p = match (k.as_str(), v) {
            ("units", _) => Part::Units,
            ("report" | "output_check", serde_json::Value::Object(o)) => {
                let findings = || match k.as_str() {
                    "report" => Part::ReportFindings,
                    _ => Part::OutputFindings,
                };
                Part::Obj(
                    o.into_iter()
                        .map(|(ik, iv)| {
                            let p = if ik == "findings" {
                                findings()
                            } else {
                                Part::Val(iv)
                            };
                            (ik, p)
                        })
                        .collect(),
                )
            }
            (_, v) => Part::Val(v),
        };
        out.insert(k, p);
    }
    Some(Part::Obj(out))
}

/// Writes the body of a result (what `reference_body` returns) to `w`.
struct BodyWriter<'a, W: std::io::Write> {
    w: &'a mut W,
    t: &'a Taken<'a>,
    index: std::collections::BTreeMap<&'a str, usize>,
    note_at: &'a std::collections::BTreeMap<String, usize>,
}

impl<W: std::io::Write> BodyWriter<'_, W> {
    fn raw(&mut self, s: &str) -> Result<(), String> {
        self.w.write_all(s.as_bytes()).map_err(|e| e.to_string())
    }

    fn json<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), String> {
        serde_json::to_writer(&mut *self.w, v).map_err(|e| e.to_string())
    }

    fn element<T: Serialize>(&mut self, x: &T, note_at: bool) -> Result<(), String> {
        let mut v = serde_json::to_value(x).map_err(|e| e.to_string())?;
        if note_at {
            if let Some(notes) = v.get_mut("notes").and_then(|n| n.as_array_mut()) {
                for n in notes.iter_mut() {
                    let at = *self
                        .note_at
                        .get(&n.to_string())
                        .ok_or("a note is missing from the note table")?;
                    *n = serde_json::Value::from(at);
                }
            }
        }
        replace(&mut v, &self.index);
        self.json(&v)
    }

    fn array<T: Serialize>(&mut self, xs: &[T], note_at: bool) -> Result<(), String> {
        self.raw("[")?;
        for (i, x) in xs.iter().enumerate() {
            if i > 0 {
                self.raw(",")?;
            }
            self.element(x, note_at)?;
        }
        self.raw("]")
    }

    fn part(&mut self, p: Part) -> Result<(), String> {
        let t = self.t;
        match p {
            Part::Val(mut v) => {
                replace(&mut v, &self.index);
                self.json(&v)
            }
            Part::Units => self.array(&t.units, true),
            Part::ReportFindings => self.array(&t.report, false),
            Part::OutputFindings => self.array(&t.output, false),
            Part::Obj(m) => {
                self.raw("{")?;
                for (i, (k, p)) in m.into_iter().enumerate() {
                    if i > 0 {
                        self.raw(",")?;
                    }
                    self.json(&k)?;
                    self.raw(":")?;
                    self.part(p)?;
                }
                self.raw("}")
            }
        }
    }
}

/// Hashes what passes through to the file.
struct HashWriter {
    out: std::io::BufWriter<std::fs::File>,
    hash: Sha256,
}

impl std::io::Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Writes the file of a result to `tmp`: 65 bytes of placeholder, the body
/// (streamed and hashed as it goes), then the digest and the newline over
/// the placeholder. The file is `framed` of `reference_body`.
fn write_framed(
    tmp: &Path,
    key: &str,
    id: &str,
    plan: Plan,
    t: &Taken,
    mix: Option<&crate::check::Mix>,
    mix_tried: bool,
) -> Result<(), String> {
    use std::io::{Seek, SeekFrom, Write};
    let here = |e: std::io::Error| format!("{}: {e}", tmp.display());
    let file = std::fs::File::create(tmp).map_err(here)?;
    let mut hw = HashWriter {
        out: std::io::BufWriter::with_capacity(1 << 16, file),
        hash: Sha256::new(),
    };
    hw.out.write_all(&[b' '; 65]).map_err(here)?;
    {
        let Plan {
            rest,
            note_at,
            pool,
        } = plan;
        let index: std::collections::BTreeMap<&str, usize> = pool
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let conversion = parts(rest).ok_or("the conversion is not an object")?;
        let mut bw = BodyWriter {
            w: &mut hw,
            t,
            index,
            note_at: &note_at,
        };
        bw.raw("{\"key\":")?;
        bw.json(key)?;
        bw.raw(",\"id\":")?;
        bw.json(id)?;
        bw.raw(",\"pool\":")?;
        bw.json(&pool)?;
        bw.raw(",\"conversion\":")?;
        bw.part(conversion)?;
        bw.raw(",\"mix\":")?;
        bw.json(&mix)?;
        bw.raw(",\"mix_tried\":")?;
        bw.json(&mix_tried)?;
        bw.raw("}")?;
    }
    let digest = hex(&hw.hash.finalize_reset());
    let mut file = hw.out.into_inner().map_err(|e| here(e.into_error()))?;
    file.seek(SeekFrom::Start(0)).map_err(here)?;
    file.write_all(digest.as_bytes()).map_err(here)?;
    file.write_all(b"\n").map_err(here)?;
    Ok(())
}

/// The body of a result as the first version of [`Store::put`] built it, all
/// in memory: the oracle the tests compare the streamed file with.
#[cfg(test)]
fn reference_body(
    key: &Key,
    id: &str,
    c: &Conversion,
    mix: Option<&crate::check::Mix>,
    mix_tried: bool,
) -> Result<Vec<u8>, String> {
    let mut value = serde_json::to_value(c).map_err(|e| e.to_string())?;
    share_notes(&mut value);
    let (pool, conversion) = intern(value);
    serde_json::to_vec(&Entry {
        key: key.0.clone(),
        id: id.to_string(),
        pool,
        conversion,
        mix: mix.cloned(),
        mix_tried,
    })
    .map_err(|e| e.to_string())
}

/// The results of one output tree.
pub struct Store {
    out: PathBuf,
}

impl Store {
    /// The store of `out`. Nothing is created until a result is put.
    pub fn of(out: &Path) -> Store {
        Store {
            out: out.to_path_buf(),
        }
    }

    fn dir_rel(key: &Key) -> String {
        format!("{DIR}/{}", key.0)
    }

    fn entry_rel(key: &Key, id: &str) -> String {
        format!("{DIR}/{}/{id}.json", key.0)
    }

    fn reads_rel(key: &Key, id: &str) -> String {
        format!("{DIR}/{}/{id}.reads", key.0)
    }

    /// The entries stored under `key` whose read sets read back whole, with
    /// the read set of each, sorted by id. The result of an entry is read
    /// only by [`get`](Self::get): this is what a run lists for every file
    /// before it plans, so it reads the small `.reads` files, not the
    /// results. An entry whose result file is missing is not listed.
    pub fn entries(&self, key: &Key) -> Vec<(String, Reads)> {
        let Ok(dir) = crate::tree::dest_under(&self.out, &Self::dir_rel(key)) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".reads") else {
                continue;
            };
            if !is_key(id) || !e.path().with_extension("json").is_file() {
                continue;
            }
            let Some(reads) = std::fs::read(e.path()).ok().and_then(|raw| {
                let f: ReadsFile = serde_json::from_slice(verified_body(&raw)?).ok()?;
                f.open(&key.0, id)
            }) else {
                continue;
            };
            found.push((id.to_string(), reads));
        }
        found.sort_by(|x, y| x.0.cmp(&y.0));
        found
    }

    /// The entry of a result whose key holds everything it read (a template
    /// job: its key has the catalog digests, so its read set is empty and
    /// always holds), if one is stored.
    pub fn whole_entry(&self, key: &Key) -> Option<EntryRef> {
        self.entries(key)
            .into_iter()
            .next()
            .map(|(id, _)| EntryRef {
                key: key.clone(),
                id,
            })
    }

    /// The stored conversion of one entry, whole: every field as it was put.
    /// A file that does not read back whole, or does not name its own key
    /// and id, is a miss.
    pub fn get(&self, e: &EntryRef) -> Option<(Conversion, Option<crate::check::Mix>, bool)> {
        let path = crate::tree::dest_under(&self.out, &Self::entry_rel(&e.key, &e.id)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let body = verified_body(&raw)?;
        let mut x: Entry = serde_json::from_slice(body).ok()?;
        // The text is not needed once it is parsed.
        drop(raw);
        if x.key != e.key.0 || x.id != e.id {
            return None;
        }
        expand(&mut x.conversion, &x.pool)?;
        unshare_notes(&mut x.conversion)?;
        let conversion = serde_json::from_value(x.conversion).ok()?;
        Some((conversion, x.mix, x.mix_tried))
    }

    /// The `mix` and `mix_tried` of the entry `e` that passes the frame, key
    /// and id checks of [`Store::get`], without building the conversion: the
    /// file is read, its digest, key and id are checked, and the conversion
    /// and the pool are only skipped over. The re-check of a sampled file
    /// decides from this whether a stored result exists before its fresh
    /// check, and reads the conversion (`get`) after it, so the stored
    /// conversion is not held through the check. What `get` checks inside the
    /// conversion (that it expands and deserialises) is not checked here: a
    /// file that passes here and fails `get` is reported by the caller as a
    /// mismatch.
    pub fn peek(&self, e: &EntryRef) -> Option<(Option<crate::check::Mix>, bool)> {
        let path = crate::tree::dest_under(&self.out, &Self::entry_rel(&e.key, &e.id)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let body = verified_body(&raw)?;
        let x: PeekEntry = serde_json::from_slice(body).ok()?;
        if x.key != e.key.0 || x.id != e.id {
            return None;
        }
        Some((x.mix, x.mix_tried))
    }

    /// The stored conversion of the entry `e` with `units` left empty, for
    /// the finish read-back of a tree run, which gives the conversion its
    /// units and counts again from the cross-file pass (`Core::restore`) and
    /// never reads the stored ones.
    ///
    /// `units` and `note_table` are skipped while parsing, so the memory of
    /// the stored units is never taken; every other field is read and its
    /// pool markers expanded exactly as [`Store::get`] does, and the digest,
    /// the key, the id and the presence of both skipped keys are checked as
    /// there. What is not checked is what `get` would check inside the
    /// skipped parts: that `units` holds valid units, that `note_table` is a
    /// list, and that its indices and the pool markers in the units are in
    /// range (the digest still covers every byte). The reuse path needs the
    /// units and uses `get`.
    pub fn get_without_units(
        &self,
        e: &EntryRef,
    ) -> Option<(Conversion, Option<crate::check::Mix>, bool)> {
        let path = crate::tree::dest_under(&self.out, &Self::entry_rel(&e.key, &e.id)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let body = verified_body(&raw)?;
        let x: LeanEntry = serde_json::from_slice(body).ok()?;
        drop(raw);
        if x.key != e.key.0 || x.id != e.id || !x.conversion.units || !x.conversion.note_table {
            return None;
        }
        let mut v = x.conversion.rest;
        expand(&mut v, &x.pool)?;
        v["units"] = serde_json::Value::Array(Vec::new());
        let conversion = serde_json::from_value(v).ok()?;
        Some((conversion, x.mix, x.mix_tried))
    }

    /// Stores a result under the reads its check made. The file is `framed`
    /// of `reference_body`, byte for byte, but the big arrays of the
    /// conversion (`units`, `report.findings`, `output_check.findings`) are
    /// never one `Value` or one buffer. They are taken out of `c` for the
    /// call and put back on every path, a panic included, by [`Taken`]; the
    /// small rest is serialised once; the arrays are written element by
    /// element into a temporary file through a hashing writer. Pass 1 (the
    /// table of notes and the pool counts, which the head of the file needs)
    /// finishes before any byte is written.
    ///
    /// The file is renamed into place, retried a few times because a virus
    /// scanner on Windows can hold the file for a moment; the reads follow,
    /// so a reader that sees the reads sees the result. An entry already
    /// stored under the same id is left as it is: a second check with the
    /// same reads must give the same bytes, and `Put::Differs` reports that
    /// it did not. A `--fresh` run (`replace`) does not trust the store and
    /// writes its results over whatever is there.
    pub fn put(
        &self,
        key: &Key,
        c: &mut Conversion,
        mix: Option<&crate::check::Mix>,
        mix_tried: bool,
        reads: &Reads,
        replace: bool,
    ) -> Result<(EntryRef, Put), String> {
        let id = entry_id(reads, mix_tried);
        let entry = EntryRef {
            key: key.clone(),
            id: id.clone(),
        };
        let taken = Taken::new(c);
        let plan = Plan::new(&taken)?;
        let dest = crate::tree::dest_under(&self.out, &Self::entry_rel(key, &id))?;
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let tmp = dest.with_extension(format!(
            "tmp{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        if let Err(e) = write_framed(&tmp, &key.0, &id, plan, &taken, mix, mix_tried) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        drop(taken);
        let new_digest = file_digest(&tmp);
        let put = match file_digest(&dest) {
            Some(old) if Some(&old) == new_digest.as_ref() => {
                let _ = std::fs::remove_file(&tmp);
                Put::Same
            }
            Some(_) if !replace => {
                let _ = std::fs::remove_file(&tmp);
                return Ok((entry, Put::Differs));
            }
            _ => {
                rename_into_place(&tmp, &dest)?;
                Put::Written
            }
        };
        let rbody = serde_json::to_vec(&ReadsFile::of(&key.0, &id, mix_tried, reads))
            .map_err(|e| e.to_string())?;
        let rdest = crate::tree::dest_under(&self.out, &Self::reads_rel(key, &id))?;
        write_atomically(&rdest, &framed(&rbody))?;
        Ok((entry, put))
    }

    /// Empties the store (after a mismatch).
    pub fn clear(&self) {
        for d in [DIR, TREE_DIR, DRAFTS_DIR, DRAFT_ENTRIES_DIR, FLAGS_DIR] {
            if let Ok(p) = crate::tree::dest_under(&self.out, d) {
                let _ = std::fs::remove_dir_all(p);
            }
        }
    }

    /// Removes every stored result whose key is not in `live`: the results
    /// of files as they were before an edit, or of another build of the
    /// program, which no later run of this tree can use (and the files of
    /// the earlier formats, which are named by keys that are not live). A
    /// key that holds more than [`MAX_ENTRIES`] entries keeps `used` (the
    /// entries this run took or wrote, so the entry that matches the
    /// catalogs of this run is never the one dropped), then fills up by
    /// id. Files that are not results are left alone, except a temporary
    /// file of a killed write older than an hour. Returns how many files
    /// were removed.
    pub fn gc(
        &self,
        live: &std::collections::BTreeSet<String>,
        used: &BTreeSet<EntryRef>,
    ) -> usize {
        let Ok(dir) = crate::tree::dest_under(&self.out, DIR) else {
            return 0;
        };
        let stale = |e: &std::fs::DirEntry, name: &str| {
            name.split_once(".tmp").is_some_and(|(k, _)| {
                // `<key>.tmp…` beside a legacy file, `<id>.tmp…` in a key's
                // folder.
                is_key(k)
            }) && e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > STALE_TMP)
        };
        let mut removed = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                if !is_key(&name) {
                    continue;
                }
                if !live.contains(&name) {
                    removed += std::fs::read_dir(e.path()).map_or(0, |d| d.count());
                    let _ = std::fs::remove_dir_all(e.path());
                } else {
                    removed += self.evict(&name, &e.path(), used, &stale);
                }
                continue;
            }
            let Some(key) = name.strip_suffix(".json") else {
                // A write killed before its rename leaves `<key>.tmp<pid>-…`;
                // one that old is no write in flight.
                if stale(&e, &name) && std::fs::remove_file(e.path()).is_ok() {
                    removed += 1;
                }
                continue;
            };
            // A file of an earlier format sits beside the folders; no key of
            // this format names it.
            if is_key(key) && std::fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
        // The flags of results that are gone go with them.
        if let Ok(dir) = crate::tree::dest_under(&self.out, FLAGS_DIR) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if is_key(&name) && !live.contains(&name) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        removed
    }

    /// Removes one entry (its result, its reads, its stand-in flags): the
    /// result of a mix the draft code no longer builds is of no use to any
    /// run, and storing the file again would otherwise meet it under the same
    /// id.
    pub fn remove(&self, e: &EntryRef) {
        let rels = [
            Self::reads_rel(&e.key, &e.id),
            Self::entry_rel(&e.key, &e.id),
        ];
        for rel in rels {
            if let Ok(p) = crate::tree::dest_under(&self.out, &rel) {
                let _ = std::fs::remove_file(p);
            }
        }
        if let Ok(f) =
            crate::tree::dest_under(&self.out, &format!("{FLAGS_DIR}/{}/{}", e.key.0, e.id))
        {
            let _ = std::fs::remove_dir_all(f);
        }
    }

    /// Trims the folder of one live key to [`MAX_ENTRIES`] entries (see
    /// [`gc`](Self::gc)), and removes the temporary files of killed writes.
    /// Returns how many files were removed.
    fn evict(
        &self,
        key: &str,
        dir: &Path,
        used: &BTreeSet<EntryRef>,
        stale: &dyn Fn(&std::fs::DirEntry, &str) -> bool,
    ) -> usize {
        let mut removed = 0;
        let mut ids = BTreeSet::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if stale(&e, &name) {
                if std::fs::remove_file(e.path()).is_ok() {
                    removed += 1;
                }
                continue;
            }
            if let Some(id) = name
                .strip_suffix(".json")
                .or_else(|| name.strip_suffix(".reads"))
            {
                if is_key(id) {
                    ids.insert(id.to_string());
                }
            }
        }
        if ids.len() <= MAX_ENTRIES {
            return removed;
        }
        let key_used: BTreeSet<&str> = used
            .iter()
            .filter(|u| u.key.0 == key)
            .map(|u| u.id.as_str())
            .collect();
        let mut keep: BTreeSet<&str> = ids
            .iter()
            .map(String::as_str)
            .filter(|id| key_used.contains(id))
            .collect();
        for id in &ids {
            if keep.len() >= MAX_ENTRIES {
                break;
            }
            keep.insert(id);
        }
        for id in ids.iter().filter(|id| !keep.contains(id.as_str())) {
            for ext in ["json", "reads"] {
                if std::fs::remove_file(dir.join(format!("{id}.{ext}"))).is_ok() {
                    removed += 1;
                }
            }
            if let Ok(f) = crate::tree::dest_under(&self.out, &format!("{FLAGS_DIR}/{key}/{id}")) {
                let _ = std::fs::remove_dir_all(f);
            }
        }
        removed
    }

    /// The key of the flags of file `fi` for the result `result`: the flags
    /// are a function of the result, the stand-ins, the file's place in the
    /// tree and the dialects ([`crate::refused::StandIns::flags_of`]).
    pub fn flags_key(
        result: &EntryRef,
        stand_ins_digest: &str,
        fi: usize,
        from: Dialect,
        to: Dialect,
    ) -> Key {
        let mut k = KeyBuilder::new();
        k.field("format", b"flags-3");
        k.field("result", result.key.0.as_bytes());
        k.field("entry", result.id.as_bytes());
        k.field("stand-ins", stand_ins_digest.as_bytes());
        k.field("file", &(fi as u64).to_le_bytes());
        k.field("from", from.id().as_bytes());
        k.field("to", to.id().as_bytes());
        k.finish()
    }

    fn flags_rel(result: &EntryRef, flags: &Key) -> String {
        format!(
            "{FLAGS_DIR}/{}/{}/{}.json",
            result.key.0, result.id, flags.0
        )
    }

    /// The stored flags, or a miss (also for a file that does not read back
    /// whole).
    pub fn get_flags(
        &self,
        result: &EntryRef,
        flags: &Key,
    ) -> Option<Vec<crate::refused::UnitFlag>> {
        let path = crate::tree::dest_under(&self.out, &Self::flags_rel(result, flags)).ok()?;
        let raw = std::fs::read(path).ok()?;
        serde_json::from_slice(verified_body(&raw)?).ok()
    }

    /// Stores flags (best effort: a flag set that is not stored is computed
    /// again next time).
    pub fn put_flags(&self, result: &EntryRef, flags: &Key, v: &[crate::refused::UnitFlag]) {
        let Ok(dest) = crate::tree::dest_under(&self.out, &Self::flags_rel(result, flags)) else {
            return;
        };
        let Ok(body) = serde_json::to_vec(v) else {
            return;
        };
        let bytes = framed(&body);
        if let Some(p) = dest.parent() {
            if std::fs::create_dir_all(p).is_err() {
                return;
            }
        }
        let tmp = dest.with_extension(format!(
            "tmp{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn tree_rel(key: &TreeKey) -> String {
        format!("{TREE_DIR}/{}.json", key.0)
    }

    /// The stored state of a tree. A file that does not read back whole, or
    /// does not name its own key, is a miss.
    pub fn get_tree(&self, key: &TreeKey) -> Option<TreeState> {
        let path = crate::tree::dest_under(&self.out, &Self::tree_rel(key)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let packed = verified_body(&raw)?;
        let mut json = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(packed), &mut json).ok()?;
        drop(raw);
        let st: TreeState = serde_json::from_slice(&json).ok()?;
        drop(json);
        (st.key == key.0).then_some(st)
    }

    /// Stores the state of a tree and drops the states of other trees: only
    /// the last one is of use to a resumed run.
    pub fn put_tree(&self, key: &TreeKey, st: &TreeState) -> Result<(), String> {
        // Compressed as it is written: the drafts repeat the same long
        // strings, and nothing is held in memory beside the state itself.
        // serde_json writes many tiny pieces and the encoder prepares its
        // whole output buffer for each write: a buffer in front of it turns
        // that into a few large writes (this step took a second on WWI).
        let mut enc = std::io::BufWriter::with_capacity(
            1 << 16,
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()),
        );
        serde_json::to_writer(&mut enc, st).map_err(|e| e.to_string())?;
        let bytes = framed(
            &enc.into_inner()
                .map_err(|e| e.into_error().to_string())?
                .finish()
                .map_err(|e| e.to_string())?,
        );
        let dest = crate::tree::dest_under(&self.out, &Self::tree_rel(key))?;
        if let Some(p) = dest.parent() {
            let _ = std::fs::remove_dir_all(p);
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let tmp = dest.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", dest.display())
        })
    }

    /// Writes what a stand-in flags mismatch found: the flags the store held
    /// and the ones computed again for the same result.
    pub fn bundle_flags(
        &self,
        result: &EntryRef,
        flags: &Key,
        stored: &[crate::refused::UnitFlag],
        fresh: &[crate::refused::UnitFlag],
    ) {
        let rel = format!("{MISMATCH_DIR}/flags-{}.json", flags.0);
        let body = serde_json::json!({
            "result": result.key.0,
            "entry": result.id,
            "flags": flags.0,
            "stored": stored,
            "fresh": fresh,
        });
        if let Ok(b) = serde_json::to_vec_pretty(&body) {
            let _ = crate::tree::write_under(&self.out, &rel, &b);
        }
    }

    /// Whether the stored result of the entry `e` serialises as `fresh`, without
    /// building the stored [`Conversion`]: the file is read and its digest
    /// and key checked as [`Store::get`] does, and its `conversion` is
    /// followed by a streaming deserialiser. `fresh` is borrowed mutably only
    /// to move its three big arrays out for the call; they are put back on
    /// every path.
    ///
    /// **Why this equals `compare(&get(e), fresh)`.** `to_vec` of a
    /// [`Conversion`] is the concatenation of its fields in declared order,
    /// and `Conversion` and [`sqldiff_model::model::Report`] are plain
    /// derived structs (no `flatten`, no untagged enum, no hand-written
    /// `Serialize`; `skip_serializing_if` decides from the field's own
    /// value). Let `lean` be a conversion with `units`, `report.findings` and
    /// `output_check.findings` emptied. The text of a conversion is the text
    /// of its `lean` with each of those three `[]` replaced by the array's
    /// text, and an array's text is `[`, its elements' texts joined by `,`,
    /// `]`. Every text is a JSON value, which splits into its parts in only
    /// one way, so two conversions serialise alike exactly when their `lean`s
    /// do and each of the three arrays has the same length and every pair of
    /// elements serialises alike. That is what is checked: the arrays one
    /// element at a time (each decoded through the same steps `get` applies:
    /// pool markers expanded, a unit's note indices replaced from the table
    /// of notes, `from_value` into the element's type), and the `lean` of the
    /// stored side built from the other members with the same steps and
    /// `from_value::<Conversion>`. An array the file lacks counts as empty
    /// where `from_value` accepts that.
    ///
    /// [`Streamed::Undecided`] when the file cannot be followed (another
    /// order of keys than `put` writes, a part that does not decode, a wrong
    /// key or digest): the caller then uses `get`.
    pub fn compare_streamed(&self, e: &EntryRef, fresh: &mut Conversion) -> Streamed {
        let Ok(path) = crate::tree::dest_under(&self.out, &Self::entry_rel(&e.key, &e.id)) else {
            return Streamed::Undecided;
        };
        let Ok(raw) = std::fs::read(path) else {
            return Streamed::Undecided;
        };
        let Some(body) = verified_body(&raw) else {
            return Streamed::Undecided;
        };
        let taken = Taken::new(fresh);
        let mut ctx = Ctx {
            key: &e.key.0,
            id: &e.id,
            units: &taken.units,
            report: &taken.report,
            output: &taken.output,
            pool: None,
            table: None,
            different: false,
            seen: [false; 3],
            rest: serde_json::Map::new(),
        };
        let mut de = serde_json::Deserializer::from_slice(body);
        let read = serde::de::DeserializeSeed::deserialize(EntrySeed { ctx: &mut ctx }, &mut de)
            .and_then(|()| de.end());
        if ctx.different {
            return Streamed::Different;
        }
        if read.is_err() {
            return Streamed::Undecided;
        }
        for (which, seen) in [Arr::Units, Arr::Report, Arr::Output]
            .into_iter()
            .zip(ctx.seen)
        {
            if !seen && ctx.len(which) != 0 {
                return Streamed::Different;
            }
        }
        let rest = std::mem::take(&mut ctx.rest);
        let Ok(lean) = serde_json::from_value::<Conversion>(serde_json::Value::Object(rest)) else {
            return Streamed::Undecided;
        };
        if compare(&lean, &*taken.c) {
            Streamed::Equal
        } else {
            Streamed::Different
        }
    }

    /// Compares the stored result of the entry `e` with the `fresh` check of the
    /// same input after it. `other_differs` is a difference found elsewhere
    /// (the mix), which makes it a mismatch whatever the conversions say.
    /// The stored conversion is followed by [`Store::compare_streamed`],
    /// never built; only when that finds a difference, cannot decide, or
    /// `other_differs` is set is the whole result read ([`Store::get`]), and
    /// a mismatch is bundled with both sides as before. A result that
    /// passed [`Store::peek`] and does not read back whole is
    /// [`Rechecked::Unread`], a mismatch, not a miss.
    pub fn recheck(&self, e: &EntryRef, fresh: &mut Conversion, other_differs: bool) -> Rechecked {
        let streamed = self.compare_streamed(e, fresh);
        if streamed == Streamed::Equal && !other_differs {
            return Rechecked::Same;
        }
        match self.get(e) {
            None => {
                self.bundle_unread(&e.key, fresh);
                Rechecked::Unread
            }
            Some((old, ..)) => {
                if streamed == Streamed::Undecided && !other_differs && compare(&old, fresh) {
                    Rechecked::Same
                } else {
                    self.bundle(&e.key, &old, fresh);
                    Rechecked::Differs
                }
            }
        }
    }

    /// Writes what a mismatch found when a stored result was present before
    /// a fresh check and did not read back whole after it: the fresh result,
    /// and `null` for the stored one.
    pub fn bundle_unread(&self, key: &Key, fresh: &Conversion) {
        let rel = format!("{MISMATCH_DIR}/{}.json", key.0);
        let body = serde_json::json!({ "key": key.0, "stored": null, "fresh": fresh });
        if let Ok(b) = serde_json::to_vec_pretty(&body) {
            let _ = crate::tree::write_under(&self.out, &rel, &b);
        }
    }

    /// Writes what a mismatch found, for whoever looks into it.
    pub fn bundle(&self, key: &Key, stored: &Conversion, fresh: &Conversion) {
        let rel = format!("{MISMATCH_DIR}/{}.json", key.0);
        let body = serde_json::json!({ "key": key.0, "stored": stored, "fresh": fresh });
        if let Ok(b) = serde_json::to_vec_pretty(&body) {
            let _ = crate::tree::write_under(&self.out, &rel, &b);
        }
    }
}

/// A writer that accepts exactly the bytes of `expect`, in order, and fails
/// at the first byte that differs or runs past its end.
struct Expect<'a> {
    expect: &'a [u8],
    at: usize,
    /// Set when the writer failed because the bytes differ.
    broke: bool,
}

impl std::io::Write for Expect<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.expect.get(self.at..self.at + buf.len()) != Some(buf) {
            self.broke = true;
            return Err(std::io::Error::other("differs"));
        }
        self.at += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Whether a reused result equals a fresh check of the same input, byte for
/// byte as it serialises. `stored` is serialised into one buffer and `fresh`
/// straight into a writer that checks each byte against it and stops at the
/// first difference, so there is one buffer, not two. A side that does not
/// serialise equals only the other side not serialising.
pub fn compare(stored: &Conversion, fresh: &Conversion) -> bool {
    let Ok(buf) = serde_json::to_vec(stored) else {
        return serde_json::to_writer(std::io::sink(), fresh).is_err();
    };
    let mut w = Expect {
        expect: &buf,
        at: 0,
        broke: false,
    };
    serde_json::to_writer(&mut w, fresh).is_ok() && w.at == buf.len()
}

/// What [`Store::compare_streamed`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Streamed {
    /// Every part serialises as the fresh one: [`compare`] would say equal.
    Equal,
    /// Some part serialises differently (or an array has another length).
    Different,
    /// The file could not be followed (it is not in the order `put` writes
    /// it, a part does not decode, the key or digest is wrong, ...). Nothing
    /// is known; the caller reads the whole result with [`Store::get`].
    Undecided,
}

/// How [`Store::recheck`] found a stored result against the fresh one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rechecked {
    Same,
    /// The stored result differs; both are in the mismatch bundle.
    Differs,
    /// The stored result was present but does not read back whole; the fresh
    /// one is in the mismatch bundle.
    Unread,
}

/// `Ok(true)` when `stored` serialises as `fresh`, byte for byte. `Err` when
/// the fresh side does not serialise, which only the whole comparison
/// ([`compare`]) judges.
fn same_bytes<T: Serialize>(stored: &T, fresh: Option<&T>) -> Result<bool, ()> {
    let Some(fresh) = fresh else {
        return Ok(false);
    };
    let buf = serde_json::to_vec(fresh).map_err(|_| ())?;
    let mut w = Expect {
        expect: &buf,
        at: 0,
        broke: false,
    };
    Ok(serde_json::to_writer(&mut w, stored).is_ok() && w.at == buf.len())
}

#[derive(Clone, Copy)]
enum Arr {
    Units = 0,
    Report = 1,
    Output = 2,
}

/// The state of one streamed comparison: the fresh conversion's three big
/// arrays, the pool and the table of notes read so far, and the small rest
/// of the stored conversion.
struct Ctx<'t> {
    key: &'t str,
    id: &'t str,
    units: &'t [crate::Unit],
    report: &'t [sqldiff_model::model::Finding],
    output: &'t [sqldiff_model::model::Finding],
    pool: Option<Vec<String>>,
    table: Option<Vec<serde_json::Value>>,
    different: bool,
    seen: [bool; 3],
    rest: serde_json::Map<String, serde_json::Value>,
}

fn bad<E: serde::de::Error>(what: &str) -> E {
    E::custom(what)
}

impl Ctx<'_> {
    fn len(&self, which: Arr) -> usize {
        match which {
            Arr::Units => self.units.len(),
            Arr::Report => self.report.len(),
            Arr::Output => self.output.len(),
        }
    }

    fn expand(&self, v: &mut serde_json::Value) -> Option<()> {
        expand(v, self.pool.as_deref()?)
    }

    /// Decodes element `i` of an array exactly as [`Store::get`] decodes it
    /// (pool markers expanded, a unit's note indices replaced by the notes of
    /// the table, `from_value` into the element's type) and says whether it
    /// serialises as the fresh element `i`.
    fn element(&self, which: Arr, i: usize, mut v: serde_json::Value) -> Result<bool, ()> {
        self.expand(&mut v).ok_or(())?;
        match which {
            Arr::Units => {
                let table = self.table.as_ref().ok_or(())?;
                if let Some(notes) = v.get_mut("notes").and_then(|n| n.as_array_mut()) {
                    for n in notes.iter_mut() {
                        let at = usize::try_from(n.as_u64().ok_or(())?).map_err(|_| ())?;
                        *n = table.get(at).ok_or(())?.clone();
                    }
                }
                let u: crate::Unit = serde_json::from_value(v).map_err(|_| ())?;
                same_bytes(&u, self.units.get(i))
            }
            Arr::Report | Arr::Output => {
                let f: sqldiff_model::model::Finding = serde_json::from_value(v).map_err(|_| ())?;
                let fresh = if matches!(which, Arr::Report) {
                    self.report
                } else {
                    self.output
                };
                same_bytes(&f, fresh.get(i))
            }
        }
    }
}

/// One of the three big arrays, read element by element.
struct ArrSeed<'a, 't> {
    ctx: &'a mut Ctx<'t>,
    which: Arr,
}

impl<'de> serde::de::DeserializeSeed<'de> for ArrSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        if std::mem::replace(&mut self.ctx.seen[self.which as usize], true) {
            return Err(bad("an array twice"));
        }
        d.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for ArrSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut i = 0usize;
        while let Some(v) = seq.next_element::<serde_json::Value>()? {
            match self.ctx.element(self.which, i, v) {
                Ok(true) => {}
                Ok(false) => {
                    self.ctx.different = true;
                    return Err(bad("an element differs"));
                }
                Err(()) => return Err(bad("an element does not decode")),
            }
            i += 1;
        }
        if i != self.ctx.len(self.which) {
            self.ctx.different = true;
            return Err(bad("an array has another length"));
        }
        Ok(())
    }
}

/// `report` or `output_check`: an object whose `findings` array is read
/// element by element and whose other members are kept as values. The
/// result has `findings` empty.
struct ReportSeed<'a, 't> {
    ctx: &'a mut Ctx<'t>,
    which: Arr,
}

impl<'de> serde::de::DeserializeSeed<'de> for ReportSeed<'_, '_> {
    type Value = serde_json::Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<serde_json::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for ReportSeed<'_, '_> {
    type Value = serde_json::Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a report object")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> Result<serde_json::Value, A::Error> {
        let mut inner = serde_json::Map::new();
        while let Some(k) = map.next_key::<String>()? {
            if k == "findings" {
                map.next_value_seed(ArrSeed {
                    ctx: &mut *self.ctx,
                    which: self.which,
                })?;
                inner.insert(k, serde_json::Value::Array(Vec::new()));
            } else {
                let mut v: serde_json::Value = map.next_value()?;
                self.ctx.expand(&mut v).ok_or_else(|| bad("a marker"))?;
                inner.insert(k, v);
            }
        }
        Ok(serde_json::Value::Object(inner))
    }
}

/// The `conversion` of a result: `note_table` first (keys come in byte
/// order), then everything else. `units` and the two `findings` arrays are
/// compared element by element and left empty in `ctx.rest`.
struct ConvSeed<'a, 't> {
    ctx: &'a mut Ctx<'t>,
}

impl<'de> serde::de::DeserializeSeed<'de> for ConvSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for ConvSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a conversion object")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(k) = map.next_key::<String>()? {
            match k.as_str() {
                NOTE_TABLE => {
                    let mut v: serde_json::Value = map.next_value()?;
                    self.ctx.expand(&mut v).ok_or_else(|| bad("a marker"))?;
                    match v {
                        serde_json::Value::Array(t) if self.ctx.table.is_none() => {
                            self.ctx.table = Some(t)
                        }
                        _ => return Err(bad("the note table")),
                    }
                }
                "units" => {
                    if self.ctx.table.is_none() {
                        return Err(bad("units before the note table"));
                    }
                    map.next_value_seed(ArrSeed {
                        ctx: &mut *self.ctx,
                        which: Arr::Units,
                    })?;
                    self.ctx
                        .rest
                        .insert(k, serde_json::Value::Array(Vec::new()));
                }
                "report" | "output_check" => {
                    let which = if k == "report" {
                        Arr::Report
                    } else {
                        Arr::Output
                    };
                    let v = map.next_value_seed(ReportSeed {
                        ctx: &mut *self.ctx,
                        which,
                    })?;
                    self.ctx.rest.insert(k, v);
                }
                _ => {
                    let mut v: serde_json::Value = map.next_value()?;
                    self.ctx.expand(&mut v).ok_or_else(|| bad("a marker"))?;
                    self.ctx.rest.insert(k, v);
                }
            }
        }
        if self.ctx.table.is_none() {
            return Err(bad("no note table"));
        }
        Ok(())
    }
}

/// The file: `key`, `id`, `pool`, `conversion` in the order `put` writes them.
struct EntrySeed<'a, 't> {
    ctx: &'a mut Ctx<'t>,
}

impl<'de> serde::de::DeserializeSeed<'de> for EntrySeed<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for EntrySeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a result")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let (mut key, mut conversion) = (false, false);
        while let Some(k) = map.next_key::<String>()? {
            match k.as_str() {
                "key" => {
                    let s: String = map.next_value()?;
                    if s != self.ctx.key {
                        return Err(bad("another key"));
                    }
                    key = true;
                }
                "id" => {
                    let s: String = map.next_value()?;
                    if s != self.ctx.id {
                        return Err(bad("another id"));
                    }
                }
                "pool" => self.ctx.pool = Some(map.next_value()?),
                "conversion" => {
                    if !key || self.ctx.pool.is_none() || conversion {
                        return Err(bad("the conversion before the key and the pool"));
                    }
                    map.next_value_seed(ConvSeed {
                        ctx: &mut *self.ctx,
                    })?;
                    conversion = true;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        if key && conversion {
            Ok(())
        } else {
            Err(bad("an incomplete result"))
        }
    }
}

/// Which stored results to re-check this run: one file in 50 (at least one),
/// drawn uniformly whatever its size. A file's chance of being re-checked is
/// the same on every run, however large it is, so the biggest files are
/// covered too; the expected cost is 2% of the bytes, the worst case is the
/// largest files.
///
/// (An earlier sample took 2% of the stored bytes, smallest files first. A
/// tree has many tiny files, so that took 80 to 146 of 336 files of
/// WideWorldImporters on every resume and saved little.)
///
/// `present` is `(index, bytes)` of every file whose result is stored.
pub fn pick(present: &[(usize, usize)], seed: u64) -> BTreeSet<usize> {
    // Rank the files by a hash of (seed, index) and take the first.
    let mut ranked: Vec<(u64, usize)> = present
        .iter()
        .map(|&(i, _)| (mix(seed, i as u64), i))
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .take(present.len().div_ceil(50))
        .map(|(_, i)| i)
        .collect()
}

/// A splitmix64 step over `seed` and `i`.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed.wrapping_add(i.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The seed of the re-check samples of one run: a function of the tree's
/// key and of how many runs this store has seen, never of the clock, so the
/// same tree and store history always re-check the same files. The counter
/// makes successive runs of an unchanged tree cover different files over
/// time. A seed never changes a result: it only decides which stored ones
/// are built again to be compared.
pub fn seed_of(tree: &TreeKey, run: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in tree.as_str().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    mix(h, run)
}

// ---------------------------------------------------------------------------
// Draft entries (performance plan W4.3, Phase B).
// ---------------------------------------------------------------------------

/// The key of a draft: everything the rules read that is not a recorded
/// pass-1 read, named. The running build (the rules themselves), the
/// dialects, the rule options, whether a source catalog was supplied, and the
/// file's text. The path is not in it (a draft is not a function of the
/// path), nor is the review ledger (hand-written output is applied after the
/// draft), nor the base catalogs (they are part of the catalog, which the
/// read set covers).
pub fn draft_key(
    build: &str,
    from: &str,
    to: &str,
    rules: &str,
    catalog_supplied: bool,
    text: &str,
) -> Key {
    let mut k = KeyBuilder::new();
    k.field("draft-build", build.as_bytes());
    k.field("from", from.as_bytes());
    k.field("to", to.as_bytes());
    k.field("rules", rules.as_bytes());
    k.field("catalog", &[catalog_supplied as u8]);
    k.field("text", text.as_bytes());
    k.finish()
}

/// What a draft entry file holds.
#[derive(Deserialize)]
struct DraftEntryFile {
    key: String,
    id: String,
    draft: crate::Draft,
}

#[derive(Serialize)]
struct DraftEntryFileRef<'a> {
    key: &'a str,
    id: &'a str,
    draft: &'a crate::Draft,
}

impl Store {
    fn draft_dir_rel(key: &Key) -> String {
        format!("{DRAFT_ENTRIES_DIR}/{}", key.0)
    }

    /// The draft entries stored under `key` whose read sets read back whole,
    /// with each read set, by id.
    pub fn draft_entries(&self, key: &Key) -> Vec<(String, Reads)> {
        let Ok(dir) = crate::tree::dest_under(&self.out, &Self::draft_dir_rel(key)) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".reads") else {
                continue;
            };
            if !is_key(id) || !e.path().with_extension("json").is_file() {
                continue;
            }
            let Some(reads) = std::fs::read(e.path()).ok().and_then(|raw| {
                let f: ReadsFile = serde_json::from_slice(verified_body(&raw)?).ok()?;
                f.open(&key.0, id)
            }) else {
                continue;
            };
            found.push((id.to_string(), reads));
        }
        found.sort_by(|x, y| x.0.cmp(&y.0));
        found
    }

    /// The draft of entry `id` under `key`; a file that does not read back
    /// whole, or does not name its own key and id, is a miss.
    pub fn get_draft_entry(&self, key: &Key, id: &str) -> Option<crate::Draft> {
        let rel = format!("{}/{id}.json", Self::draft_dir_rel(key));
        let raw = std::fs::read(crate::tree::dest_under(&self.out, &rel).ok()?).ok()?;
        let f: DraftEntryFile = serde_json::from_slice(verified_body(&raw)?).ok()?;
        (f.key == key.0 && f.id == id).then_some(f.draft)
    }

    /// Stores a draft with the read set it was made under. The draft first,
    /// then the read set, which is what lists it. An entry stored already
    /// under the same id with other bytes is left as it is and reported
    /// ([`Put::Differs`]): a draft is a function of its key and reads, so
    /// that is a recorder or determinism bug.
    pub fn put_draft_entry(
        &self,
        key: &Key,
        reads: &Reads,
        draft: &crate::Draft,
    ) -> Result<(String, Put), String> {
        let id = entry_id(reads, false);
        let body = serde_json::to_vec(&DraftEntryFileRef {
            key: &key.0,
            id: &id,
            draft,
        })
        .map_err(|e| e.to_string())?;
        let bytes = framed(&body);
        let dest = crate::tree::dest_under(
            &self.out,
            &format!("{}/{id}.json", Self::draft_dir_rel(key)),
        )?;
        let put = match std::fs::read(&dest) {
            Ok(old) if old == bytes => Put::Same,
            // An entry that reads back whole and holds another draft.
            Ok(old)
                if verified_body(&old)
                    .and_then(|b| serde_json::from_slice::<DraftEntryFile>(b).ok())
                    .is_some_and(|f| f.key == key.0 && f.id == id) =>
            {
                return Ok((id, Put::Differs));
            }
            // Missing, or damaged: written again.
            _ => {
                write_atomically(&dest, &bytes)?;
                Put::Written
            }
        };
        let rbody = serde_json::to_vec(&ReadsFile::of(&key.0, &id, false, reads))
            .map_err(|e| e.to_string())?;
        let rdest = crate::tree::dest_under(
            &self.out,
            &format!("{}/{id}.reads", Self::draft_dir_rel(key)),
        )?;
        write_atomically(&rdest, &framed(&rbody))?;
        Ok((id, put))
    }

    /// Removes the draft entries of keys not in `live` (files as they were
    /// before an edit, or of another build), keeps at most [`MAX_ENTRIES`]
    /// of the others (those in `used` first, then by id), and removes the
    /// folder of the earlier draft layout. Returns how many files went.
    pub fn gc_drafts(&self, live: &BTreeSet<String>, used: &BTreeSet<(String, String)>) -> usize {
        let mut removed = 0;
        if let Ok(old) = crate::tree::dest_under(&self.out, DRAFTS_DIR) {
            if old.exists() {
                let _ = std::fs::remove_dir_all(&old);
                removed += 1;
            }
        }
        let Ok(root) = crate::tree::dest_under(&self.out, DRAFT_ENTRIES_DIR) else {
            return removed;
        };
        for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_key(&name) {
                continue;
            }
            if !live.contains(&name) {
                removed += std::fs::read_dir(e.path()).map_or(0, |d| d.count());
                let _ = std::fs::remove_dir_all(e.path());
                continue;
            }
            let mut ids = BTreeSet::new();
            for f in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
                let n = f.file_name().to_string_lossy().into_owned();
                if let Some(id) = n.strip_suffix(".json").or_else(|| n.strip_suffix(".reads")) {
                    if is_key(id) {
                        ids.insert(id.to_string());
                    }
                }
            }
            if ids.len() <= MAX_ENTRIES {
                continue;
            }
            let mut keep: BTreeSet<&str> = ids
                .iter()
                .map(String::as_str)
                .filter(|id| used.contains(&(name.clone(), id.to_string())))
                .collect();
            for id in &ids {
                if keep.len() >= MAX_ENTRIES {
                    break;
                }
                keep.insert(id);
            }
            for id in ids.iter().filter(|id| !keep.contains(id.as_str())) {
                for ext in ["json", "reads"] {
                    if std::fs::remove_file(e.path().join(format!("{id}.{ext}"))).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        removed
    }
}

/// The file holding the run counter of a store.
const RUN_COUNTER: &str = ".sqldiff/run-counter";

impl Store {
    /// How many runs this store has completed (0 for none). Reads only: a
    /// run that stops early leaves the folder as it was.
    pub fn run_count(&self) -> u64 {
        crate::tree::dest_under(&self.out, RUN_COUNTER)
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| t.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// Records that run number `n` (a value of [`run_count`](Self::run_count))
    /// is complete. An unwritable counter reads as 0: the seed is then the
    /// same on every run, which is still deterministic.
    pub fn record_run(&self, n: u64) {
        let Ok(path) = crate::tree::dest_under(&self.out, RUN_COUNTER) else {
            return;
        };
        if let Some(dir) = path.parent() {
            if std::fs::create_dir_all(dir).is_ok() {
                let tmp = path.with_extension(format!("tmp{}", std::process::id()));
                if std::fs::write(&tmp, (n + 1).to_string()).is_ok()
                    && std::fs::rename(&tmp, &path).is_err()
                {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interned_strings_round_trip() {
        let long = "a note text that is long enough to be pooled ".repeat(2);
        let tricky = format!("{MARK}7 looks like a marker");
        let v = serde_json::json!({
            "a": [long, long, tricky, tricky, "short", "short"],
            "b": { "c": long, "d": format!("{MARK}") },
        });
        let (pool, packed) = intern(v.clone());
        assert_eq!(
            pool,
            vec![long.clone()],
            "only the repeated long string is pooled"
        );
        let mut back = packed;
        expand(&mut back, &pool).unwrap();
        assert_eq!(back, v);
        let mut bad = serde_json::json!([format!("{MARK}9")]);
        assert!(expand(&mut bad, &pool).is_none());
    }

    #[test]
    fn a_framed_file_is_read_back_only_whole() {
        let body = b"[1,2,3]";
        let f = framed(body);
        assert_eq!(verified_body(&f), Some(&body[..]));
        let mut edited = f.clone();
        *edited.last_mut().unwrap() = b'4';
        assert_eq!(verified_body(&edited), None, "edited body");
        assert_eq!(verified_body(&f[..40]), None, "truncated digest");
        assert_eq!(verified_body(&f[65..]), None, "no digest");
        assert_eq!(verified_body(b""), None);
    }

    #[test]
    fn stored_flags_with_a_wrong_digest_are_a_miss() {
        let out = std::env::temp_dir().join(format!("sqldiff-flags-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let store = Store::of(&out);
        let (r, k) = (
            EntryRef {
                key: Key("r".repeat(64)),
                id: "e".repeat(64),
            },
            Key("f".repeat(64)),
        );
        store.put_flags(&r, &k, &[]);
        assert!(store.get_flags(&r, &k).is_some_and(|v| v.is_empty()));
        let path = out.join(Store::flags_rel(&r, &k));
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(65);
        bytes.extend_from_slice(b"[]  "); // still valid JSON, not what was written
        std::fs::write(&path, bytes).unwrap();
        assert!(store.get_flags(&r, &k).is_none());
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn the_sample_reaches_the_largest_file() {
        // 99 small files and one huge one: the sample is drawn by count, so
        // the huge file is picked for some seeds and every file has the same
        // chance whatever its size.
        let mut present: Vec<(usize, usize)> = (0..99).map(|i| (i, 100)).collect();
        present.push((99, 1_000_000));
        let hits = (0..2000u64)
            .filter(|&s| pick(&present, s.wrapping_mul(0x9E37_79B9_7F4A_7C15)).contains(&99))
            .count();
        assert!(
            (10..=120).contains(&hits),
            "huge file picked in {hits} of 2000 runs"
        );
    }

    #[test]
    fn fields_do_not_run_together() {
        let key = |a: &str, b: &str| {
            let mut k = KeyBuilder::new();
            k.field("x", a.as_bytes());
            k.field("y", b.as_bytes());
            k.finish()
        };
        assert_ne!(key("ab", "c"), key("a", "bc"));
        assert_eq!(key("ab", "c"), key("ab", "c"));
        assert_eq!(key("a", "b").as_str().len(), 64);
    }

    #[test]
    fn the_sample_is_small_and_never_empty() {
        let files: Vec<(usize, usize)> = (0..100).map(|i| (i, 1000)).collect();
        for seed in 0..10 {
            assert_eq!(pick(&files, seed).len(), 2, "one in 50 of 100 files");
        }
        // Many tiny files and one huge one: the sample stays one in 50.
        let mut many: Vec<(usize, usize)> = (0..336).map(|i| (i, 300)).collect();
        many.push((336, 3_000_000));
        for seed in 0..10 {
            assert_eq!(pick(&many, seed).len(), 337usize.div_ceil(50));
        }
        // A single file is always re-checked.
        assert_eq!(pick(&[(5, 123)], 3), BTreeSet::from([5]));
        assert!(pick(&[], 1).is_empty());
    }
}

#[cfg(test)]
mod note_table_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unit_notes_are_written_once_and_read_back_in_order() {
        let a = json!({"status": "assumptions", "rule": "ASM-A", "text": "a"});
        let b = json!({"status": "converted", "rule": "ASM-B", "text": "b"});
        let original = json!({"units": [
            {"notes": [a, b, a]},
            {"notes": []},
            {"notes": [b, a]},
        ], "other": 1});
        let mut v = original.clone();
        share_notes(&mut v);
        assert_eq!(v[NOTE_TABLE].as_array().unwrap().len(), 2);
        assert_eq!(v["units"][0]["notes"], json!([0, 1, 0]));
        unshare_notes(&mut v).unwrap();
        assert_eq!(v, original);
        // An index outside the table is a miss, not a panic.
        let mut bad = json!({"units": [{"notes": [3]}], NOTE_TABLE: []});
        assert!(unshare_notes(&mut bad).is_none());
    }

    fn sample() -> Conversion {
        crate::convert("SELECT 1;\n", Dialect::Tsql, Dialect::Postgres).unwrap()
    }

    fn reads_of(n: u8) -> Reads {
        let mut r = Reads::default();
        r.0.insert(
            (ReadSide::Source, Query::Supplied),
            Outcome::of(&format!("answer {n}")),
        );
        r
    }

    fn scratch(name: &str) -> (PathBuf, Store) {
        let out = std::env::temp_dir().join(format!("sqldiff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let store = Store::of(&out);
        (out, store)
    }

    #[test]
    fn an_entry_reads_back_with_its_reads_and_a_second_write_of_other_bytes_is_refused() {
        let (out, store) = scratch("entries");
        let key = Key("a".repeat(64));
        let mut c = sample();
        c.output = "one".into();
        let (e, put) = store
            .put(&key, &mut c.clone(), None, true, &reads_of(1), false)
            .unwrap();
        assert_eq!(put, Put::Written);
        assert_eq!(store.entries(&key), vec![(e.id.clone(), reads_of(1))]);
        assert_eq!(store.get(&e).unwrap().0.output, "one");
        // The same bytes again: nothing to do.
        assert_eq!(
            store
                .put(&key, &mut c.clone(), None, true, &reads_of(1), false)
                .unwrap()
                .1,
            Put::Same
        );
        // Other bytes under the same reads: refused, and the entry stays.
        c.output = "two".into();
        assert_eq!(
            store
                .put(&key, &mut c.clone(), None, true, &reads_of(1), false)
                .unwrap()
                .1,
            Put::Differs
        );
        assert_eq!(store.get(&e).unwrap().0.output, "one");
        // A `--fresh` run writes over it.
        store
            .put(&key, &mut c.clone(), None, true, &reads_of(1), true)
            .unwrap();
        assert_eq!(store.get(&e).unwrap().0.output, "two");
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn a_reads_file_that_names_another_id_is_not_an_entry() {
        let (out, store) = scratch("reads-id");
        let key = Key("b".repeat(64));
        let (e, _) = store
            .put(&key, &mut sample(), None, true, &reads_of(1), false)
            .unwrap();
        let other = entry_id(&reads_of(2), true);
        let dir = out.join(DIR).join(&key.0);
        std::fs::copy(
            dir.join(format!("{}.reads", e.id)),
            dir.join(format!("{other}.reads")),
        )
        .unwrap();
        std::fs::copy(
            dir.join(format!("{}.json", e.id)),
            dir.join(format!("{other}.json")),
        )
        .unwrap();
        assert_eq!(store.entries(&key).len(), 1, "only the honest entry");
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn eviction_keeps_the_entries_this_run_used_then_fills_by_id() {
        let (out, store) = scratch("evict");
        let key = Key("c".repeat(64));
        let mut ids = Vec::new();
        for n in 0..(MAX_ENTRIES as u8 + 4) {
            let (e, _) = store
                .put(&key, &mut sample(), None, true, &reads_of(n), false)
                .unwrap();
            ids.push(e);
        }
        // Use the entries with the largest ids: a purely sorted cut would
        // drop exactly these.
        let mut sorted = ids.clone();
        sorted.sort();
        let used: BTreeSet<EntryRef> = sorted.iter().rev().take(3).cloned().collect();
        let live: BTreeSet<String> = [key.0.clone()].into();
        store.gc(&live, &used);
        let left: Vec<String> = store.entries(&key).into_iter().map(|e| e.0).collect();
        assert_eq!(left.len(), MAX_ENTRIES);
        for u in &used {
            assert!(left.contains(&u.id), "a used entry was evicted");
        }
        // The rest are the smallest ids, and the same on every run.
        let mut want: Vec<String> = sorted
            .iter()
            .take(MAX_ENTRIES - 3)
            .map(|e| e.id.clone())
            .collect();
        want.extend(used.iter().map(|u| u.id.clone()));
        want.sort();
        assert_eq!(left, want);
        let _ = std::fs::remove_dir_all(&out);
    }
}

#[cfg(test)]
mod template_key_tests {
    use super::*;

    fn base<'a>(helpers: &'a [Helper]) -> TemplateInput<'a> {
        TemplateInput {
            build: "b",
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            helpers,
            job: "job",
            draft: "draft",
            source: Some("s"),
            target: Some("t"),
        }
    }

    #[test]
    fn every_input_of_a_template_job_is_in_its_key() {
        let h: Vec<Helper> = Vec::new();
        let k = base(&h).key();
        assert_eq!(k, base(&h).key());
        let others = [
            TemplateInput {
                build: "c",
                ..base(&h)
            },
            TemplateInput {
                from: Dialect::Postgres,
                ..base(&h)
            },
            TemplateInput {
                to: Dialect::Db2i,
                ..base(&h)
            },
            TemplateInput {
                job: "job2",
                ..base(&h)
            },
            TemplateInput {
                draft: "draft2",
                ..base(&h)
            },
            TemplateInput {
                source: Some("s2"),
                ..base(&h)
            },
            TemplateInput {
                source: None,
                ..base(&h)
            },
            TemplateInput {
                target: Some("t2"),
                ..base(&h)
            },
            TemplateInput {
                target: None,
                ..base(&h)
            },
        ];
        for o in others {
            assert_ne!(k, o.key());
        }
    }
}

#[cfg(test)]
mod streamed_put_tests {
    use super::*;
    use crate::check::Mix;

    const SRC: &str = "\
CREATE PROCEDURE dbo.a1 @n INT AS BEGIN SELECT TOP (10) x FROM dbo.t WHERE x > @n ORDER BY x; END;
GO
CREATE PROCEDURE dbo.a2 AS BEGIN SELECT DATEADD(day, 1, GETDATE()) AS d, ISNULL(NULL, 1) AS e; END;
GO
CREATE PROCEDURE dbo.a3 AS BEGIN DECLARE @s NVARCHAR(40) = N'x'; SELECT LEN(@s) + 1 AS n; END;
GO
CREATE TABLE dbo.t (x INT NOT NULL, y DATETIME2 NULL);
GO
SELECT 1;
";

    fn converted(src: &str) -> Conversion {
        crate::convert(
            src,
            sqldiff_core::Dialect::Tsql,
            sqldiff_core::Dialect::Postgres,
        )
        .unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sqldiff-streamed-put-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Puts `c` and checks the file against the reference, the conversion
    /// against itself, and `get` against `c`.
    fn check_same(name: &str, mut c: Conversion, mix: Option<&Mix>, mix_tried: bool) {
        let out = scratch(name);
        let store = Store::of(&out);
        let key = Key(format!("{:0>64}", name.len()));
        let before = serde_json::to_vec(&c).unwrap();
        let (entry, _) = store
            .put(&key, &mut c, mix, mix_tried, &Reads::default(), false)
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&c).unwrap(),
            before,
            "{name}: c restored"
        );
        let want = framed(&reference_body(&key, &entry.id, &c, mix, mix_tried).unwrap());
        let got = std::fs::read(out.join(Store::entry_rel(&key, &entry.id))).unwrap();
        assert!(
            got == want,
            "{name}: the streamed file differs from the reference"
        );
        assert!(verified_body(&got).is_some());
        let (back, bmix, btried) = store.get(&entry).unwrap();
        assert!(compare(&back, &c), "{name}: round trip");
        assert_eq!((bmix.as_ref(), btried), (mix, mix_tried), "{name}: mix");
        let leftovers = std::fs::read_dir(out.join(Store::dir_rel(&key)))
            .unwrap()
            .count();
        // The result and its reads.
        assert_eq!(leftovers, 2, "{name}: no temporary file is left");
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn streamed_file_equals_the_reference_on_a_real_conversion() {
        let c = converted(SRC);
        assert!(
            c.units.iter().any(|u| !u.notes.is_empty()),
            "fixture has notes"
        );
        assert!(!c.report.findings.is_empty() || !c.output_check.findings.is_empty());
        let e: Entry = serde_json::from_slice(
            &reference_body(&Key("a".repeat(64)), "id", &c, None, false).unwrap(),
        )
        .unwrap();
        assert!(!e.pool.is_empty(), "fixture pools a repeated string");
        let table = e.conversion[NOTE_TABLE].as_array().unwrap().len();
        assert!(table > 0, "fixture has a note table");
        check_same("real", c.clone(), None, false);
        let mix = Mix {
            plain_at: vec![1, 3],
            draft: "a draft long enough to be pooled but it is not".into(),
        };
        check_same("mixed", c, Some(&mix), true);
    }

    #[test]
    fn streamed_file_equals_the_reference_on_pooled_and_marked_strings() {
        let mut c = converted(SRC);
        let long = "a repeated text that is long enough to be pooled".to_string();
        c.unproven = vec![
            long.clone(),
            long.clone(),
            format!("{MARK}0 starts with the marker"),
            format!("{MARK}{MARK}twice"),
            format!("{MARK}"),
        ];
        c.output = format!("{MARK}{long}");
        c.report.unknown_reasons = vec![long.clone(), "short".into(), long.clone()];
        c.output_check.normalizations = vec![long.clone()];
        let extra = c.units.last().cloned().unwrap();
        for _ in 0..3 {
            let mut u = extra.clone();
            if let Some(n) = u.notes.first().cloned() {
                u.notes = vec![n.clone(), n];
            }
            c.units.push(u);
        }
        check_same("marked", c.clone(), None, true);
        // Findings that repeat a long text across both finding arrays.
        if let Some(f) = c.report.findings.first().cloned() {
            c.output_check.findings.push(f.clone());
            c.output_check.findings.push(f);
        }
        check_same("shared-findings", c, None, false);
    }

    #[test]
    fn streamed_file_equals_the_reference_on_empty_arrays() {
        check_same("empty-source", converted(""), None, false);
        let mut c = converted(SRC);
        c.units.clear();
        c.report.findings.clear();
        c.output_check.findings.clear();
        check_same("emptied", c.clone(), None, false);
        c.template = None;
        c.unplaced = vec![0, 2];
        check_same("emptied-unplaced", c, None, true);
    }

    #[test]
    fn put_restores_the_conversion_when_it_fails() {
        let mut c = converted(SRC);
        let before = serde_json::to_vec(&c).unwrap();
        let out = scratch("fails");
        // A file where the results directory should be: create_dir_all fails.
        std::fs::create_dir_all(out.join(".sqldiff")).unwrap();
        std::fs::write(out.join(DIR), b"in the way").unwrap();
        let key = Key("c".repeat(64));
        assert!(Store::of(&out)
            .put(&key, &mut c, None, false, &Reads::default(), false)
            .is_err());
        assert_eq!(serde_json::to_vec(&c).unwrap(), before);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Puts `c` and returns the store, its entry, the path of the file and the
    /// output directory.
    fn stored(name: &str, c: &Conversion) -> (Store, EntryRef, PathBuf, PathBuf) {
        let out = scratch(name);
        let store = Store::of(&out);
        let key = Key(format!("{:0>64}", name.len() + 40));
        let (entry, _) = store
            .put(&key, &mut c.clone(), None, true, &Reads::default(), false)
            .unwrap();
        let path = out.join(Store::entry_rel(&key, &entry.id));
        (store, entry, path, out)
    }

    /// A conversion with units and notes, pooled and marked strings.
    fn fixture_with_notes() -> Conversion {
        let mut c = converted(SRC);
        let long = "a repeated text that is long enough to be pooled".to_string();
        c.unproven = vec![long.clone(), long.clone(), format!("{MARK}0 marker")];
        c.output = format!("{MARK}{long}");
        let extra = c.units.last().cloned().unwrap();
        for _ in 0..3 {
            let mut u = extra.clone();
            if let Some(n) = u.notes.first().cloned() {
                u.notes = vec![n.clone(), n];
            }
            c.units.push(u);
        }
        assert!(c.units.iter().any(|u| !u.notes.is_empty()));
        assert!(!c.report.findings.is_empty() || !c.output_check.findings.is_empty());
        c
    }

    #[test]
    fn get_without_units_is_get_with_the_units_cleared() {
        for (name, c) in [
            ("lean-real", converted(SRC)),
            ("lean-pooled", fixture_with_notes()),
            ("lean-empty", converted("")),
        ] {
            let (store, key, _, out) = stored(name, &c);
            let (full, ..) = store.get(&key).unwrap();
            assert!(compare(&full, &c), "{name}: get still returns everything");
            let (lean, mix, tried) = store.get_without_units(&key).unwrap();
            assert!(lean.units.is_empty(), "{name}");
            assert_eq!((mix, tried), (None, true), "{name}");
            let mut want = c.clone();
            want.units = Vec::new();
            assert!(compare(&lean, &want), "{name}: only units differ");
            let _ = std::fs::remove_dir_all(&out);
        }
    }

    #[test]
    fn get_without_units_does_not_parse_the_units() {
        let c = fixture_with_notes();
        let (store, key, path, out) = stored("lean-invalid", &c);
        let mut v: serde_json::Value =
            serde_json::from_slice(verified_body(&std::fs::read(&path).unwrap()).unwrap()).unwrap();
        v["conversion"]["units"] = serde_json::json!([{"status": "no such status"}, 7]);
        std::fs::write(&path, framed(&serde_json::to_vec(&v).unwrap())).unwrap();
        assert!(store.get(&key).is_none(), "get parses the units");
        let (lean, ..) = store.get_without_units(&key).unwrap();
        assert!(lean.units.is_empty());
        let mut want = c;
        want.units = Vec::new();
        assert!(compare(&lean, &want));
        // Without either skipped key, both reads miss.
        let mut no_units = v.clone();
        no_units["conversion"]
            .as_object_mut()
            .unwrap()
            .remove("units");
        std::fs::write(&path, framed(&serde_json::to_vec(&no_units).unwrap())).unwrap();
        assert!(store.get(&key).is_none() && store.get_without_units(&key).is_none());
        let mut no_table = v;
        no_table["conversion"]
            .as_object_mut()
            .unwrap()
            .remove(NOTE_TABLE);
        std::fs::write(&path, framed(&serde_json::to_vec(&no_table).unwrap())).unwrap();
        assert!(store.get(&key).is_none() && store.get_without_units(&key).is_none());
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn get_without_units_checks_the_digest_and_the_key() {
        let c = fixture_with_notes();
        let (store, key, path, out) = stored("lean-digest", &c);
        let good = std::fs::read(&path).unwrap();
        assert!(store.get_without_units(&key).is_some());
        // A flipped byte in the body does not match the digest.
        let mut bad = good.clone();
        let at = bad.len() - 5;
        bad[at] ^= 1;
        std::fs::write(&path, &bad).unwrap();
        assert!(store.get(&key).is_none() && store.get_without_units(&key).is_none());
        // Valid JSON and digest, but another key inside.
        let mut v: serde_json::Value =
            serde_json::from_slice(verified_body(&good).unwrap()).unwrap();
        v["key"] = serde_json::json!("f".repeat(64));
        std::fs::write(&path, framed(&serde_json::to_vec(&v).unwrap())).unwrap();
        assert!(store.get(&key).is_none() && store.get_without_units(&key).is_none());
        // A missing file.
        std::fs::remove_file(&path).unwrap();
        assert!(store.get(&key).is_none() && store.get_without_units(&key).is_none());
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn peek_reads_the_mix_and_checks_the_frame_and_the_key() {
        let c = fixture_with_notes();
        let (store, key, path, out) = stored("peek", &c);
        assert_eq!(store.peek(&key), Some((None, true)));
        let good = std::fs::read(&path).unwrap();
        // The same checks as `get` makes outside the conversion.
        let mut bad = good.clone();
        let at = bad.len() - 5;
        bad[at] ^= 1;
        std::fs::write(&path, &bad).unwrap();
        assert!(store.peek(&key).is_none() && store.get(&key).is_none());
        let mut v: serde_json::Value =
            serde_json::from_slice(verified_body(&good).unwrap()).unwrap();
        let mut other = v.clone();
        other["key"] = serde_json::json!("f".repeat(64));
        std::fs::write(&path, framed(&serde_json::to_vec(&other).unwrap())).unwrap();
        assert!(store.peek(&key).is_none() && store.get(&key).is_none());
        // A conversion `get` cannot read passes `peek`: the caller reads it
        // after the check and reports the difference.
        v["conversion"]["units"] = serde_json::json!([{"status": "no such status"}]);
        std::fs::write(&path, framed(&serde_json::to_vec(&v).unwrap())).unwrap();
        assert!(store.get(&key).is_none());
        assert_eq!(store.peek(&key), Some((None, true)));
        std::fs::remove_file(&path).unwrap();
        assert!(store.peek(&key).is_none());
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn compare_is_byte_exact() {
        let c = fixture_with_notes();
        assert!(compare(&c, &c.clone()));
        let mut d = c.clone();
        d.output.push('x');
        assert!(!compare(&c, &d) && !compare(&d, &c), "a longer side");
        let mut d = c.clone();
        d.output = format!("y{}", &c.output[1..]);
        assert!(!compare(&c, &d), "the same length, one byte");
        let mut d = c.clone();
        d.units.pop();
        assert!(!compare(&c, &d) && !compare(&d, &c), "a unit fewer");
        let mut d = c.clone();
        d.unproven.push("last field".into());
        assert!(!compare(&c, &d) && !compare(&d, &c));
    }

    /// The runtime re-check proves the read path on the sample; this proves
    /// it on the corpus: what `get` gives back serialises as what was put.
    #[test]
    fn get_of_put_serialises_as_the_conversion_for_the_corpus() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let mut files = Vec::new();
        for dir in ["pairs", "mutations"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                for f in std::fs::read_dir(e.path()).unwrap().flatten() {
                    let n = f.file_name().to_string_lossy().into_owned();
                    if n == "tsql.sql" || n.ends_with(".tsql.sql") {
                        files.push(f.path());
                    }
                }
            }
        }
        files.push(root.join("catalog/tsql.sql"));
        files.sort();
        assert!(files.len() > 20);
        let out = scratch("corpus-round-trip");
        let store = Store::of(&out);
        for (i, p) in files.iter().enumerate() {
            let src = std::fs::read_to_string(p).unwrap();
            let mut c = converted(&src);
            let key = Key(format!("{i:0>64}"));
            let (entry, _) = store
                .put(&key, &mut c, None, i % 2 == 0, &Reads::default(), false)
                .unwrap();
            let (back, mix, tried) = store.get(&entry).unwrap();
            assert_eq!((mix, tried), (None, i % 2 == 0));
            assert!(
                serde_json::to_vec(&back).unwrap() == serde_json::to_vec(&c).unwrap(),
                "{}: get(put(c)) differs from c",
                p.display()
            );
            assert!(compare(&back, &c), "{}", p.display());
        }
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Changes that a fresh check could differ by: one element, an element
    /// more or fewer, a small field, in each of the three big arrays.
    fn variants(c: &Conversion) -> Vec<(&'static str, Conversion)> {
        let mut v: Vec<(&'static str, Conversion)> = vec![("same", c.clone())];
        let mut add = |name: &'static str, f: &dyn Fn(&mut Conversion)| {
            let mut d = c.clone();
            f(&mut d);
            v.push((name, d));
        };
        add("unit changed", &|d| {
            if let Some(u) = d.units.first_mut() {
                u.out.end_line += 1;
            }
        });
        add("last unit changed", &|d| {
            if let Some(u) = d.units.last_mut() {
                u.src.line += 1;
            }
        });
        add("unit removed", &|d| {
            d.units.pop();
        });
        add("unit added", &|d| {
            if let Some(u) = d.units.first().cloned() {
                d.units.push(u);
            }
        });
        add("units cleared", &|d| d.units.clear());
        add("note changed", &|d| {
            if let Some(n) = d.units.iter_mut().find_map(|u| u.notes.first_mut()) {
                n.rule = format!("{}x", n.rule).into();
            }
        });
        add("report finding changed", &|d| {
            if let Some(f) = d.report.findings.first_mut() {
                f.fingerprint.push('x');
            }
        });
        add("report finding removed", &|d| {
            d.report.findings.pop();
        });
        add("report finding added", &|d| {
            if let Some(f) = d.report.findings.first().cloned() {
                d.report.findings.push(f);
            }
        });
        add("output finding changed", &|d| {
            if let Some(f) = d.output_check.findings.last_mut() {
                f.fingerprint.push('x');
            }
        });
        add("output finding removed", &|d| {
            d.output_check.findings.pop();
        });
        add("output finding added", &|d| {
            if let Some(f) = d.output_check.findings.first().cloned() {
                d.output_check.findings.push(f);
            }
        });
        add("output text", &|d| d.output.push('x'));
        add("counts", &|d| d.counts.converted += 1);
        add("report field", &|d| d.report.tool_version.push('x'));
        add("output_check field", &|d| {
            d.output_check.normalizations.push("x".into())
        });
        add("unproven", &|d| d.unproven.push("x".into()));
        add("unplaced", &|d| d.unplaced.push(0));
        add("from", &|d| d.from.push('x'));
        v
    }

    /// The streamed comparison and the whole one (`compare(get(..), ..)`)
    /// agree, for the corpus and for every variant of it, and the fresh
    /// conversion is whole afterwards.
    #[test]
    fn the_streamed_compare_agrees_with_the_whole_one() {
        let mut cs = vec![
            ("src", converted(SRC)),
            ("notes", fixture_with_notes()),
            ("empty", converted("")),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        for dir in ["pairs", "mutations"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
                let f = e.path().join("tsql.sql");
                if f.is_file() {
                    cs.push(("corpus", converted(&std::fs::read_to_string(f).unwrap())));
                }
            }
        }
        let mut different = 0;
        for (i, (name, c)) in cs.iter().enumerate() {
            let (store, key, _, out) = stored(&format!("stream-{i}"), c);
            let (whole, ..) = store.get(&key).unwrap();
            for (what, mut fresh) in variants(c) {
                let before = serde_json::to_vec(&fresh).unwrap();
                let want = compare(&whole, &fresh);
                let got = store.compare_streamed(&key, &mut fresh);
                assert_eq!(
                    serde_json::to_vec(&fresh).unwrap(),
                    before,
                    "{name} {what}: fresh restored"
                );
                assert_eq!(
                    got,
                    if want {
                        Streamed::Equal
                    } else {
                        Streamed::Different
                    },
                    "{name} {what}"
                );
                different += usize::from(!want);
            }
            let _ = std::fs::remove_dir_all(&out);
        }
        assert!(different > 20, "the variants change something: {different}");
    }

    /// Re-signs `path` after `f` changed its body text, which keeps the
    /// order of keys `put` writes.
    fn edit_body(path: &Path, f: impl Fn(&str) -> String) {
        let raw = std::fs::read(path).unwrap();
        let body = String::from_utf8(verified_body(&raw).unwrap().to_vec()).unwrap();
        let new = f(&body);
        assert_ne!(new, body, "the edit changed nothing");
        std::fs::write(path, framed(new.as_bytes())).unwrap();
    }

    #[test]
    fn a_tampered_stored_result_is_different_or_undecided_never_equal() {
        let c = fixture_with_notes();
        let (store, key, path, out) = stored("stream-stored", &c);
        let mut fresh = c.clone();
        assert_eq!(store.compare_streamed(&key, &mut fresh), Streamed::Equal);
        assert_eq!(store.recheck(&key, &mut fresh, false), Rechecked::Same);
        // A mix that differs is a mismatch whatever the conversions say.
        assert_eq!(store.recheck(&key, &mut fresh, true), Rechecked::Differs);
        let good = std::fs::read(&path).unwrap();
        // One element, a small field, a length, each in the stored file.
        for (what, f) in [
            (
                "a unit",
                Box::new(|b: &str| b.replacen("\"end_line\":", "\"end_line\":9", 1))
                    as Box<dyn Fn(&str) -> String>,
            ),
            (
                "a small field",
                Box::new(|b| b.replacen("\"from\":\"", "\"from\":\"x", 1)),
            ),
            (
                "an array",
                Box::new(|b| b.replacen("\"units\":[", "\"units\":[null,", 1)),
            ),
        ] {
            std::fs::write(&path, &good).unwrap();
            edit_body(&path, f);
            let got = store.compare_streamed(&key, &mut fresh);
            assert_ne!(got, Streamed::Equal, "{what}");
            assert_eq!(
                store.recheck(&key, &mut fresh, false),
                if store.get(&key).is_some() {
                    Rechecked::Differs
                } else {
                    Rechecked::Unread
                },
                "{what}"
            );
        }
        // A unit that is another unit: Different, and it reads back.
        std::fs::write(&path, &good).unwrap();
        edit_body(&path, |b| b.replacen("\"end_line\":", "\"end_line\":9", 1));
        assert_eq!(
            store.compare_streamed(&key, &mut fresh),
            Streamed::Different
        );
        assert!(store.get(&key).is_some());
        // The order of keys of a file written through a `Value`: not
        // followed, and equal when read whole.
        std::fs::write(&path, &good).unwrap();
        let v: serde_json::Value = serde_json::from_slice(verified_body(&good).unwrap()).unwrap();
        std::fs::write(&path, framed(&serde_json::to_vec(&v).unwrap())).unwrap();
        assert_eq!(
            store.compare_streamed(&key, &mut fresh),
            Streamed::Undecided
        );
        assert_eq!(store.recheck(&key, &mut fresh, false), Rechecked::Same);
        // A unit that does not decode: undecided, and a mismatch.
        std::fs::write(&path, &good).unwrap();
        edit_body(&path, |b| b.replacen("\"status\":\"", "\"status\":\"zz", 1));
        assert_eq!(
            store.compare_streamed(&key, &mut fresh),
            Streamed::Undecided
        );
        assert!(store.get(&key).is_none());
        assert_eq!(store.recheck(&key, &mut fresh, false), Rechecked::Unread);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// `Value` objects are written in key order only while serde_json keeps
    /// `Map` a `BTreeMap` (no `preserve_order`). The streamed put writes the
    /// objects of `conversion` in byte order itself and relies on that.
    #[test]
    fn the_reference_values_have_sorted_keys() {
        fn sorted(v: &serde_json::Value) -> bool {
            match v {
                serde_json::Value::Object(o) => {
                    let ks: Vec<&String> = o.keys().collect();
                    ks.windows(2).all(|w| w[0] < w[1]) && o.values().all(sorted)
                }
                serde_json::Value::Array(a) => a.iter().all(sorted),
                _ => true,
            }
        }
        let mut m = serde_json::Map::new();
        for k in ["b", "a", "c"] {
            m.insert(k.to_string(), serde_json::Value::Null);
        }
        assert_eq!(m.keys().collect::<Vec<_>>(), ["a", "b", "c"]);
        let mut v = serde_json::to_value(converted(SRC)).unwrap();
        share_notes(&mut v);
        assert!(
            sorted(&v),
            "enabling preserve_order would reorder the stored bytes"
        );
        assert!(v.as_object().unwrap().contains_key(NOTE_TABLE));
    }
}
