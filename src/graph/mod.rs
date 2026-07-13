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
pub mod code_entities;
pub mod components;
pub mod context;
pub mod debt;
pub mod dossier;
pub mod lifecycle;
pub mod link_code;
pub mod links;
pub mod proposals;
pub mod query;
pub mod relations;
pub mod state;
pub mod util;

pub use capture::*;
pub use code_entities::*;
pub use components::*;
pub use context::*;
pub use debt::*;
pub use dossier::*;
pub use lifecycle::*;
pub use link_code::*;
pub use links::*;
pub use proposals::*;
pub use query::*;
pub use relations::*;
pub use state::*;
pub use util::*;

/// Named graph holding recorded knowledge instances (the durable project KG).
pub const PROJECT_KG_GRAPH_IRI: &str = "https://moosedev.dev/kg/project";
