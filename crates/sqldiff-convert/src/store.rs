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
//! `sqldiff-core`. Keys are SHA-256, and a result is a file named by its key
//! under `OUT/.sqldiff/results`, written to a temporary name and renamed
//! into place, so a run killed at any moment leaves only whole results.
//! Two runs writing the same key write the same bytes (no timings are stored),
//! so no lock is needed for the results themselves.
//!
//! Every run re-checks a sample of the results it would reuse and compares
//! them byte for byte with the stored ones (`pick`, `compare`): a mismatch
//! writes a diagnostic bundle and empties the store before the run fails.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_core::catalog::builtins::Helper;
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
    /// [`catalog_digest`] of the source and the target catalog the check
    /// binds against, when the run uses catalogs.
    pub source: Option<&'a str>,
    pub target: Option<&'a str>,
}

impl CheckInput<'_> {
    pub fn key(&self) -> Key {
        let mut k = KeyBuilder::new();
        k.field("format", b"4");
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
        k.field("source", self.source.unwrap_or("none").as_bytes());
        k.field("target", self.target.unwrap_or("none").as_bytes());
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
    c: &Conversion,
    mix: Option<&crate::check::Mix>,
    mix_tried: bool,
) -> Result<Vec<u8>, String> {
    let mut value = serde_json::to_value(c).map_err(|e| e.to_string())?;
    share_notes(&mut value);
    let (pool, conversion) = intern(value);
    serde_json::to_vec(&Entry {
        key: key.0.clone(),
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

    fn rel(key: &Key) -> String {
        format!("{DIR}/{}.json", key.0)
    }

    /// Whether a result is stored under `key` (it may still not read back).
    pub fn has(&self, key: &Key) -> bool {
        crate::tree::dest_under(&self.out, &Self::rel(key)).is_ok_and(|p| p.is_file())
    }

    /// The stored conversion. A file that does not read back whole, or does
    /// not name its own key, is a miss.
    pub fn get(&self, key: &Key) -> Option<(Conversion, Option<crate::check::Mix>, bool)> {
        let path = crate::tree::dest_under(&self.out, &Self::rel(key)).ok()?;
        let raw = std::fs::read(path).ok()?;
        let body = verified_body(&raw)?;
        let mut e: Entry = serde_json::from_slice(body).ok()?;
        if e.key != key.0 {
            return None;
        }
        expand(&mut e.conversion, &e.pool)?;
        unshare_notes(&mut e.conversion)?;
        let conversion = serde_json::from_value(e.conversion).ok()?;
        Some((conversion, e.mix, e.mix_tried))
    }

    /// Stores a result: written beside its name and renamed into place,
    /// retried a few times because a virus scanner on Windows can hold the
    /// file for a moment.
    ///
    /// The file is `framed` of `reference_body`, byte for byte, but the big
    /// arrays of the conversion (`units`, `report.findings`,
    /// `output_check.findings`) are never one `Value` or one buffer. They are
    /// taken out of `c` for the call and put back on every path, a panic
    /// included, by [`Taken`]; the small rest is serialised once; the arrays
    /// are written element by element into the temporary file through a
    /// hashing writer. Pass 1 (the table of notes and the pool counts, which
    /// the head of the file needs) finishes before any byte is written. `c`
    /// is borrowed mutably only for that take and restore.
    pub fn put(
        &self,
        key: &Key,
        c: &mut Conversion,
        mix: Option<&crate::check::Mix>,
        mix_tried: bool,
    ) -> Result<(), String> {
        let taken = Taken::new(c);
        let plan = Plan::new(&taken)?;
        let dest = crate::tree::dest_under(&self.out, &Self::rel(key))?;
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let tmp = dest.with_extension(format!(
            "tmp{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        if let Err(e) = write_framed(&tmp, &key.0, plan, &taken, mix, mix_tried) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        drop(taken);
        let mut last = String::new();
        for attempt in 0..5u64 {
            match std::fs::rename(&tmp, &dest) {
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
    /// program, which no later run of this tree can use. Files that are not
    /// results are left alone, except a temporary file of a killed write
    /// older than an hour. Returns how many were removed.
    pub fn gc(&self, live: &std::collections::BTreeSet<String>) -> usize {
        let Ok(dir) = crate::tree::dest_under(&self.out, DIR) else {
            return 0;
        };
        let mut removed = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".json") else {
                // A write killed before its rename leaves `<key>.tmp<pid>-…`;
                // one that old is no write in flight.
                let stale = name.split_once(".tmp").is_some_and(|(k, _)| is_key(k))
                    && e.metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > STALE_TMP);
                if stale && std::fs::remove_file(e.path()).is_ok() {
                    removed += 1;
                }
                continue;
            };
            if is_key(key) && !live.contains(key) && std::fs::remove_file(e.path()).is_ok() {
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

    /// The key of the flags of file `fi` for the result `result`: the flags
    /// are a function of the result, the stand-ins, the file's place in the
    /// tree and the dialects ([`crate::refused::StandIns::flags_of`]).
    pub fn flags_key(
        result: &Key,
        stand_ins_digest: &str,
        fi: usize,
        from: Dialect,
        to: Dialect,
    ) -> Key {
        let mut k = KeyBuilder::new();
        k.field("format", b"flags-2");
        k.field("result", result.0.as_bytes());
        k.field("stand-ins", stand_ins_digest.as_bytes());
        k.field("file", &(fi as u64).to_le_bytes());
        k.field("from", from.id().as_bytes());
        k.field("to", to.id().as_bytes());
        k.finish()
    }

    fn flags_rel(result: &Key, flags: &Key) -> String {
        format!("{FLAGS_DIR}/{}/{}.json", result.0, flags.0)
    }

    /// The stored flags, or a miss (also for a file that does not read back
    /// whole).
    pub fn get_flags(&self, result: &Key, flags: &Key) -> Option<Vec<crate::refused::UnitFlag>> {
        let path = crate::tree::dest_under(&self.out, &Self::flags_rel(result, flags)).ok()?;
        let raw = std::fs::read(path).ok()?;
        serde_json::from_slice(verified_body(&raw)?).ok()
    }

    /// Stores flags (best effort: a flag set that is not stored is computed
    /// again next time).
    pub fn put_flags(&self, result: &Key, flags: &Key, v: &[crate::refused::UnitFlag]) {
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
        result: &Key,
        flags: &Key,
        stored: &[crate::refused::UnitFlag],
        fresh: &[crate::refused::UnitFlag],
    ) {
        let rel = format!("{MISMATCH_DIR}/flags-{}.json", flags.0);
        let body = serde_json::json!({
            "result": result.0,
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
        let (r, k) = (Key("r".repeat(64)), Key("f".repeat(64)));
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
        store.put(&key, &mut c, mix, mix_tried).unwrap();
        assert_eq!(
            serde_json::to_vec(&c).unwrap(),
            before,
            "{name}: c restored"
        );
        let want = framed(&reference_body(&key, &c, mix, mix_tried).unwrap());
        let got = std::fs::read(out.join(Store::rel(&key))).unwrap();
        assert!(
            got == want,
            "{name}: the streamed file differs from the reference"
        );
        assert!(verified_body(&got).is_some());
        let (back, bmix, btried) = store.get(&key).unwrap();
        assert!(compare(&back, &c), "{name}: round trip");
        assert_eq!((bmix.as_ref(), btried), (mix, mix_tried), "{name}: mix");
        let leftovers = std::fs::read_dir(out.join(DIR)).unwrap().count();
        assert_eq!(leftovers, 1, "{name}: no temporary file is left");
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
        let e: Entry =
            serde_json::from_slice(&reference_body(&Key("a".repeat(64)), &c, None, false).unwrap())
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
        assert!(Store::of(&out).put(&key, &mut c, None, false).is_err());
        assert_eq!(serde_json::to_vec(&c).unwrap(), before);
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
