//! Stored conversions: a run's converted tree kept in the data directory
//! (`gui/conversions/`), so its review opens again after a restart without
//! converting the tree again.
//!
//! A stored conversion stands for a fresh one (Davy, 2026-10-29): the
//! review opened from it judges the done gate on it, with the ledger, the
//! instance report and the files in OUT read as they are now. So its key
//! (`convert_review::stored_key`) takes everything a conversion is made
//! from: this build of the app (a hash of its executable), the run's
//! command, the contents of the config files and packs (`pack.toml`) it loads,
//! every source file, the ledger's hand-written texts in ledger order and
//! the base catalogs. A file must read back to the very bytes it was
//! written from, or it is deleted and the tree converted again.
//!
//! Each file is `<key>.json.gz`: the `tree::Tree` as JSON, gzipped. The
//! least recently used go once the folder holds more than [`CAP`]; the
//! settings page shows the folder, what it holds, and clears it.
//!
//! Stored conversions hold SQL, unlike the run history: an app with a
//! `data_key` (or no data directory) keeps none.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;
use sqldiff_convert::tree::Tree;

/// The most the folder holds; the least recently used files go first.
pub const CAP: u64 = 2 << 30;

const SUFFIX: &str = ".json.gz";

pub struct Stored {
    dir: PathBuf,
}

/// What the folder holds, as the settings page shows it.
#[derive(Debug, Serialize)]
pub struct Usage {
    pub dir: String,
    pub files: usize,
    pub bytes: u64,
    pub cap: u64,
}

impl Stored {
    /// The stored conversions in `data_dir`.
    pub fn in_data_dir(data_dir: &Path) -> Stored {
        Stored {
            dir: data_dir.join("gui").join("conversions"),
        }
    }

    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}{SUFFIX}"))
    }

    /// The conversion stored under `key`, if one is there and reads back.
    pub fn get(&self, key: &str) -> Option<Tree> {
        let file = self.file(key);
        let bytes = std::fs::read(&file).ok()?;
        let mut json = Vec::new();
        // Read back exactly: what serializes to other bytes is not taken.
        let read = flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut json)
            .ok()
            .and_then(|_| serde_json::from_slice::<Tree>(&json).ok())
            .filter(|t| serde_json::to_vec(t).is_ok_and(|again| again == json));
        match read {
            Some(t) => {
                // Used now: the last to go.
                let _ = std::fs::File::options()
                    .append(true)
                    .open(&file)
                    .and_then(|f| f.set_modified(std::time::SystemTime::now()));
                Some(t)
            }
            None => {
                let _ = std::fs::remove_file(&file);
                None
            }
        }
    }

    /// Stores `json` (a `tree::Tree`) under `key`, through a temporary file
    /// so a reader never sees half of it; then trims the folder to [`CAP`].
    /// A convenience: a failure is not an error.
    pub fn put(&self, key: &str, json: &[u8]) {
        if json.is_empty() || std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let file = self.file(key);
        let tmp = self.dir.join(format!("{key}.{}.tmp", std::process::id()));
        let written = std::fs::File::create(&tmp).and_then(|f| {
            let mut z = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            z.write_all(json)?;
            z.finish()?.sync_all()
        });
        match written.and_then(|_| std::fs::rename(&tmp, &file)) {
            Ok(()) => self.trim(CAP),
            Err(_) => {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Stored files, least recently used first, with their sizes.
    fn files(&self) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        let mut v: Vec<_> = std::fs::read_dir(&self.dir)
            .map(|d| d.flatten().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.file_name().to_string_lossy().ends_with(SUFFIX))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((e.path(), m.len(), m.modified().ok()?))
            })
            .collect();
        v.sort_by_key(|(_, _, t)| *t);
        v
    }

    /// Deletes the least recently used files until the rest fit in `cap`.
    fn trim(&self, cap: u64) {
        let files = self.files();
        let mut total: u64 = files.iter().map(|(_, n, _)| n).sum();
        for (f, n, _) in files {
            if total <= cap {
                break;
            }
            if std::fs::remove_file(&f).is_ok() {
                total -= n;
            }
        }
    }

    pub fn usage(&self) -> Usage {
        let files = self.files();
        Usage {
            dir: self.dir.display().to_string(),
            files: files.len(),
            bytes: files.iter().map(|(_, n, _)| n).sum(),
            cap: CAP,
        }
    }

    /// Deletes every stored conversion (and nothing else in the folder).
    pub fn clear(&self) -> Usage {
        for (f, _, _) in self.files() {
            let _ = std::fs::remove_file(f);
        }
        self.usage()
    }
}

/// This build of the app: a hash of its own executable, so a conversion
/// another build made is never taken. Empty when the executable cannot be
/// read, which stores nothing.
pub fn build() -> &'static str {
    static BUILD: OnceLock<String> = OnceLock::new();
    BUILD.get_or_init(|| {
        use sha2::{Digest, Sha256};
        std::env::current_exe()
            .and_then(std::fs::read)
            .map(|b| format!("{:x}", Sha256::digest(b)))
            .unwrap_or_default()
    })
}

/// `p` and the contents of the file it names, read as the config and
/// pack loaders read it (through a link, if it is one): what a key takes
/// of a config file or a pack. `None` when it cannot be read, which
/// stores nothing.
pub fn contents(p: &Path, out: &mut Vec<u8>) -> Option<()> {
    let text = std::fs::read(p).ok()?;
    for b in [p.display().to_string().as_bytes(), &text[..]] {
        out.extend((b.len() as u64).to_le_bytes());
        out.extend(b);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sqldiff-stored-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn what_cannot_be_read_back_is_deleted_and_clear_keeps_other_files() {
        let d = dir("bad");
        let s = Stored::in_data_dir(&d);
        assert_eq!(s.usage().files, 0);
        s.put("k", b"{\"not\": \"a tree\"}");
        assert_eq!(s.usage().files, 1);
        assert!(s.get("k").is_none());
        assert_eq!(s.usage().files, 0, "deleted");
        assert!(s.get("missing").is_none());
        std::fs::write(s.dir.join("README"), "kept").unwrap();
        s.put("a", b"1");
        s.put("b", b"2");
        assert_eq!(s.clear().files, 0);
        assert!(s.dir.join("README").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_least_recently_used_go_first_over_the_cap() {
        let d = dir("trim");
        let s = Stored::in_data_dir(&d);
        for k in ["a", "b", "c"] {
            s.put(k, k.repeat(1000).as_bytes());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let size = |k: &str| std::fs::metadata(s.file(k)).unwrap().len();
        // Room for two: the oldest goes.
        s.trim(size("b") + size("c"));
        assert!(!s.file("a").exists());
        assert!(s.file("b").exists() && s.file("c").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
