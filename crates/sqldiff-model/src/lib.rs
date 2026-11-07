//! The base layer of the engine: the report model, the assumption registry,
//! target versions, the semantic profiles, proofs and the analysed artifact.
//!
//! Nothing here depends on the rest of the engine (`sqldiff-core`), which
//! re-exports these modules under their old paths. See `docs/crate-split.md`.

pub mod artifact;
pub mod assumptions;
pub mod model;
pub mod profile;
pub mod proof;
pub mod session;
pub mod target;
pub mod witness;
