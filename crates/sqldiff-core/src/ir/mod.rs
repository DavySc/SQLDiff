//! Semantic IR (design §3 Layer 3): dialect profiles as data, lowering to a
//! control-flow graph with error edges and effect annotations, and
//! transaction-state abstract interpretation. `behaviour` compares two
//! artifacts by the outcomes of their runs.

pub mod behaviour;
pub mod cfg;
pub mod delta;
pub mod interp;
pub mod overlay;
pub use sqldiff_model::profile;
pub use sqldiff_model::witness;

pub use cfg::{lower, Cfg};
pub use interp::{
    decision_free, explore, Exit, Explorer, Fate, Fault, KeyUniverse, Outcome, Routed, Scenario,
};
pub use profile::{ErrorKind, Profile};
