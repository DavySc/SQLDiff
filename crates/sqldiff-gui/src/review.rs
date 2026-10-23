//! Review decisions (design §6.3): the shared store of `sqldiff-review`,
//! keyed by finding fingerprint and snapshot, with optional expiry.

use serde::Serialize;

pub use sqldiff_review::store::{Decision, State, Store, ANY_SNAPSHOT};
pub use sqldiff_review::time;

/// One decision as the page sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub fingerprint: String,
    /// `<content_hash_a>:<content_hash_b>`, or `*` for every snapshot.
    pub snapshot: String,
    #[serde(flatten)]
    pub decision: Decision,
}

/// Snapshot key of a pair: both content hashes (as `sqldiff_review::snapshot`).
pub fn snapshot(hash_a: &str, hash_b: &str) -> String {
    format!("{hash_a}:{hash_b}")
}
