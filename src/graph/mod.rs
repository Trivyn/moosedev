//! Durable project knowledge graph: server state, instance-IRI minting, typed
//! capture (built on MOOSE's cache-coherent `kg::assert_instance`), and NLQ
//! query (via MOOSE's `execute_graph_walk_nlq_with_context`, answer + trace).
//!
//! MOOSEDev owns the *domain* semantics (what a decision is, IRI conventions,
//! the durable store); MOOSE owns the *mechanics* (transactional write + index
//! coherence; symbolic-first graph-walk query).
//!
//! The graph implementation is split by responsibility below, but this module
//! re-exports each submodule so the public `crate::graph::*` API remains stable.

pub mod capture;
pub mod context;
pub mod lifecycle;
pub mod links;
pub mod query;
pub mod relations;
pub mod state;
pub mod util;

pub use capture::*;
pub use context::*;
pub use lifecycle::*;
pub use links::*;
pub use query::*;
pub use relations::*;
pub use state::*;
pub use util::*;

/// Named graph holding recorded knowledge instances (the durable project KG).
pub const PROJECT_KG_GRAPH_IRI: &str = "https://moosedev.dev/kg/project";
