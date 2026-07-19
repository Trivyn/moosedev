//! Code substrate index core for MOOSEDev v2 spec §3.1-§3.3.
//!
//! This module consumes the derived `.moosedev/substrate/` SCIP index and
//! resolves source positions to semantic symbols. Positions are 0-based UTF-8
//! byte offsets within a line, and ranges are end-exclusive. SCIP symbol strings
//! are preserved as emitted for resolution; helper APIs expose version-normalized
//! forms for stable code-entity identity.

use std::path::{Path, PathBuf};

pub mod churn;
pub(crate) mod lang;
pub mod meta;
pub mod producer;
pub mod resolver;
pub mod scheduler;
pub(crate) mod scip;
pub mod symbols;
pub(crate) mod treesitter;

pub use churn::{ChurnIndex, FileChurn};
pub use meta::{ProducerRun, SubstrateIdentity, SubstrateMeta};
pub use producer::{
    registry, run_index, IndexReport, ProducerReport, ProducerSpec, ProducerTarget,
};
pub use resolver::{
    DefinitionEntry, Position, Resolution, ResolutionMode, SourceRange, Substrate, SubstrateStats,
    STALE_CHECK_TTL,
};

pub const SUBSTRATE_DIR: &str = "substrate";
pub const GENERATIONS_DIR: &str = "generations";
pub const INDEX_FILE_NAME: &str = "index.scip";
pub const INDEX_TMP_FILE_NAME: &str = "index.scip.tmp";
pub const INDEX_LOG_FILE_NAME: &str = "index.log";
pub const INDEX_LOCK_FILE_NAME: &str = "index.lock";
pub const CHURN_FILE_NAME: &str = "churn.json";
pub const META_FILE_NAME: &str = "meta.json";

pub fn substrate_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SUBSTRATE_DIR)
}

pub fn index_path(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(INDEX_FILE_NAME)
}

pub fn index_tmp_path(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(INDEX_TMP_FILE_NAME)
}

pub fn producer_index_path(data_dir: &Path, producer: &str) -> PathBuf {
    producer_index_path_in(&substrate_dir(data_dir), producer)
}

pub fn producer_index_tmp_path(data_dir: &Path, producer: &str) -> PathBuf {
    producer_index_tmp_path_in(&substrate_dir(data_dir), producer)
}

pub fn producer_index_path_in(artifact_root: &Path, producer: &str) -> PathBuf {
    artifact_root.join(format!("index.{producer}.scip"))
}

pub fn producer_index_tmp_path_in(artifact_root: &Path, producer: &str) -> PathBuf {
    artifact_root.join(format!("index.{producer}.scip.tmp"))
}

pub fn generations_dir(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(GENERATIONS_DIR)
}

pub fn generation_dir(data_dir: &Path, generation: &str) -> PathBuf {
    generations_dir(data_dir).join(generation)
}

pub fn index_lock_path(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(INDEX_LOCK_FILE_NAME)
}

pub fn index_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(INDEX_LOG_FILE_NAME)
}

pub fn meta_path(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(META_FILE_NAME)
}
