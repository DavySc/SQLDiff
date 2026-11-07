//! The base layer of the engine: the report model, the assumption registry,
//! target versions, the semantic profiles, proofs and the analysed artifact.
//!
//! Nothing here depends on the rest of the engine (`sqldiff-core`), which
//! re-exports these modules under their old paths. See `docs/crate-split.md`.

// Determinism guard (perf plan W0): iterating a hash map or set in a `for` loop is
// as order-dependent as `.iter()`; each site needs a reasoned `#[allow]`.
#![warn(clippy::iter_over_hash_type)]

pub mod artifact;
pub mod assumptions;
pub mod model;
pub mod profile;
pub mod proof;
pub mod session;
pub mod target;
pub mod witness;
pub mod work;
