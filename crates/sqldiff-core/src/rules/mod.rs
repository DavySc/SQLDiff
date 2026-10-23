//! Idiom rules (design section 8): declarative S-expression rules that lift
//! dialect patterns to canonical nodes and emit them back, translate between
//! dialects, and report lints and near misses.

pub mod analysis;
pub mod convert;
pub mod pattern;
pub mod print;
pub mod rewrite;
pub mod rule;
pub mod sexp;
pub mod surface;
