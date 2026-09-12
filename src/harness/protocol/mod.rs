//! Shared wire types. Only the daemon interprets project knowledge and policy.
//! Every type is re-exported here so callers keep addressing
//! `harness::protocol::X`; the submodules only group the exchange by purpose.
mod associate;
mod candidates;
mod capture;
mod context;
mod scope;
mod typing;

pub use associate::*;
pub use candidates::*;
pub use capture::*;
pub use context::*;
pub use scope::*;
pub use typing::*;
