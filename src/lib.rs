//! MOOSEDev library crate.
//!
//! Houses the MCP server surface, the durable knowledge-graph plumbing, and
//! ontology handling. The `moosedev` binary (`src/main.rs`) is a thin wrapper
//! that serves [`mcp::MooseDevServer`] over stdio. Keeping the logic in a lib
//! makes every module reachable from integration tests in `tests/`.

pub mod adrs;
pub mod alignment;
pub mod api;
mod artifacts;
pub mod canonical;
pub mod export;
pub mod graph;
pub mod graph_import;
pub mod init;
pub mod llm;
pub mod mcp;
pub mod ontology;
pub mod provenance;
pub mod reasoning;
pub mod requirements;
pub mod runtime;
pub mod sparql;
pub mod validation;
pub mod vectors;
