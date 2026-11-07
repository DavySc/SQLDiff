//! SQLDiff core: deterministic semantic diff of SQL artifacts across
//! T-SQL, PostgreSQL and Db2 for i.
//!
//! The engine never reports a false "equal": anything it cannot show to be
//! equivalent is reported as "unknown" or "equal under assumptions".

// Determinism guard (perf plan W0): iterating a hash map or set in a `for` loop is
// as order-dependent as `.iter()`; each site needs a reasoned `#[allow]`.
#![warn(clippy::iter_over_hash_type)]

pub use sqldiff_model::artifact;
pub use sqldiff_model::assumptions;
pub mod bind;
pub use sqldiff_catalog as catalog;
pub use sqldiff_equiv::classify;
pub mod composition;
pub use sqldiff_equiv::coverage;
mod engine;
pub use sqldiff_equiv::equiv;
pub mod facts;
pub mod format;
pub mod hooks;
pub mod ir;
pub mod ledger;
pub mod lint;
pub use sqldiff_equiv::normalize;
pub use sqldiff_model::model;
pub mod observer;
pub mod phrase;
pub use sqldiff_model::proof;
pub mod render;
pub mod rule_proofs;
pub use sqldiff_catalog::tokens;
pub use sqldiff_equiv::rules;
pub use sqldiff_equiv::treediff;
pub use sqldiff_model::session;
pub use sqldiff_model::target;

pub use artifact::Artifact;
pub use engine::{
    check, check_recorded, diff, diff_recorded, identity, overlay, pair_states, shared_conditions,
    what_if, Facts, Options, Replay, TOOL_VERSION,
};
pub use model::{ChangeClass, Finding, Report, Severity, Verdict};
pub use sqldiff_syntax::Dialect;

/// Stack size for threads that run the engine. Its parsers and tree walks
/// recurse with the nesting of the SQL; on the default 2 MiB (8 MiB for the
/// main thread) deeply nested input overflows the stack, which aborts the
/// whole process (a tree conversion, the GUI server, the MCP server) instead
/// of failing one file or request.
pub const ENGINE_STACK: usize = 256 << 20;

/// Runs `f` on a thread with [`ENGINE_STACK`] and returns its result; a
/// panic in `f` continues in the caller.
pub fn with_engine_stack<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(ENGINE_STACK)
            .spawn_scoped(s, f)
            .expect("start the engine thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}
