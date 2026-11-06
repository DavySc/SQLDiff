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
    /// The running executable's hash: another build may check differently.
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
        k.field("format", b"2");
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
}

/// A string at least this long that occurs twice goes to the pool.
const POOL_MIN: usize = 40;
/// Starts a marker. A real string starting with it is written with one more.
const MARK: char = '\u{1}';

fn count(v: &serde_json::Value, seen: &mut std::collections::HashMap<String, usize>) {
    match v {
        serde_json::Value::String(s) if s.len() >= POOL_MIN => {
            *seen.entry(s.clone()).or_default() += 1;
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| count(x, seen)),
        serde_json::Value::Object(o) => o.values().for_each(|x| count(x, seen)),
        _ => {}
    }
}

fn replace(v: &mut serde_json::Value, index: &std::collections::HashMap<&str, usize>) {
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
    let mut seen = std::collections::HashMap::new();
    count(&v, &mut seen);
    let mut pool: Vec<String> = seen
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(s, _)| s)
        .collect();
    pool.sort();
    let index: std::collections::HashMap<&str, usize> = pool
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    replace(&mut v, &index);
    (pool, v)
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
    pub fn get(&self, key: &Key) -> Option<Conversion> {
        let path = crate::tree::dest_under(&self.out, &Self::rel(key)).ok()?;
        let mut e: Entry = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        if e.key != key.0 {
            return None;
        }
        expand(&mut e.conversion, &e.pool)?;
        serde_json::from_value(e.conversion).ok()
    }

    /// Stores a result: written beside its name and renamed into place,
    /// retried a few times because a virus scanner on Windows can hold the
    /// file for a moment.
    pub fn put(&self, key: &Key, c: &Conversion) -> Result<(), String> {
        let (pool, conversion) = intern(serde_json::to_value(c).map_err(|e| e.to_string())?);
        let bytes = serde_json::to_vec(&Entry {
            key: key.0.clone(),
            pool,
            conversion,
        })
        .map_err(|e| e.to_string())?;
        let dest = crate::tree::dest_under(&self.out, &Self::rel(key))?;
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let tmp = dest.with_extension(format!(
            "tmp{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
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
        if let Ok(p) = crate::tree::dest_under(&self.out, DIR) {
            let _ = std::fs::remove_dir_all(p);
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

/// Which stored results to re-check this run: two samples, joined.
///
/// - By bytes: about 2% of the stored files' bytes, small files first from a
///   start that differs from run to run. It re-checks many files cheaply.
/// - By count: one file in 50 (at least one), drawn uniformly whatever its
///   size. A file's chance of being re-checked is therefore the same on every
///   run, however large it is, so the biggest files are covered too; the
///   expected cost is 2% of the bytes, the worst case is the largest files.
///
/// `present` is `(index, bytes)` of every file whose result is stored.
pub fn pick(present: &[(usize, usize)], seed: u64) -> BTreeSet<usize> {
    let mut by_size: Vec<(usize, usize)> = present.iter().map(|&(i, b)| (b, i)).collect();
    by_size.sort();
    let n = by_size.len();
    let mut out = BTreeSet::new();
    if n == 0 {
        return out;
    }
    let total: usize = by_size.iter().map(|x| x.0).sum();
    let budget = (total / 50).max(by_size[0].0);
    let mut spent = 0usize;
    let start = (seed % n as u64) as usize;
    for k in 0..n {
        let (bytes, i) = by_size[(start + k) % n];
        if spent + bytes <= budget {
            spent += bytes;
            out.insert(i);
        }
        if spent >= budget {
            break;
        }
    }
    // By count: rank the files by a hash of (seed, index) and take the first.
    let mut ranked: Vec<(u64, usize)> = present
        .iter()
        .map(|&(i, _)| (mix(seed, i as u64), i))
        .collect();
    ranked.sort();
    for &(_, i) in ranked.iter().take(n.div_ceil(50)) {
        out.insert(i);
    }
    out
}

/// A splitmix64 step over `seed` and `i`.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed.wrapping_add(i.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A different number on every run.
pub fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
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
    fn the_sample_reaches_the_largest_file() {
        // 99 small files and one huge one: the byte sample can never take the
        // huge one, the count sample does for some seeds, and every file has
        // the same chance whatever its size.
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
            let s = pick(&files, seed);
            assert!(
                (2..=4).contains(&s.len()),
                "2% by bytes plus 1 in 50: {s:?}"
            );
        }
        // The byte sample alone never takes a file bigger than its budget,
        // but it is never empty.
        let s = pick(&[(0, 1_000_000), (1, 10), (2, 10)], 7);
        assert!(!s.is_empty());
        // A single file is always re-checked.
        assert_eq!(pick(&[(5, 123)], 3), BTreeSet::from([5]));
        assert!(pick(&[], 1).is_empty());
    }
}
