//! Statement equivalence: the part of the engine that decides whether two
//! statements mean the same, below the control-flow and diff layers.
//!
//! It depends on `sqldiff-model`, `sqldiff-catalog` and `sqldiff-syntax` only.
//! `sqldiff-core` re-exports every module under its old path. See
//! `docs/crate-split.md`.

pub mod classify;
pub mod coverage;
pub mod equiv;
pub mod normalize;
pub mod rules;
pub mod treediff;

// The moved code names these through `crate::`, as it did inside `sqldiff-core`.
pub use sqldiff_catalog as catalog;
pub use sqldiff_catalog::tokens;
pub use sqldiff_model::{artifact, assumptions, model, session, target};
