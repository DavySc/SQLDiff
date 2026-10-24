//! SQLDiff acquisition and configuration (design section 5).
//!
//! - [`pack`]: declarative packs and the policy they carry, validated at load.
//! - [`config`]: the layered configuration (built-in < packs < project <
//!   user < command line) and its provenance.
//! - [`protocol`] and [`Source`]: the adapter protocol of section 5.4, served
//!   in process by the reference adapters and out of process over stdio by
//!   [`process`].
//! - [`fs`], [`git`] and [`bundle`]: the reference adapters. A snapshot
//!   bundle is the canonical offline and fixture format.
//! - [`pairing`] and [`policy`]: pack-driven pairing, and pack policy applied
//!   to engine reports (overrides, declared assumptions, limits).
//! - [`queue`]: every pair of a source diffed and ranked for review, with
//!   session-local assumption flips and GUI deep links.

pub mod bundle;
pub mod changes;
pub mod compose;
pub mod config;
mod error;
pub mod fs;
pub mod git;
pub mod glob;
pub mod pack;
pub mod pairing;
pub mod policy;
pub mod process;
pub mod protocol;
pub mod queue;
mod source;
pub mod tree;
mod walk;
pub use walk::defined_name;

pub use bundle::{Bundle, BundleSource};
pub use config::{LayerKind, LayerRef, Resolved};
pub use error::{Error, Result};
pub use fs::FsSource;
pub use git::GitSource;
pub use pack::{Pack, Policy};
pub use process::ProcessSource;
pub use source::{open, Source, SourceSpec};
pub use walk::classify;
