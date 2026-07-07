//! Code substrate index core for MOOSEDev v2 spec §3.1-§3.3.
//!
//! This module consumes the derived `.moosedev/substrate/` SCIP index and
//! resolves source positions to semantic symbols. Positions are 0-based UTF-8
//! byte offsets within a line, and ranges are end-exclusive. SCIP symbol strings
//! are preserved as emitted, including crate versions; symbol normalization is a
//! later slice's concern.

use std::path::{Path, PathBuf};

pub mod meta;
pub mod producer;
pub mod resolver;
pub(crate) mod scip;

pub use meta::SubstrateMeta;
pub use producer::{run_index, IndexReport};
pub use resolver::{Position, Resolution, ResolutionMode, SourceRange, Substrate, SubstrateStats};

pub const SUBSTRATE_DIR: &str = "substrate";
pub const INDEX_FILE_NAME: &str = "index.scip";
pub const INDEX_TMP_FILE_NAME: &str = "index.scip.tmp";
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

pub fn meta_path(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(META_FILE_NAME)
}
