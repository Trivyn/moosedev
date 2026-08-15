//! Human-readable subsystem Stories projected from authoritative project knowledge.
//!
//! Recipes are version-controlled presentation metadata under `stories/`. Story
//! runs are read-only projections: this module never writes the project graph.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use moose::traits::LlmClient;
use moose::types::{LlmAssistLevel, LlmParams};
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNode, Term};
use serde::{Deserialize, Serialize};

use crate::graph::{
    asserted_project_types, first_literal, in_working_set, load_components, local_name,
    relevant_context_snapshot, resolve_component_query, AppState, CodeTerms, ComponentEntry,
    PROJECT_KG_GRAPH_IRI,
};

const MIN_PUBLISHED_BEATS: usize = 3;
const MAX_BEATS: usize = 5;
const MAX_ANCHORS_PER_BEAT: usize = 6;
const MAX_CHECK_GRANTS: usize = 1_024;
const MAX_RETIRED_CHECK_HANDLES: usize = 1_024;
const MAX_CHECK_OPTIONS: usize = 3;
const MAX_LLM_FIELD_BYTES: usize = 512;
const MAX_LLM_PROMPT_BYTES: usize = 32 * 1024;
const STORY_SCHEMA_VERSION: u8 = 2;
const MAX_TOPIC_CHARS: usize = 200;
const CHECK_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

mod checks;
mod grounding;
mod model;
mod narration;
mod planner;
mod repository;
mod resolution;

pub use checks::*;
pub use model::*;
pub use narration::*;
pub use planner::*;
pub use repository::*;
pub use resolution::*;

#[cfg(test)]
use grounding::*;

#[cfg(test)]
mod tests;
