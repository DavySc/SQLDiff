//! Stored conversions: a run's converted tree kept in the data directory
//! (`gui/conversions/`), so its review opens again after a restart without
//! waiting for the tree to be converted again.
//!
//! A stored conversion only ever stands in until a fresh one is there. The
//! review it opens is provisional (`convert_review::GATE_CONFIRMING`): the
//! queue shows at once, while the tree is converted again on another
//! thread; the done gate is judged, and decisions recorded, only on the
//! fresh conversion, which then replaces the stored one. A key that misses
//! some input can make a stored conversion stale, never a verdict.
//!
//! Each file is `<key>.json.gz`: the `tree::Tree` as JSON, gzipped. The key
//! (`convert_review::stored_key`) hashes the run's command and the contents
//! of the config and packs it loads, every source file, the ledger's
//! hand-written texts and the base catalogs. Not the build of the app: what
//! another build converts differently, the fresh conversion replaces. Files that
//! cannot be read back are deleted. The oldest files go once the folder
//! holds more than [`CAP`]; the settings page shows the folder, what it
//! holds, and clears it.
//!
//! Stored conversions hold SQL, unlike the run history: an app with a
//! `data_key` (or no data directory) keeps none.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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
        let read = flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut json)
            .ok()
            .and_then(|_| serde_json::from_slice::<Tree>(&json).ok());
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

/// Every file under `p` (or `p` itself), in a fixed order, with its
/// contents: what a key takes of config files and packs. Links below `p`
/// are not followed.
pub fn contents(p: &Path, out: &mut Vec<u8>) {
    fn part(out: &mut Vec<u8>, b: &[u8]) {
        out.extend((b.len() as u64).to_le_bytes());
        out.extend(b);
    }
    part(out, p.display().to_string().as_bytes());
    if p.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();
        part(out, &(entries.len() as u64).to_le_bytes());
        for e in entries {
            match std::fs::symlink_metadata(&e) {
                Ok(m) if m.file_type().is_symlink() => part(out, b"link"),
                _ => contents(&e, out),
            }
        }
    } else {
        let text = std::fs::read(p).unwrap_or_default();
        part(out, &text);
    }
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
