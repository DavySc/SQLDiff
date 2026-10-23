//! The review store (design section 6.3): accepted, rejected and commented
//! decisions keyed by finding fingerprint and snapshot, with optional
//! expiry. GUI, CLI and MCP share it.
//!
//! The store is an append-only log, `<data_dir>/review/decisions.jsonl`,
//! one record per line; the last record for a key wins and a `cleared`
//! record removes the key. Appends are single writes, so the GUI and the
//! CLI can share the file; call [`Store::reload`] to see the other's writes.
//! With a data key every line is sealed (see [`crate::crypto`]).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqldiff_core::Report;
use sqldiff_source::{Error, Resolved, Result};

use crate::crypto::{self, DataKey};
use crate::time;

/// Snapshot that matches every snapshot of a fingerprint.
pub const ANY_SNAPSHOT: &str = "*";

pub const STORE_FILE: &str = "review/decisions.jsonl";

/// The snapshot a report was made from: `<content_hash_a>:<content_hash_b>`,
/// or `<content_hash_a>` for a check.
pub fn snapshot(report: &Report) -> String {
    match &report.b {
        Some(b) => format!("{}:{}", report.a.content_hash, b.content_hash),
        None => report.a.content_hash.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Key {
    pub fingerprint: String,
    /// A [`snapshot`] string, or [`ANY_SNAPSHOT`].
    pub snapshot: String,
}

impl Key {
    pub fn new(fingerprint: &str, snapshot: &str) -> Key {
        Key {
            fingerprint: fingerprint.to_string(),
            snapshot: snapshot.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// A deliberate, accepted divergence: suppressed in reports.
    Accepted,
    /// Confirmed as a real problem to fix.
    Rejected,
    /// Neither, only a comment.
    Commented,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Accepted => "accepted",
            State::Rejected => "rejected",
            State::Commented => "commented",
        }
    }
}

impl std::str::FromStr for State {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<State, String> {
        match s {
            "accepted" | "accept" => Ok(State::Accepted),
            "rejected" | "reject" => Ok(State::Rejected),
            "commented" | "comment" => Ok(State::Commented),
            _ => Err(format!(
                "review state `{s}`: expected accepted, rejected or commented"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// When the decision was made (normalized RFC 3339, UTC).
    pub at: String,
    /// After this time the decision no longer applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// Rule of the finding, for listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Object of the finding, for listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

impl Decision {
    /// A decision made now, with nothing else set.
    pub fn new(state: State) -> Decision {
        Decision {
            state,
            comment: None,
            author: None,
            at: time::now(),
            expires: None,
            rule: None,
            object: None,
        }
    }

    pub fn comment(mut self, c: impl Into<String>) -> Self {
        self.comment = Some(c.into());
        self
    }

    pub fn author(mut self, a: impl Into<String>) -> Self {
        self.author = Some(a.into());
        self
    }

    /// Any form [`time::normalize`] accepts.
    pub fn expires(mut self, t: &str) -> Result<Self> {
        self.expires = Some(time::normalize(t)?);
        Ok(self)
    }

    pub fn is_expired(&self, now: &str) -> bool {
        self.expires.as_deref().is_some_and(|e| e <= now)
    }
}

/// Review state of one finding in one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Unreviewed,
    Accepted,
    Rejected,
    Commented,
    /// The decision for this snapshot has expired.
    Expired,
    /// Decided for another snapshot only; the source changed since.
    Outdated,
}

impl ReviewState {
    pub fn label(self) -> &'static str {
        match self {
            ReviewState::Unreviewed => "unreviewed",
            ReviewState::Accepted => "accepted",
            ReviewState::Rejected => "rejected",
            ReviewState::Commented => "commented",
            ReviewState::Expired => "expired",
            ReviewState::Outdated => "outdated",
        }
    }

    pub fn is_reviewed(self) -> bool {
        matches!(
            self,
            ReviewState::Accepted | ReviewState::Rejected | ReviewState::Commented
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: ReviewState,
    /// The decision behind the state, if any (for `outdated`, the latest one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    /// The snapshot the decision was recorded for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    fingerprint: String,
    snapshot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<Decision>,
    /// Time the key was cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleared: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Store {
    path: Option<PathBuf>,
    key: Option<DataKey>,
    entries: BTreeMap<Key, Decision>,
    warnings: Vec<String>,
}

impl Store {
    /// A store that is never written to disk (tests, session-only use).
    pub fn in_memory() -> Store {
        Store::default()
    }

    /// Opens `<data_dir>/review/decisions.jsonl`, without encryption.
    pub fn open(data_dir: &Path) -> Result<Store> {
        Store::open_with(data_dir, None)
    }

    /// Opens the store with an optional data key.
    pub fn open_with(data_dir: &Path, key: Option<DataKey>) -> Result<Store> {
        let mut s = Store {
            path: Some(data_dir.join(STORE_FILE)),
            key,
            entries: BTreeMap::new(),
            warnings: Vec::new(),
        };
        s.reload()?;
        Ok(s)
    }

    /// Opens the store in the configured data directory, sealed with the
    /// configured data key if there is one.
    pub fn from_config(cfg: &Resolved) -> Result<Store> {
        Store::open_with(&cfg.data_dir, crate::data_key(cfg)?)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Lines the last [`Store::reload`] skipped because they did not decode.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Re-reads the log, picking up writes from other processes.
    pub fn reload(&mut self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::new(format!("{}: {e}", path.display()))),
        };
        let mut entries = BTreeMap::new();
        let mut warnings = Vec::new();
        let (mut good, mut first_bad) = (0usize, None);
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let at = |e: Error| e.context(format!("{}:{}", path.display(), i + 1));
            let parsed = crypto::decode(self.key.as_ref(), line)
                .and_then(|plain| serde_json::from_str::<Record>(&plain).map_err(Error::from))
                .map_err(at);
            let r = match parsed {
                Ok(r) => r,
                Err(e) => {
                    // A torn line (a crash mid-append) or a corrupt one is
                    // skipped; a store where nothing decodes is an error
                    // (wrong or missing data key).
                    warnings.push(format!("skipped: {e}"));
                    first_bad.get_or_insert(e);
                    continue;
                }
            };
            good += 1;
            let k = Key {
                fingerprint: r.fingerprint,
                snapshot: r.snapshot,
            };
            match r.decision {
                Some(d) => {
                    entries.insert(k, d);
                }
                None => {
                    entries.remove(&k);
                }
            }
        }
        if good == 0 {
            if let Some(e) = first_bad {
                return Err(e);
            }
        }
        self.entries = entries;
        self.warnings = warnings;
        Ok(())
    }

    /// The decision for exactly this key, else for [`ANY_SNAPSHOT`].
    pub fn get(&self, fingerprint: &str, snapshot: &str) -> Option<Decision> {
        self.entries
            .get(&Key::new(fingerprint, snapshot))
            .or_else(|| self.entries.get(&Key::new(fingerprint, ANY_SNAPSHOT)))
            .cloned()
    }

    /// Records a decision; it replaces any decision for the same key.
    pub fn put(&mut self, fingerprint: &str, snapshot: &str, decision: Decision) -> Result<()> {
        let mut decision = decision;
        decision.at = time::normalize(&decision.at)?;
        if let Some(e) = &decision.expires {
            decision.expires = Some(time::normalize(e)?);
        }
        let r = Record {
            fingerprint: fingerprint.to_string(),
            snapshot: snapshot.to_string(),
            decision: Some(decision.clone()),
            cleared: None,
        };
        self.append(&r)?;
        self.entries
            .insert(Key::new(fingerprint, snapshot), decision);
        Ok(())
    }

    /// Removes the decision for exactly this key. Returns whether there was one.
    pub fn clear(&mut self, fingerprint: &str, snapshot: &str) -> Result<bool> {
        let k = Key::new(fingerprint, snapshot);
        if !self.entries.contains_key(&k) {
            return Ok(false);
        }
        self.append(&Record {
            fingerprint: fingerprint.to_string(),
            snapshot: snapshot.to_string(),
            decision: None,
            cleared: Some(time::now()),
        })?;
        self.entries.remove(&k);
        Ok(true)
    }

    /// Every decision, by key.
    pub fn all(&self) -> Vec<(Key, Decision)> {
        self.entries
            .iter()
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect()
    }

    /// Decisions for one fingerprint in any snapshot, newest first.
    pub fn history(&self, fingerprint: &str) -> Vec<(Key, Decision)> {
        let mut v: Vec<(Key, Decision)> = self
            .entries
            .iter()
            .filter(|(k, _)| k.fingerprint == fingerprint)
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect();
        v.sort_by(|x, y| y.1.at.cmp(&x.1.at));
        v
    }

    /// Review state of a finding in a snapshot at time `now`.
    pub fn status(&self, fingerprint: &str, snapshot: &str, now: &str) -> Status {
        let exact = Key::new(fingerprint, snapshot);
        let any = Key::new(fingerprint, ANY_SNAPSHOT);
        for k in [&exact, &any] {
            if let Some(d) = self.entries.get(k) {
                let state = if d.is_expired(now) {
                    ReviewState::Expired
                } else {
                    match d.state {
                        State::Accepted => ReviewState::Accepted,
                        State::Rejected => ReviewState::Rejected,
                        State::Commented => ReviewState::Commented,
                    }
                };
                return Status {
                    state,
                    decision: Some(d.clone()),
                    snapshot: Some(k.snapshot.clone()),
                };
            }
        }
        match self.history(fingerprint).into_iter().next() {
            Some((k, d)) => Status {
                state: ReviewState::Outdated,
                decision: Some(d),
                snapshot: Some(k.snapshot),
            },
            None => Status {
                state: ReviewState::Unreviewed,
                decision: None,
                snapshot: None,
            },
        }
    }

    /// Rewrites the log with one record per live key, dropping decisions
    /// that expired before `now`.
    pub fn compact(&mut self, now: &str) -> Result<usize> {
        let Some(path) = self.path.clone() else {
            let before = self.entries.len();
            self.entries.retain(|_, d| !d.is_expired(now));
            return Ok(before - self.entries.len());
        };
        // Under the lock, re-read so records other processes appended since
        // this store was opened are kept.
        let _lock = Lock::take(&path)?;
        self.reload()?;
        let before = self.entries.len();
        self.entries.retain(|_, d| !d.is_expired(now));
        let mut out = String::new();
        for (k, d) in &self.entries {
            let r = Record {
                fingerprint: k.fingerprint.clone(),
                snapshot: k.snapshot.clone(),
                decision: Some(d.clone()),
                cleared: None,
            };
            out.push_str(&crypto::encode(
                self.key.as_ref(),
                &serde_json::to_string(&r)?,
            ));
            out.push('\n');
        }
        write_atomic(&path, out.as_bytes())?;
        Ok(before - self.entries.len())
    }

    fn append(&self, r: &Record) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = crypto::encode(self.key.as_ref(), &serde_json::to_string(r)?);
        line.push('\n');
        let _lock = Lock::take(path)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The store as the review queue sees it at one moment: expired and
/// outdated decisions count as unreviewed.
pub struct Lookup<'a> {
    pub store: &'a Store,
    pub now: String,
}

impl sqldiff_source::queue::ReviewLookup for Lookup<'_> {
    fn state(
        &self,
        fingerprint: &str,
        snapshot: &str,
    ) -> Option<sqldiff_source::queue::ReviewState> {
        use sqldiff_source::queue::ReviewState as Q;
        match self.store.status(fingerprint, snapshot, &self.now).state {
            ReviewState::Accepted => Some(Q::Accepted),
            ReviewState::Rejected => Some(Q::Rejected),
            ReviewState::Commented => Some(Q::Commented),
            ReviewState::Unreviewed | ReviewState::Expired | ReviewState::Outdated => None,
        }
    }
}

/// An advisory lock file next to the log, held by appends and compaction so
/// a compaction never drops a concurrent append. A lock older than ten
/// seconds is taken to be left over from a crash and is broken.
struct Lock(PathBuf);

impl Lock {
    fn take(log: &Path) -> Result<Lock> {
        let path = log.with_extension("lock");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        for _ in 0..500 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > 10);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
                Err(e) => return Err(Error::new(format!("{}: {e}", path.display()))),
            }
        }
        Err(Error::new(format!("{}: store is locked", path.display())))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
