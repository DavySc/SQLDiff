//! Review decisions (design §6.3): accepted, rejected or commented, keyed by
//! finding fingerprint and snapshot, with optional expiry.
//!
//! Stand-in for the shared review store (`sqldiff-review`, WP6) with the
//! same shape, kept as one JSON file in the data directory. Swap the
//! implementation when that crate lands; the GUI only uses this module's API.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Accepted,
    Rejected,
    Commented,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub state: ReviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// RFC 3339, UTC.
    pub at: String,
    /// RFC 3339, UTC; the decision is ignored after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// A decision with what it is keyed by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub fingerprint: String,
    /// Content hashes of both sides, `<a>:<b>`.
    pub snapshot: String,
    #[serde(flatten)]
    pub decision: Decision,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    version: u32,
    decisions: Vec<Entry>,
}

/// The decisions of one data directory. `None` path keeps them in memory only.
#[derive(Debug, Clone, Default)]
pub struct Store {
    path: Option<PathBuf>,
    entries: BTreeMap<(String, String), Decision>,
}

pub const FILE_NAME: &str = "reviews.json";

impl Store {
    /// Opens (or starts) the store in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Store, String> {
        let path = data_dir.join(FILE_NAME);
        let mut s = Store {
            path: Some(path.clone()),
            entries: BTreeMap::new(),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let f: File =
                    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                let now = now_rfc3339();
                for e in f.decisions {
                    if e.decision
                        .expires
                        .as_deref()
                        .is_some_and(|x| x <= now.as_str())
                    {
                        continue;
                    }
                    s.entries.insert((e.fingerprint, e.snapshot), e.decision);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        Ok(s)
    }

    pub fn in_memory() -> Store {
        Store::default()
    }

    pub fn get(&self, fingerprint: &str, snapshot: &str) -> Option<&Decision> {
        self.entries
            .get(&(fingerprint.to_string(), snapshot.to_string()))
    }

    /// The decision for this snapshot, else the latest one for the
    /// fingerprint on another snapshot (`false`: made on an earlier text).
    pub fn lookup(&self, fingerprint: &str, snapshot: &str) -> Option<(&Decision, bool)> {
        if let Some(d) = self.get(fingerprint, snapshot) {
            return Some((d, true));
        }
        self.entries
            .iter()
            .filter(|((f, _), _)| f == fingerprint)
            .map(|(_, d)| d)
            .max_by(|x, y| x.at.cmp(&y.at))
            .map(|d| (d, false))
    }

    pub fn put(&mut self, fingerprint: &str, snapshot: &str, d: Decision) -> Result<(), String> {
        self.entries
            .insert((fingerprint.to_string(), snapshot.to_string()), d);
        self.save()
    }

    pub fn remove(&mut self, fingerprint: &str, snapshot: &str) -> Result<(), String> {
        self.entries
            .remove(&(fingerprint.to_string(), snapshot.to_string()));
        self.save()
    }

    pub fn all(&self) -> impl Iterator<Item = Entry> + '_ {
        self.entries.iter().map(|((f, s), d)| Entry {
            fingerprint: f.clone(),
            snapshot: s.clone(),
            decision: d.clone(),
        })
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let f = File {
            version: 1,
            decisions: self.all().collect(),
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&f).unwrap())
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Snapshot key of a pair: both content hashes.
pub fn snapshot(hash_a: &str, hash_b: &str) -> String {
    format!("{hash_a}:{hash_b}")
}

pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_792_768_743), "2026-10-23T15:19:03Z");
    }
}
