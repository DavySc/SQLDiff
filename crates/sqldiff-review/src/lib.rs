//! SQLDiff review workflow (design sections 5.5, 6.3, 9 and 10).
//!
//! - [`store`]: accept / reject / comment decisions keyed by finding
//!   fingerprint and snapshot, with optional expiry, shared by GUI, CLI and MCP.
//! - [`suppress`]: hides accepted findings and in-source deliberate
//!   differences from reports, keeping them listed as suppressed.
//! - [`markers`]: pack `author_markers` applied to reports: deliberate
//!   difference markers and author-declared assumptions.
//! - [`redact`]: redacted export with stable placeholders for pack-derived
//!   identifiers, checked against the deny-list.
//! - [`history`]: change descriptions from `get_history` grouped by work item.
//! - [`cache`]: per-content-hash cache of analysis results in the data directory.
//! - [`crypto`]: optional encryption at rest for the store and the cache.

pub mod cache;
pub mod crypto;
pub mod history;
pub mod markers;
pub mod redact;
pub mod store;
pub mod suppress;
pub mod time;

pub use cache::Cache;
pub use crypto::DataKey;
pub use redact::Redactor;
pub use store::{snapshot, Decision, Key, Lookup, ReviewState, State, Status, Store, ANY_SNAPSHOT};
pub use suppress::{gate_verdict, suppress};

use sqldiff_source::{Resolved, Result};

/// The configured data key, if the data directory is encrypted.
pub fn data_key(cfg: &Resolved) -> Result<Option<DataKey>> {
    cfg.data_key.as_deref().map(DataKey::from_ref).transpose()
}
