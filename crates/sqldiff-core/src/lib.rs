//! SQLDiff core: deterministic semantic diff of SQL artifacts across
//! T-SQL, PostgreSQL and Db2 for i.
//!
//! The engine never reports a false "equal": anything it cannot show to be
//! equivalent is reported as "unknown" or "equal under assumptions".

pub mod artifact;
pub mod classify;
pub mod composition;
mod engine;
pub mod equiv;
pub mod facts;
pub mod ir;
pub mod ledger;
pub mod lint;
pub mod model;
pub mod normalize;
pub mod phrase;
pub mod render;
pub mod rules;
pub mod session;
pub mod treediff;

pub use artifact::Artifact;
pub use engine::{check, diff, overlay, what_if, Options, TOOL_VERSION};
pub use model::{ChangeClass, Finding, Report, Severity, Verdict};
pub use sqldiff_syntax::Dialect;
