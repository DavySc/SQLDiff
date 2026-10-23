//! Cache of analysis results in the data directory (design section 10).
//!
//! Entries are keyed by a hash of everything the result depends on: the
//! exact text, dialect, version and path of each side, the engine options,
//! the content hashes of every config layer, and the build of the running
//! program. A changed input misses; nothing is ever invalidated by hand.
//! With a data key, entries are sealed like the review store.
//!
//! Layout: `<data_dir>/cache/<namespace>/<xx>/<key>.json`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqldiff_core::model::hex;
use sqldiff_core::{Artifact, Options, Report};
use sqldiff_source::{Resolved, Result};

use crate::crypto::{self, DataKey};

/// Bump when the format of a cached value changes.
pub const CACHE_SCHEMA: &str = "1";

#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
    key: Option<DataKey>,
    enabled: bool,
}

/// Identifies the running build, so a rebuilt engine never reads results
/// of an older one: tool version plus the executable's size and mtime.
fn build_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| {
                let t = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                format!("{}:{t}", m.len())
            })
            .unwrap_or_default();
        format!("{}/{CACHE_SCHEMA}/{exe}", sqldiff_core::TOOL_VERSION)
    })
}

impl Cache {
    pub fn open(data_dir: &Path, key: Option<DataKey>) -> Cache {
        Cache {
            dir: data_dir.join("cache"),
            key,
            enabled: true,
        }
    }

    pub fn from_config(cfg: &Resolved) -> Result<Cache> {
        Ok(Cache::open(&cfg.data_dir, crate::data_key(cfg)?))
    }

    /// A cache that never hits and never writes.
    pub fn disabled() -> Cache {
        Cache {
            dir: PathBuf::new(),
            key: None,
            enabled: false,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, ns: &str, key: &str) -> PathBuf {
        self.dir
            .join(ns)
            .join(&key[..2.min(key.len())])
            .join(format!("{key}.json"))
    }

    pub fn get<T: DeserializeOwned>(&self, ns: &str, key: &str) -> Option<T> {
        if !self.enabled {
            return None;
        }
        let text = std::fs::read_to_string(self.file(ns, key)).ok()?;
        let plain = crypto::decode(self.key.as_ref(), &text).ok()?;
        serde_json::from_str(&plain).ok()
    }

    /// Stores a value. Failures are ignored: the cache is an optimisation.
    pub fn put<T: Serialize>(&self, ns: &str, key: &str, value: &T) {
        if !self.enabled {
            return;
        }
        let Ok(json) = serde_json::to_string(value) else {
            return;
        };
        let _ = crate::store::write_atomic(
            &self.file(ns, key),
            crypto::encode(self.key.as_ref(), &json).as_bytes(),
        );
    }

    /// Removes every entry. Returns whether there was a cache.
    pub fn clear(&self) -> Result<bool> {
        if !self.enabled || !self.dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&self.dir)?;
        Ok(true)
    }

    /// `diff` or `check` (when `b` is `None`) through the cache.
    pub fn report(
        &self,
        a: &Artifact,
        b: Option<&Artifact>,
        opts: &Options,
        cfg: &Resolved,
        compute: impl FnOnce() -> Report,
    ) -> Report {
        let key = report_key(a, b, opts, cfg);
        if let Some(r) = self.get::<Report>("report", &key) {
            return r;
        }
        let r = compute();
        self.put("report", &key, &r);
        r
    }
}

fn artifact_part(h: &mut Sha256, a: &Artifact) {
    for p in [a.path.as_str(), a.dialect.id(), a.version.as_str()] {
        h.update(p.as_bytes());
        h.update([0]);
    }
    h.update(Sha256::digest(a.text.as_bytes()));
}

/// Cache key of a report.
pub fn report_key(a: &Artifact, b: Option<&Artifact>, opts: &Options, cfg: &Resolved) -> String {
    let mut h = Sha256::new();
    h.update(build_id().as_bytes());
    h.update([0]);
    h.update(opts.excerpt_lines.to_le_bytes());
    for (id, holds) in &opts.flips {
        h.update(b"F");
        h.update(id.as_bytes());
        h.update([0, u8::from(*holds) + 1]);
    }
    for (id, text) in &opts.declared {
        h.update(b"D");
        h.update(id.as_bytes());
        h.update([0]);
        h.update(text.as_bytes());
        h.update([0]);
    }
    for (tag, cat) in [(b"CA", &opts.catalog_a), (b"CB", &opts.catalog_b)] {
        for c in cat.iter() {
            h.update(tag);
            artifact_part(&mut h, c);
        }
    }
    for l in &cfg.layers {
        h.update(l.hash.as_bytes());
        h.update([0]);
    }
    // Structured catalog facts change binding too; pack helpers are covered by
    // the layer hashes.
    for (tag, facts) in [(b"FA", &opts.facts_a), (b"FB", &opts.facts_b)] {
        h.update(tag);
        for f in facts {
            h.update(serde_json::to_vec(f.as_ref()).unwrap_or_default());
            h.update([0]);
        }
    }
    artifact_part(&mut h, a);
    match b {
        Some(b) => {
            h.update(b"B");
            artifact_part(&mut h, b);
        }
        None => h.update(b"-"),
    }
    hex(&h.finalize())
}
