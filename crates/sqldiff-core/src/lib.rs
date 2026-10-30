//! SQLDiff core: deterministic semantic diff of SQL artifacts across
//! T-SQL, PostgreSQL and Db2 for i.
//!
//! The engine never reports a false "equal": anything it cannot show to be
//! equivalent is reported as "unknown" or "equal under assumptions".

pub mod artifact;
pub mod assumptions;
pub mod bind;
pub mod catalog;
pub mod classify;
pub mod composition;
pub mod coverage;
mod engine;
pub mod equiv;
pub mod facts;
pub mod format;
pub mod hooks;
pub mod ir;
pub mod ledger;
pub mod lint;
pub mod model;
pub mod normalize;
pub mod observer;
pub mod phrase;
pub mod proof;
pub mod render;
pub mod rule_proofs;
pub mod rules;
pub mod session;
pub mod target;
pub mod tokens;
pub mod treediff;

pub use artifact::Artifact;
pub use engine::{check, diff, overlay, what_if, Options, TOOL_VERSION};
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
