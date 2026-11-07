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
/// The rules' drafts of the files of one tree, one file each under a folder
/// named by the key of the tree, written as pass 1 goes. They survive a run
/// killed in pass 1 and are dropped once the whole tree state is stored.
pub const DRAFTS_DIR: &str = ".sqldiff/drafts";

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

/// What a draft file holds.
#[derive(Deserialize)]
struct DraftEntry {
    key: String,
    rel: String,
    draft: crate::Draft,
}

#[derive(Serialize)]
struct DraftEntryRef<'a> {
    key: &'a str,
    rel: &'a str,
    draft: &'a crate::Draft,
}

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

    /// The stored conversion of one entry. A file that does not read back
    /// whole, or does not name its own key and id, is a miss.
    pub fn get(&self, e: &EntryRef) -> Option<(Conversion, Option<crate::check::Mix>, bool)> {
        let path = crate::tree::dest_under(&self.out, &Self::entry_rel(&e.key, &e.id)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let body = verified_body(&raw)?;
        let mut x: Entry = serde_json::from_slice(body).ok()?;
        if x.key != e.key.0 || x.id != e.id {
            return None;
        }
        expand(&mut x.conversion, &x.pool)?;
        unshare_notes(&mut x.conversion)?;
        let conversion = serde_json::from_value(x.conversion).ok()?;
        Some((conversion, x.mix, x.mix_tried))
    }

    /// Stores a result under the reads its check made. The entry is written
    /// beside its name and renamed into place, retried a few times because a
    /// virus scanner on Windows can hold the file for a moment; the reads
    /// follow, so a reader that sees the reads sees the result. An entry
    /// already stored under the same id is left as it is: a second check
    /// with the same reads must give the same bytes, and `Put::Differs`
    /// reports that it did not.
    pub fn put(
        &self,
        key: &Key,
        c: &Conversion,
        mix: Option<&crate::check::Mix>,
        mix_tried: bool,
        reads: &Reads,
    ) -> Result<(EntryRef, Put), String> {
        let id = entry_id(reads, mix_tried);
        let mut value = serde_json::to_value(c).map_err(|e| e.to_string())?;
        share_notes(&mut value);
        let (pool, conversion) = intern(value);
        let body = serde_json::to_vec(&Entry {
            key: key.0.clone(),
            id: id.clone(),
            pool,
            conversion,
            mix: mix.cloned(),
            mix_tried,
        })
        .map_err(|e| e.to_string())?;
        let bytes = framed(&body);
        let entry = EntryRef {
            key: key.clone(),
            id: id.clone(),
        };
        let dest = crate::tree::dest_under(&self.out, &Self::entry_rel(key, &id))?;
        let put = match std::fs::read(&dest) {
            Ok(old) if verified_body(&old).is_some() => {
                if old == bytes {
                    Put::Same
                } else {
                    return Ok((entry, Put::Differs));
                }
            }
            _ => {
                write_atomically(&dest, &bytes)?;
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
        for d in [DIR, TREE_DIR, DRAFTS_DIR, FLAGS_DIR] {
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

    fn draft_rel(key: &TreeKey, rel: &str) -> String {
        let h = Sha256::digest(rel.as_bytes());
        let name: String = h.iter().map(|b| format!("{b:02x}")).collect();
        format!("{DRAFTS_DIR}/{}/{name}.json", key.0)
    }

    /// The stored draft of file `rel` of the tree `key`. A file that does
    /// not read back whole, or does not name its own tree and path, is a
    /// miss.
    pub fn get_draft(&self, key: &TreeKey, rel: &str) -> Option<crate::Draft> {
        let path = crate::tree::dest_under(&self.out, &Self::draft_rel(key, rel)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let e: DraftEntry = serde_json::from_slice(verified_body(&raw)?).ok()?;
        (e.key == key.0 && e.rel == rel).then_some(e.draft)
    }

    /// Stores the draft of file `rel` of the tree `key`, whole or not at
    /// all. Another tree's drafts are dropped first. Failing to store is
    /// not an error: the run builds the draft itself.
    pub fn put_draft(&self, key: &TreeKey, rel: &str, draft: &crate::Draft) {
        let e = DraftEntryRef {
            key: &key.0,
            rel,
            draft,
        };
        let Ok(body) = serde_json::to_vec(&e) else {
            return;
        };
        let bytes = framed(&body);
        let Ok(dest) = crate::tree::dest_under(&self.out, &Self::draft_rel(key, rel)) else {
            return;
        };
        let Some(dir) = dest.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = dest.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Drops the drafts of every tree except `key`'s (a tree that changed
    /// leaves nothing of use behind).
    pub fn keep_drafts_of(&self, key: &TreeKey) {
        let Ok(root) = crate::tree::dest_under(&self.out, DRAFTS_DIR) else {
            return;
        };
        for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy() != key.0 {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    /// Drops every stored draft, and the `.sqldiff` folder if that leaves it
    /// empty.
    pub fn drop_drafts(&self) {
        if let Ok(p) = crate::tree::dest_under(&self.out, DRAFTS_DIR) {
            let _ = std::fs::remove_dir_all(&p);
            if let Some(d) = p.parent() {
                let _ = std::fs::remove_dir(d);
            }
        }
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

    /// Writes what a mismatch found, for whoever looks into it.
    pub fn bundle(&self, key: &Key, stored: &Conversion, fresh: &Conversion) {
        let rel = format!("{MISMATCH_DIR}/{}.json", key.0);
        let body = serde_json::json!({ "key": key.0, "stored": stored, "fresh": fresh });
        if let Ok(b) = serde_json::to_vec_pretty(&body) {
            let _ = crate::tree::write_under(&self.out, &rel, &b);
        }
    }
}

/// Whether a reused result equals a fresh check of the same input, byte for
/// byte as it serialises.
pub fn compare(stored: &Conversion, fresh: &Conversion) -> bool {
    serde_json::to_vec(stored).ok() == serde_json::to_vec(fresh).ok()
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
}
