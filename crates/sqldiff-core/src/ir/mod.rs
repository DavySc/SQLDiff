//! Semantic IR (design §3 Layer 3): dialect profiles as data, lowering to a
//! control-flow graph with error edges and effect annotations, and
//! transaction-state abstract interpretation. `behaviour` compares two
//! artifacts by the outcomes of their runs.

pub mod behaviour;
pub mod cfg;
pub mod interp;
pub mod overlay;
pub mod profile;
pub mod witness;

pub use cfg::{lower, Cfg};
pub use interp::{explore, Exit, Fate, Fault, Outcome, Routed, Scenario};
pub use profile::{ErrorKind, Profile};
