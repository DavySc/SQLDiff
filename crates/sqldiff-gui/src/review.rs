//! Review decisions (design §6.3): the shared store of `sqldiff-review`,
//! keyed by finding fingerprint and snapshot, with optional expiry.

pub use sqldiff_review::store::{Decision, State, Status, Store, ANY_SNAPSHOT};
pub use sqldiff_review::{snapshot, time};
