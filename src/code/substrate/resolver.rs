//! Public substrate resolver API.
//!
//! The resolver answers exact position lookups over the in-memory projection
//! produced by `scip.rs`. Misses are intentional silence: no fuzzy matching and
//! no enclosing-range fallback, because downstream surfaces must not present a
//! lexical guess as a semantic entity.

use std::path::Path;

use anyhow::{Context, Result};

use super::meta::SubstrateMeta;
use super::scip::{self, IngestedIndex, OccurrenceEntry, SymbolData};
use super::symbols;
use super::treesitter::{parse_identity, TreeSitterFallback};
use super::{meta_path, producer_index_path_in, CHURN_FILE_NAME};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based UTF-8 byte column.
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    /// Inclusive start position.
    pub start: Position,
    /// Exclusive end position.
    pub end: Position,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionMode {
    /// Semantic resolution from the SCIP substrate.
    Scip,
    /// Reserved for the tree-sitter fallback slice.
    TreeSitter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Raw SCIP symbol string. It includes the producer's crate version.
    pub symbol: String,
    /// Human display name from `Document.symbols`, when the producer supplied one.
    pub display_name: Option<String>,
    /// SCIP symbol kind from `Document.symbols`, when known.
    pub kind: Option<String>,
    /// True when SCIP role bit 1 marks this occurrence as a definition.
    pub is_definition: bool,
    /// True for SCIP local symbols, which are not stable cross-file identities.
    pub is_local: bool,
    /// Resolver backend used to produce this result.
    pub mode: ResolutionMode,
    /// Smallest occurrence range enclosing the query position.
    pub range: SourceRange,
    /// True when HEAD differs from `SubstrateMeta::indexed_commit`.
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubstrateStats {
    pub documents: usize,
    pub occurrences: usize,
    pub definitions: usize,
    pub symbols: usize,
}

/// One workspace definition, enumerated for KG minting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionEntry {
    /// Static registry name of the producer that emitted this definition.
    pub producer: String,
    /// Raw SCIP symbol string.
    pub symbol: String,
    /// Version-normalized SCIP symbol string.
    pub normalized_symbol: String,
    pub display_name: Option<String>,
    pub kind: Option<String>,
    pub signature: Option<String>,
    /// Defining document `relative_path`.
    pub file: String,
    pub is_module: bool,
    pub is_public: bool,
}

/// One workspace definition with the source range of its definition occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDefinition {
    pub entry: DefinitionEntry,
    pub range: SourceRange,
}

#[derive(Debug)]
pub struct Substrate {
    index: IngestedIndex,
    meta: SubstrateMeta,
    /// Synthetic or live disk-backed staleness state.
    staleness: Staleness,
    syntactic: TreeSitterFallback,
    /// Optional churn/authorship sidecar (observation stratum, AD `8dd7da0c`).
    /// Absent on installs indexed before the sidecar existed — every consumer
    /// degrades to "no observations".
    churn: Option<super::ChurnIndex>,
    /// Lazy workspace-wide fan-in: normalized symbol → reference count.
    reference_counts: std::sync::OnceLock<std::collections::HashMap<String, u32>>,
}

impl Substrate {
    pub fn load(data_dir: &Path, repo_root: &Path) -> Result<Substrate> {
        let meta = SubstrateMeta::load(data_dir).with_context(|| {
            format!(
                "failed to load substrate metadata {}; run `moosedev index`",
                meta_path(data_dir).display()
            )
        })?;
        Self::load_with(data_dir, repo_root, meta)
    }

    /// Load a specific, already-sampled manifest. Callers that compare an
    /// identity before loading — and memoize failures against it — must load
    /// exactly what they sampled, not whatever the manifest says by the time
    /// the load starts.
    pub fn load_with(data_dir: &Path, repo_root: &Path, meta: SubstrateMeta) -> Result<Substrate> {
        Self::load_generation(data_dir, repo_root, meta)
    }

    fn load_generation(
        data_dir: &Path,
        repo_root: &Path,
        meta: SubstrateMeta,
    ) -> Result<Substrate> {
        let artifact_root = meta.artifact_root(data_dir)?;
        let mut merged = IngestedIndex::default();
        for producer in &meta.producers {
            let path = producer_index_path_in(&artifact_root, &producer.name);
            let index = scip::read_index(&path).with_context(|| {
                format!(
                    "failed to load substrate index for producer `{}` at {}; run `moosedev index`",
                    producer.name,
                    path.display()
                )
            })?;
            let ingested = scip::ingest(&index).with_context(|| {
                format!(
                    "failed to ingest substrate index for producer `{}`",
                    producer.name
                )
            })?;
            merged.merge(ingested, &producer.name, producer.path_prefix.as_deref())?;
        }
        let current_head = SubstrateMeta::current_head(repo_root);
        let stale = current_head != meta.indexed_commit;
        // Best-effort: a missing or unreadable sidecar never fails the load.
        let churn =
            super::ChurnIndex::load_from(&artifact_root.join(CHURN_FILE_NAME)).unwrap_or_default();
        Ok(Substrate {
            index: merged,
            meta,
            staleness: Staleness::disk_backed(repo_root, stale),
            syntactic: TreeSitterFallback::new(),
            churn,
            reference_counts: std::sync::OnceLock::new(),
        })
    }

    pub fn resolve(&self, relative_path: &str, pos: Position) -> Option<Resolution> {
        let Some(file) = self.index.files.get(relative_path) else {
            let syntactic =
                self.syntactic
                    .resolve_position(self.repo_root()?, relative_path, pos)?;
            // Every syntactic node modeled by the fallback is a declaration;
            // arbitrary syntax nodes are intentionally never returned.
            return Some(Resolution {
                symbol: syntactic.identity,
                display_name: Some(syntactic.name),
                kind: Some(syntactic.kind),
                is_definition: true,
                is_local: false,
                mode: ResolutionMode::TreeSitter,
                range: syntactic.range,
                stale: self.is_stale(),
            });
        };
        let occurrences = &file.occurrences;
        // Occurrences are sorted by start position during ingestion. `partition_point`
        // gives the first occurrence that cannot contain `pos` because it starts
        // after the query.
        let insertion = occurrences.partition_point(|entry| entry.range.start <= pos);
        let min_start_line = pos.line.saturating_sub(file.max_line_span);

        let mut best: Option<&OccurrenceEntry> = None;
        for entry in occurrences[..insertion].iter().rev() {
            // No earlier occurrence can span far enough forward to contain `pos`.
            if entry.range.start.line < min_start_line {
                break;
            }
            if !range_contains(entry.range, pos) {
                continue;
            }
            let symbol = &self.index.symbols[entry.symbol_id];
            if is_synthetic_whole_file_marker(symbol, entry.range) {
                continue;
            }
            // Nested names are common (`foo.bar`). The semantic token under the
            // cursor is the smallest enclosing range, not the first broad range.
            best = match best {
                Some(current) if range_len(entry.range) >= range_len(current.range) => {
                    Some(current)
                }
                _ => Some(entry),
            };
        }

        let entry = best?;
        let symbol = &self.index.symbols[entry.symbol_id];
        Some(Resolution {
            symbol: symbol.symbol.clone(),
            display_name: symbol.display_name.clone(),
            kind: symbol.kind.clone(),
            is_definition: scip::is_definition_role(entry.symbol_roles),
            is_local: symbol.is_local,
            mode: ResolutionMode::Scip,
            range: entry.range,
            stale: self.is_stale(),
        })
    }

    pub fn meta(&self) -> &SubstrateMeta {
        &self.meta
    }

    /// Read a source baseline only when filesystem evidence proves it did not
    /// change during or after this generation's producer run. The two metadata
    /// samples reject concurrent writes while the file is being read.
    pub fn read_indexed_source(&self, relative_path: &str) -> Option<String> {
        if !self.index.files.contains_key(relative_path) {
            return None;
        }
        if !std::path::Path::new(relative_path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return None;
        }
        let build_started_at = self.meta.indexed_started_at?;
        let path = self.repo_root()?.join(relative_path);
        let before = std::fs::metadata(&path).ok()?;
        let before_modified = before.modified().ok()?;
        if chrono::DateTime::<chrono::Utc>::from(before_modified) >= build_started_at {
            return None;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let after = std::fs::metadata(path).ok()?;
        let after_modified = after.modified().ok()?;
        if before_modified != after_modified || before.len() != after.len() {
            return None;
        }
        (chrono::DateTime::<chrono::Utc>::from(after_modified) < build_started_at).then_some(text)
    }

    pub fn is_stale(&self) -> bool {
        let Some(repo_root) = &self.staleness.repo_root else {
            return self.staleness.constructed_stale;
        };

        let mut cache = self
            .staleness
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((checked_at, stale)) = *cache {
            if checked_at.elapsed() < STALE_CHECK_TTL {
                return stale;
            }
        }

        let stale = SubstrateMeta::current_head(repo_root) != self.meta.indexed_commit;
        *cache = Some((std::time::Instant::now(), stale));
        stale
    }

    /// The repository that enables live staleness and disk reloads. Synthetic
    /// test substrates intentionally have no root and remain fully in-memory.
    pub fn repo_root(&self) -> Option<&Path> {
        self.staleness.repo_root.as_deref()
    }

    pub fn stats(&self) -> SubstrateStats {
        SubstrateStats {
            documents: self.meta.documents(),
            occurrences: self.meta.occurrences(),
            definitions: self.index.definitions,
            symbols: self.index.symbols.len(),
        }
    }

    pub fn definitions(&self) -> Vec<DefinitionEntry> {
        let mut definitions = self
            .index
            .symbols
            .iter()
            .filter_map(definition_entry)
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| a.normalized_symbol.cmp(&b.normalized_symbol));
        definitions
    }

    pub fn definitions_in_file(&self, relative_path: &str) -> Vec<FileDefinition> {
        let Some(file) = self.index.files.get(relative_path) else {
            return Vec::new();
        };

        let mut definitions = file
            .occurrences
            .iter()
            .filter(|entry| scip::is_definition_role(entry.symbol_roles))
            .filter_map(|entry| {
                let symbol = &self.index.symbols[entry.symbol_id];
                if symbol.is_local || is_synthetic_whole_file_marker(symbol, entry.range) {
                    return None;
                }
                Some(FileDefinition {
                    entry: definition_entry(symbol)?,
                    range: entry.range,
                })
            })
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| {
            a.range
                .start
                .cmp(&b.range.start)
                .then_with(|| a.entry.normalized_symbol.cmp(&b.entry.normalized_symbol))
        });
        definitions
    }

    /// Returns whether the merged substrate contains an ingested document at
    /// this exact repository-relative path.
    pub fn covers_file(&self, relative_path: &str) -> bool {
        self.index.files.contains_key(relative_path)
    }

    /// Whether a position in this path can be anchored semantically or by a
    /// supported on-disk syntactic grammar.
    pub fn can_anchor(&self, relative_path: &str) -> bool {
        self.covers_file(relative_path)
            || self.repo_root().is_some_and(|root| {
                let relative = Path::new(relative_path);
                TreeSitterFallback::supports_path(relative) && root.join(relative).is_file()
            })
    }

    /// Lists each producer and its indexed document count in metadata order.
    ///
    /// The order is stable and reflects the producer order recorded during
    /// indexing, which keeps user-facing coverage descriptions deterministic.
    pub fn coverage_summary(&self) -> Vec<(String, usize)> {
        self.meta
            .producers
            .iter()
            .map(|producer| (producer.name.clone(), producer.documents))
            .collect()
    }

    /// Formats the producer coverage summary shared by MCP dossier and
    /// `link_code` replies, or `nothing indexed` when no producers ran.
    pub fn describe_coverage(&self) -> String {
        let summary = self.coverage_summary();
        if summary.is_empty() {
            return "nothing indexed".to_string();
        }
        summary
            .into_iter()
            .map(|(name, documents)| format!("{name} {documents} docs"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn definition_for_symbol(&self, symbol: &str) -> Option<DefinitionEntry> {
        if symbol.starts_with("ts:") {
            let parsed = parse_identity(symbol)?;
            if self.identity_alive(symbol) != Some(true) {
                return None;
            }
            return Some(DefinitionEntry {
                producer: "tree-sitter".to_string(),
                symbol: symbol.to_string(),
                normalized_symbol: symbol.to_string(),
                display_name: symbols::last_descriptor_name(symbol),
                kind: Some(parsed.kind.to_string()),
                signature: None,
                file: parsed.path.to_string(),
                is_module: parsed.kind == "mod",
                // Syntactic entries exist solely for lazy anchoring and must
                // never enter the always-mint public surface.
                is_public: false,
            });
        }
        if let Some(exact) = self
            .index
            .symbols
            .iter()
            .find(|data| data.symbol == symbol)
            .and_then(definition_entry)
        {
            return Some(exact);
        }
        // Compare stable identities on both sides. Proposal nodes may come
        // from an older index and therefore carry a stale raw package version;
        // normalizing only the current indexed symbol would fail to resolve
        // that otherwise-identical target after a crate-version bump. Local
        // SCIP symbols deliberately have no normalized identity, but retain
        // their exact-lookup compatibility through the branch above.
        let normalized = symbols::normalize_symbol(symbol)?;
        self.index.symbols.iter().find_map(|data| {
            (symbols::normalize_symbol(&data.symbol).as_deref() == Some(normalized.as_str()))
                .then(|| definition_entry(data))
                .flatten()
        })
    }

    /// Check a self-describing syntactic identity against the current file.
    /// `None` means this substrate cannot verify it.
    pub fn identity_alive(&self, identity: &str) -> Option<bool> {
        self.syntactic.identity_alive(self.repo_root()?, identity)
    }

    /// Public so integration tests and tooling can inject a synthetic substrate;
    /// production code uses [`Substrate::load`].
    pub fn from_index(
        index: ::scip::types::Index,
        meta: SubstrateMeta,
        stale: bool,
    ) -> Result<Substrate> {
        let producer = meta
            .producers
            .first()
            .context("synthetic substrate metadata has no producer")?;
        let mut merged = IngestedIndex::default();
        merged.merge(
            scip::ingest(&index)?,
            &producer.name,
            producer.path_prefix.as_deref(),
        )?;
        Ok(Substrate {
            index: merged,
            meta,
            staleness: Staleness::synthetic(stale),
            syntactic: TreeSitterFallback::new(),
            churn: None,
            reference_counts: std::sync::OnceLock::new(),
        })
    }

    /// Attach a churn sidecar to a synthetic substrate (test hook).
    pub fn with_churn(mut self, churn: super::ChurnIndex) -> Substrate {
        self.churn = Some(churn);
        self
    }

    /// Churn/authorship metrics for one repo-relative file, when the sidecar
    /// exists and saw commits for it in the window.
    pub fn churn_for_file(&self, relative_path: &str) -> Option<&super::FileChurn> {
        self.churn.as_ref()?.files.get(relative_path)
    }

    /// History window the churn sidecar covers, when loaded.
    pub fn churn_window_months(&self) -> Option<u32> {
        self.churn.as_ref().map(|c| c.window_months)
    }

    /// Workspace-wide fan-in per normalized symbol: every retained
    /// non-definition occurrence of a non-local symbol (the raw material the
    /// topology Constraint `720dcd1c` reserves — references, not text).
    /// Computed once, lazily; the classifier's blast-radius signal.
    pub fn reference_counts(&self) -> &std::collections::HashMap<String, u32> {
        self.reference_counts.get_or_init(|| {
            let mut counts = std::collections::HashMap::new();
            for file in self.index.files.values() {
                for occurrence in &file.occurrences {
                    if scip::is_definition_role(occurrence.symbol_roles) {
                        continue;
                    }
                    let Some(symbol) = self.index.symbols.get(occurrence.symbol_id) else {
                        continue;
                    };
                    if symbol.is_local {
                        continue;
                    }
                    let Some(normalized) = symbols::normalize_symbol(&symbol.symbol) else {
                        continue;
                    };
                    *counts.entry(normalized).or_insert(0) += 1;
                }
            }
            counts
        })
    }

    /// Synthetic substrate with an on-disk root for fallback integration tests.
    pub fn from_index_rooted(
        index: ::scip::types::Index,
        meta: SubstrateMeta,
        stale: bool,
        repo_root: impl AsRef<Path>,
    ) -> Result<Substrate> {
        let mut substrate = Self::from_index(index, meta, stale)?;
        substrate.staleness = Staleness::disk_backed(repo_root.as_ref(), stale);
        Ok(substrate)
    }
}

/// Interval between git HEAD checks for a disk-backed substrate.
/// Public for integration tests that exercise the live-staleness cache.
pub const STALE_CHECK_TTL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug)]
struct Staleness {
    constructed_stale: bool,
    repo_root: Option<std::path::PathBuf>,
    cache: std::sync::Mutex<Option<(std::time::Instant, bool)>>,
}

impl Staleness {
    fn synthetic(constructed_stale: bool) -> Self {
        Self {
            constructed_stale,
            repo_root: None,
            cache: std::sync::Mutex::new(None),
        }
    }

    fn disk_backed(repo_root: &Path, constructed_stale: bool) -> Self {
        Self {
            constructed_stale,
            repo_root: Some(repo_root.to_path_buf()),
            cache: std::sync::Mutex::new(Some((std::time::Instant::now(), constructed_stale))),
        }
    }
}

fn definition_entry(symbol: &SymbolData) -> Option<DefinitionEntry> {
    if symbol.is_local {
        return None;
    }
    let file = symbol.defined_in.clone()?;
    let normalized_symbol = symbols::normalize_symbol(&symbol.symbol)?;
    let signature = symbol.signature.clone();
    let is_public = definition_is_public(symbol);

    Some(DefinitionEntry {
        producer: symbol.producer.clone(),
        symbol: symbol.symbol.clone(),
        normalized_symbol,
        display_name: symbol
            .display_name
            .clone()
            .or_else(|| symbols::last_descriptor_name(&symbol.symbol)),
        kind: symbol.kind.clone(),
        signature,
        file,
        is_module: symbols::is_module_symbol(&symbol.symbol),
        is_public,
    })
}

fn definition_is_public(symbol: &SymbolData) -> bool {
    match symbol.producer.as_str() {
        "rust-analyzer" => {
            // Invariant: rust-analyzer renders Rust visibility as the signature
            // prefix, so `pub`, `pub(crate)`, and `pub(super)` all match. A
            // substring check would misclassify private items whose names or
            // parameters contain "pub" (e.g. `fn ..._publishes_...()`). Items
            // with no rendered visibility (trait/impl members) are treated as
            // private; lazy minting covers them.
            symbol
                .signature
                .as_deref()
                .is_some_and(|text| text.starts_with("pub"))
        }
        // scip-typescript 0.4.0 does not encode export-ness. This structural
        // over-approximation therefore includes private top-level declarations,
        // while members and parameters remain lazy-mint-only.
        "scip-typescript" => !symbol.is_local && symbols::is_top_level_declaration(&symbol.symbol),
        // Unknown producers remain lazy-mint-only until their visibility
        // semantics have an explicit dispatch arm.
        _ => false,
    }
}

fn range_contains(range: SourceRange, pos: Position) -> bool {
    range.start <= pos && pos < range.end
}

fn is_synthetic_whole_file_marker(symbol: &SymbolData, range: SourceRange) -> bool {
    // rust-analyzer emits whole-file Module occurrences as synthetic containers.
    // They are not tokens: positions with no real token must remain honest misses.
    // Real module name tokens are single-line ranges and still resolve normally.
    symbol.kind.as_deref() == Some("Module")
        && range.start == (Position { line: 0, col: 0 })
        && range.end.line > range.start.line
}

fn range_len(range: SourceRange) -> (u32, u32, u32, u32) {
    (
        range.end.line.saturating_sub(range.start.line),
        range.end.col.saturating_sub(range.start.col),
        range.start.line,
        range.start.col,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::{DateTime, Utc};
    use protobuf::EnumOrUnknown;
    use protobuf::{Message, MessageField};
    use scip::types::{
        symbol_information, Document, Index, Metadata, Occurrence, PositionEncoding, Signature,
        SymbolInformation, ToolInfo,
    };

    use super::{Position, ResolutionMode, SourceRange, Substrate, SubstrateMeta};
    use crate::code::substrate::{producer_index_path, ProducerRun};

    #[test]
    fn position_boundaries_are_start_inclusive_end_exclusive() {
        let substrate = substrate_with_occurrences(vec![occ("s", vec![3, 5, 8], 0)]);

        assert!(substrate
            .resolve("src/lib.rs", Position { line: 3, col: 5 })
            .is_some());
        assert!(substrate
            .resolve("src/lib.rs", Position { line: 3, col: 7 })
            .is_some());
        assert!(substrate
            .resolve("src/lib.rs", Position { line: 3, col: 8 })
            .is_none());
    }

    #[test]
    fn normalizes_three_and_four_element_ranges() {
        let substrate = substrate_with_occurrences(vec![
            occ("three", vec![1, 2, 4], 0),
            occ("four", vec![2, 1, 4, 3], 0),
        ]);

        assert_eq!(
            substrate
                .resolve("src/lib.rs", Position { line: 1, col: 3 })
                .unwrap()
                .range,
            SourceRange {
                start: Position { line: 1, col: 2 },
                end: Position { line: 1, col: 4 },
            }
        );
        assert_eq!(
            substrate
                .resolve("src/lib.rs", Position { line: 3, col: 0 })
                .unwrap()
                .range,
            SourceRange {
                start: Position { line: 2, col: 1 },
                end: Position { line: 4, col: 3 },
            }
        );
    }

    #[test]
    fn nested_ranges_choose_smallest_enclosing_token() {
        let substrate = substrate_with_occurrences(vec![
            occ("outer", vec![1, 0, 20], 0),
            occ("inner", vec![1, 5, 8], 0),
        ]);

        let resolution = substrate
            .resolve("src/lib.rs", Position { line: 1, col: 6 })
            .unwrap();
        assert_eq!(resolution.symbol, "inner");
        assert_eq!(
            resolution.range,
            SourceRange {
                start: Position { line: 1, col: 5 },
                end: Position { line: 1, col: 8 },
            }
        );
    }

    #[test]
    fn miss_returns_none() {
        let substrate = substrate_with_occurrences(vec![occ("s", vec![3, 5, 8], 0)]);

        assert!(substrate
            .resolve("src/lib.rs", Position { line: 10, col: 0 })
            .is_none());
        assert!(substrate
            .resolve("missing.rs", Position { line: 3, col: 6 })
            .is_none());
    }

    #[test]
    fn reference_counts_exclude_definitions_and_locals() {
        let symbol = "rust-analyzer cargo pkg 1.0.0 mod/f().";
        let substrate = substrate_with_occurrences(vec![
            occ(symbol, vec![0, 0, 5], 1),    // definition role — not fan-in
            occ(symbol, vec![1, 0, 5], 0),    // reference
            occ(symbol, vec![2, 0, 5], 0),    // reference
            occ("local 3", vec![3, 0, 5], 0), // local — not a stable identity
        ]);

        let counts = substrate.reference_counts();
        assert_eq!(
            counts.get("rust-analyzer cargo pkg . mod/f()."),
            Some(&2),
            "two references, version-normalized key"
        );
        assert_eq!(counts.len(), 1, "definitions and locals excluded");
    }

    #[test]
    fn scip_covered_file_never_uses_syntactic_fallback() {
        let symbol = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.occurrences.push(occ(symbol, vec![0, 3, 15], 1));
        index.documents.push(document);

        let root = std::env::temp_dir().join(format!(
            "moosedev-covered-scip-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/runtime.rs"), "fn build_server() {}\n").unwrap();
        let substrate = Substrate::from_index_rooted(index, meta(), false, &root).unwrap();

        let resolution = substrate
            .resolve("src/runtime.rs", Position { line: 0, col: 4 })
            .unwrap();
        assert_eq!(resolution.mode, ResolutionMode::Scip);
        assert_eq!(resolution.symbol, symbol);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn synthetic_whole_file_module_marker_does_not_turn_miss_into_file_wide_hit() {
        let mut index = Index::new();
        let mut document = doc("src/lib.rs");

        let module_symbol = "rust-analyzer cargo moosedev 0.6.3 runtime/";
        document.symbols.push(info(
            module_symbol,
            "runtime",
            symbol_information::Kind::Module,
            "pub mod runtime",
        ));
        document
            .occurrences
            .push(occ(module_symbol, vec![0, 0, 10, 0], 1));

        let function_symbol = "rust-analyzer cargo moosedev 0.6.3 parse_mode().";
        let mut function_info = SymbolInformation::new();
        function_info.symbol = function_symbol.to_string();
        function_info.display_name = "parse_mode".to_string();
        function_info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
        document.symbols.push(function_info);
        document
            .occurrences
            .push(occ(function_symbol, vec![3, 5, 15], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        assert!(substrate
            .resolve("src/lib.rs", Position { line: 1, col: 0 })
            .is_none());
        assert_eq!(
            substrate
                .resolve("src/lib.rs", Position { line: 3, col: 6 })
                .unwrap()
                .symbol,
            function_symbol
        );
    }

    #[test]
    fn narrow_single_line_module_reference_resolves() {
        let module_symbol = "rust-analyzer cargo moosedev 0.6.3 runtime/";
        let mut index = Index::new();
        let mut document = doc("src/lib.rs");
        document.symbols.push(info(
            module_symbol,
            "runtime",
            symbol_information::Kind::Module,
            "mod runtime;",
        ));
        document
            .occurrences
            .push(occ(module_symbol, vec![2, 4, 11], 0));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();

        assert_eq!(
            substrate
                .resolve("src/lib.rs", Position { line: 2, col: 5 })
                .unwrap()
                .symbol,
            module_symbol
        );
    }

    #[test]
    fn local_symbols_are_flagged() {
        let substrate = substrate_with_occurrences(vec![occ("local 1", vec![0, 0, 1], 0)]);

        assert!(
            substrate
                .resolve("src/lib.rs", Position { line: 0, col: 0 })
                .unwrap()
                .is_local
        );
    }

    #[test]
    fn definition_bit_is_exposed() {
        let substrate = substrate_with_occurrences(vec![occ("s", vec![0, 0, 1], 1)]);

        assert!(
            substrate
                .resolve("src/lib.rs", Position { line: 0, col: 0 })
                .unwrap()
                .is_definition
        );
    }

    #[test]
    fn synthetic_substrate_preserves_constructed_staleness() {
        let index = Index::new();

        assert!(!Substrate::from_index(index.clone(), meta(), false)
            .unwrap()
            .is_stale());
        assert!(Substrate::from_index(index, meta(), true)
            .unwrap()
            .is_stale());
    }

    #[test]
    fn unsorted_input_is_handled() {
        let substrate = substrate_with_occurrences(vec![
            occ("later", vec![3, 0, 4], 0),
            occ("earlier", vec![1, 0, 4], 0),
        ]);

        assert_eq!(
            substrate
                .resolve("src/lib.rs", Position { line: 1, col: 2 })
                .unwrap()
                .symbol,
            "earlier"
        );
    }

    #[test]
    fn symbol_info_first_wins_even_when_seen_after_reference_document() {
        let mut index = Index::new();
        let mut reference_doc = doc("src/ref.rs");
        reference_doc
            .occurrences
            .push(occ("global sym.", vec![0, 0, 3], 0));
        index.documents.push(reference_doc);

        let mut definition_doc = doc("src/def.rs");
        let mut first = SymbolInformation::new();
        first.symbol = "global sym.".to_string();
        first.display_name = "first".to_string();
        first.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
        definition_doc.symbols.push(first);

        let mut duplicate = SymbolInformation::new();
        duplicate.symbol = "global sym.".to_string();
        duplicate.display_name = "duplicate".to_string();
        duplicate.kind = EnumOrUnknown::new(symbol_information::Kind::Class);
        definition_doc.symbols.push(duplicate);
        index.documents.push(definition_doc);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let resolution = substrate
            .resolve("src/ref.rs", Position { line: 0, col: 1 })
            .unwrap();

        assert_eq!(resolution.display_name.as_deref(), Some("first"));
        assert_eq!(resolution.kind.as_deref(), Some("Function"));
    }

    #[test]
    fn definitions_include_defining_file_and_signature() {
        let symbol = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            symbol,
            "build_server",
            symbol_information::Kind::Function,
            "pub fn build_server()",
        ));
        document.occurrences.push(occ(symbol, vec![7, 4, 16], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let definitions = substrate.definitions();

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].symbol, symbol);
        assert_eq!(
            definitions[0].normalized_symbol,
            "rust-analyzer cargo moosedev . runtime/build_server()."
        );
        assert_eq!(definitions[0].display_name.as_deref(), Some("build_server"));
        assert_eq!(definitions[0].kind.as_deref(), Some("Function"));
        assert_eq!(
            definitions[0].signature.as_deref(),
            Some("pub fn build_server()")
        );
        assert_eq!(definitions[0].file, "src/runtime.rs");
        assert!(!definitions[0].is_module);
        assert!(definitions[0].is_public);
    }

    #[test]
    fn definition_lookup_accepts_a_stale_raw_package_version() {
        let current = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
        let stale = "rust-analyzer cargo moosedev 0.5.0 runtime/build_server().";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            current,
            "build_server",
            symbol_information::Kind::Function,
            "pub fn build_server()",
        ));
        document.occurrences.push(occ(current, vec![7, 4, 16], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let definition = substrate
            .definition_for_symbol(stale)
            .expect("stale raw version resolves through normalized identity");

        assert_eq!(definition.symbol, current);
        assert_eq!(
            definition.normalized_symbol,
            "rust-analyzer cargo moosedev . runtime/build_server()."
        );
    }

    #[test]
    fn typescript_definition_uses_descriptor_name_and_fenced_declaration() {
        let symbol =
            "scip-typescript npm moosedev-ui 0.6.3 src/pages/`RecordPage.tsx`/RecordPage().";
        let mut index = Index::new();
        set_tool_name(&mut index, "scip-typescript");
        let mut document = doc("src/vis.ts");
        let mut symbol_info = SymbolInformation::new();
        symbol_info.symbol = symbol.to_string();
        symbol_info.documentation =
            vec!["```ts\nfunction exportedFn(x: number): number\n```".to_string()];
        document.symbols.push(symbol_info);
        document.occurrences.push(occ(symbol, vec![0, 9, 19], 1));
        index.documents.push(document);

        let definition = Substrate::from_index(index, meta_for("scip-typescript"), false)
            .unwrap()
            .definitions()
            .remove(0);

        assert_eq!(definition.display_name.as_deref(), Some("RecordPage"));
        assert_eq!(
            definition.signature.as_deref(),
            Some("function exportedFn(x: number): number")
        );
        assert!(definition.is_public);
    }

    #[test]
    fn non_typescript_documentation_is_not_used_as_a_signature() {
        let symbol = "rust-analyzer cargo moosedev 0.6.3 runtime/helper().";
        let mut index = Index::new();
        set_tool_name(&mut index, "rust-analyzer");
        let mut document = doc("src/runtime.rs");
        let mut symbol_info = SymbolInformation::new();
        symbol_info.symbol = symbol.to_string();
        symbol_info.documentation = vec!["```ts\nfn helper()\n```".to_string()];
        document.symbols.push(symbol_info);
        document.occurrences.push(occ(symbol, vec![0, 0, 6], 1));
        index.documents.push(document);

        let definition = Substrate::from_index(index, meta(), false)
            .unwrap()
            .definitions()
            .remove(0);

        assert_eq!(definition.signature, None);
        assert!(!definition.is_public);
    }

    #[test]
    fn definitions_skip_reference_only_and_local_symbols() {
        let global = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
        let local = "local 1";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            global,
            "build_server",
            symbol_information::Kind::Function,
            "pub fn build_server()",
        ));
        document.occurrences.push(occ(global, vec![0, 0, 12], 0));
        document.symbols.push(info(
            local,
            "tmp",
            symbol_information::Kind::Variable,
            "let tmp",
        ));
        document.occurrences.push(occ(local, vec![1, 4, 7], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();

        assert!(substrate.definitions().is_empty());
    }

    #[test]
    fn definitions_are_sorted_by_normalized_symbol() {
        let b = "rust-analyzer cargo moosedev 0.6.3 runtime/b().";
        let a = "rust-analyzer cargo moosedev 0.6.3 runtime/a().";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document
            .symbols
            .push(info(b, "b", symbol_information::Kind::Function, "fn b()"));
        document.occurrences.push(occ(b, vec![0, 0, 1], 1));
        document
            .symbols
            .push(info(a, "a", symbol_information::Kind::Function, "fn a()"));
        document.occurrences.push(occ(a, vec![1, 0, 1], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let symbols = substrate
            .definitions()
            .into_iter()
            .map(|definition| definition.display_name.unwrap())
            .collect::<Vec<_>>();

        assert_eq!(symbols, vec!["a", "b"]);
    }

    #[test]
    fn definitions_public_flag_follows_signature_text() {
        let public = "rust-analyzer cargo moosedev 0.6.3 runtime/public().";
        let private = "rust-analyzer cargo moosedev 0.6.3 runtime/private().";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            public,
            "public",
            symbol_information::Kind::Function,
            "pub(crate) fn public()",
        ));
        document.occurrences.push(occ(public, vec![0, 0, 6], 1));
        document.symbols.push(info(
            private,
            "private",
            symbol_information::Kind::Function,
            "fn private()",
        ));
        document.occurrences.push(occ(private, vec![1, 0, 7], 1));
        // Private, but "pub" appears as a substring of the name and a
        // parameter — must not be classified public.
        let pub_substring = "rust-analyzer cargo moosedev 0.6.3 runtime/publishes_port().";
        document.symbols.push(info(
            pub_substring,
            "publishes_port",
            symbol_information::Kind::Function,
            "fn publishes_port(publish: bool)",
        ));
        document
            .occurrences
            .push(occ(pub_substring, vec![2, 0, 14], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let definitions = substrate.definitions();

        assert_eq!(definitions.len(), 3);
        let private_definition = definitions
            .iter()
            .find(|definition| definition.display_name.as_deref() == Some("private"))
            .unwrap();
        let public_definition = definitions
            .iter()
            .find(|definition| definition.display_name.as_deref() == Some("public"))
            .unwrap();
        let pub_substring_definition = definitions
            .iter()
            .find(|definition| definition.display_name.as_deref() == Some("publishes_port"))
            .unwrap();
        assert!(!private_definition.is_public);
        assert!(public_definition.is_public);
        assert!(!pub_substring_definition.is_public);
    }

    #[test]
    fn typescript_public_gate_keeps_only_top_level_declarations() {
        let fixtures = [
            (
                "scip-typescript npm moosedev-ui 0.6.3 src/pages/`RecordPage.tsx`/RecordPage().",
                true,
            ),
            (
                "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/ExportedIface#",
                true,
            ),
            (
                "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/ExportedIface#a.",
                false,
            ),
            (
                "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/ExportedClass#method().",
                false,
            ),
            (
                "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/exportedFn().(x)",
                false,
            ),
        ];
        let mut index = Index::new();
        set_tool_name(&mut index, "scip-typescript");
        let mut document = doc("src/vis.ts");
        for (line, (symbol, _)) in fixtures.iter().enumerate() {
            let mut symbol_info = SymbolInformation::new();
            symbol_info.symbol = (*symbol).to_string();
            document.symbols.push(symbol_info);
            document
                .occurrences
                .push(occ(symbol, vec![line as i32, 0, 1], 1));
        }
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta_for("scip-typescript"), false).unwrap();
        for (symbol, expected) in fixtures {
            assert_eq!(
                substrate.definition_for_symbol(symbol).unwrap().is_public,
                expected,
                "{symbol}"
            );
        }
    }

    #[test]
    fn unspecified_position_encodings_are_accepted_for_an_index() {
        let mut index = Index::new();
        for n in 0..3 {
            let mut document = Document::new();
            document.relative_path = format!("src/{n}.ts");
            index.documents.push(document);
        }

        assert!(Substrate::from_index(index, meta_for("scip-typescript"), false).is_ok());
    }

    #[test]
    fn definitions_identify_modules() {
        let module = "rust-analyzer cargo moosedev 0.6.3 runtime/";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            module,
            "runtime",
            symbol_information::Kind::Module,
            "pub mod runtime",
        ));
        document.occurrences.push(occ(module, vec![0, 0, 7], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();

        assert!(substrate.definitions()[0].is_module);
    }

    #[test]
    fn definitions_include_whole_file_module_definitions() {
        let module = "rust-analyzer cargo moosedev 0.6.3 runtime/";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            module,
            "runtime",
            symbol_information::Kind::Module,
            "pub mod runtime",
        ));
        document.occurrences.push(occ(module, vec![0, 0, 30, 0], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let definitions = substrate.definitions();

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].symbol, module);
        assert_eq!(definitions[0].file, "src/runtime.rs");
        assert!(definitions[0].is_module);
    }

    #[test]
    fn definitions_in_file_excludes_whole_file_module_and_keeps_token_range() {
        let module = "rust-analyzer cargo moosedev 0.6.3 runtime/";
        let function = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
        let mut index = Index::new();
        let mut document = doc("src/runtime.rs");
        document.symbols.push(info(
            module,
            "runtime",
            symbol_information::Kind::Module,
            "pub mod runtime",
        ));
        document.occurrences.push(occ(module, vec![0, 0, 30, 0], 1));
        document.symbols.push(info(
            function,
            "build_server",
            symbol_information::Kind::Function,
            "pub fn build_server()",
        ));
        document.occurrences.push(occ(function, vec![7, 4, 16], 1));
        index.documents.push(document);

        let substrate = Substrate::from_index(index, meta(), false).unwrap();
        let definitions = substrate.definitions_in_file("src/runtime.rs");

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].entry.symbol, function);
        assert_eq!(
            definitions[0].range,
            SourceRange {
                start: Position { line: 7, col: 4 },
                end: Position { line: 7, col: 16 },
            }
        );
    }

    #[test]
    fn utf16_document_errors() {
        let mut index = Index::new();
        let mut document = doc("src/lib.rs");
        document.position_encoding =
            EnumOrUnknown::new(PositionEncoding::UTF16CodeUnitOffsetFromLineStart);
        document.occurrences.push(occ("s", vec![0, 0, 1], 0));
        index.documents.push(document);

        let err = Substrate::from_index(index, meta(), false).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported SCIP position_encoding"));
    }

    #[test]
    fn load_merges_producers_with_prefix_and_isolated_symbol_tables() {
        let data_dir = unique_temp_dir("merged-load");
        let first_symbol = "rust-analyzer cargo first 1.0.0 first().";
        let second_symbol = "other tool second 1.0.0 second().";
        write_index(
            &data_dir,
            "first",
            index_with_definition("src/first.rs", first_symbol),
        );
        write_index(
            &data_dir,
            "second",
            index_with_definition("src/second.rs", second_symbol),
        );
        let meta = multi_meta(vec![
            producer_run("first", None),
            producer_run("second", Some("ui/")),
        ]);
        meta.save(&data_dir).unwrap();

        let substrate = Substrate::load(&data_dir, &data_dir).unwrap();
        assert!(substrate.covers_file("src/first.rs"));
        assert!(substrate.covers_file("ui/src/second.rs"));
        assert!(!substrate.covers_file("src/absent.rs"));
        assert_eq!(
            substrate
                .resolve("src/first.rs", Position { line: 0, col: 0 })
                .unwrap()
                .symbol,
            first_symbol
        );
        assert_eq!(
            substrate
                .resolve("ui/src/second.rs", Position { line: 0, col: 0 })
                .unwrap()
                .symbol,
            second_symbol
        );
        let second = substrate
            .definitions()
            .into_iter()
            .find(|definition| definition.producer == "second")
            .unwrap();
        assert_eq!(second.file, "ui/src/second.rs");
        assert!(!second.is_public, "unknown producers are lazy-mint-only");

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn coverage_summary_and_description_follow_producer_order() {
        let data_dir = unique_temp_dir("coverage-summary");
        let mut rust = producer_run("rust-analyzer", None);
        rust.documents = 104;
        let mut typescript = producer_run("scip-typescript", Some("ui/"));
        typescript.documents = 39;
        multi_meta(vec![rust, typescript]).save(&data_dir).unwrap();
        write_index(&data_dir, "rust-analyzer", Index::new());
        write_index(&data_dir, "scip-typescript", Index::new());

        let substrate = Substrate::load(&data_dir, &data_dir).unwrap();
        assert_eq!(
            substrate.coverage_summary(),
            vec![
                ("rust-analyzer".to_string(), 104),
                ("scip-typescript".to_string(), 39),
            ]
        );
        assert_eq!(
            substrate.describe_coverage(),
            "rust-analyzer 104 docs, scip-typescript 39 docs"
        );

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn empty_coverage_description_is_explicit() {
        let data_dir = unique_temp_dir("empty-coverage");
        multi_meta(Vec::new()).save(&data_dir).unwrap();

        let substrate = Substrate::load(&data_dir, &data_dir).unwrap();
        assert_eq!(substrate.describe_coverage(), "nothing indexed");

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn load_rejects_re_rooted_path_collisions_with_both_producers() {
        let data_dir = unique_temp_dir("path-collision");
        write_index(
            &data_dir,
            "first",
            index_with_definition("src/shared.rs", "rust-analyzer cargo first 1.0.0 first()."),
        );
        write_index(
            &data_dir,
            "second",
            index_with_definition("src/shared.rs", "other tool second 1.0.0 second()."),
        );
        multi_meta(vec![
            producer_run("first", None),
            producer_run("second", None),
        ])
        .save(&data_dir)
        .unwrap();

        let error = Substrate::load(&data_dir, &data_dir)
            .unwrap_err()
            .to_string();
        assert!(error.contains("first"), "{error}");
        assert!(error.contains("second"), "{error}");
        assert!(error.contains("src/shared.rs"), "{error}");

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn load_rejects_listed_but_missing_index() {
        let data_dir = unique_temp_dir("missing-listed-index");
        multi_meta(vec![producer_run("missing", None)])
            .save(&data_dir)
            .unwrap();

        let error = Substrate::load(&data_dir, &data_dir)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing"), "{error}");
        assert!(error.contains("run `moosedev index`"), "{error}");

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn load_with_uses_the_sampled_manifest_exactly() {
        let data_dir = unique_temp_dir("sampled-load");
        write_index(
            &data_dir,
            "latest",
            index_with_definition("src/lib.rs", "rust-analyzer cargo latest 1.0.0 item()."),
        );
        multi_meta(vec![producer_run("latest", None)])
            .save(&data_dir)
            .unwrap();
        let sampled = multi_meta(vec![producer_run("missing", None)]);

        let error = Substrate::load_with(&data_dir, &data_dir, sampled)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing"), "{error}");

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn indexed_source_requires_a_stable_file_older_than_the_build() {
        let data_dir = unique_temp_dir("indexed-source");
        let source_path = data_dir.join("src/lib.rs");
        std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        std::fs::write(&source_path, "pub fn item() {}\n").unwrap();
        write_index(
            &data_dir,
            "rust-analyzer",
            index_with_definition("src/lib.rs", "rust-analyzer cargo test 1.0.0 item()."),
        );

        let mut trusted_meta = multi_meta(vec![producer_run("rust-analyzer", None)]);
        trusted_meta.indexed_started_at = Some(Utc::now() + chrono::Duration::seconds(2));
        trusted_meta.save(&data_dir).unwrap();
        let trusted = Substrate::load(&data_dir, &data_dir).unwrap();
        assert_eq!(
            trusted.read_indexed_source("src/lib.rs").as_deref(),
            Some("pub fn item() {}\n")
        );
        assert_eq!(trusted.read_indexed_source("src/missing.rs"), None);

        let mut untrusted_meta = trusted_meta;
        untrusted_meta.indexed_started_at = Some(Utc::now() - chrono::Duration::seconds(2));
        untrusted_meta.save(&data_dir).unwrap();
        let untrusted = Substrate::load(&data_dir, &data_dir).unwrap();
        assert_eq!(untrusted.read_indexed_source("src/lib.rs"), None);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    fn write_index(data_dir: &Path, producer: &str, index: Index) {
        let path = producer_index_path(data_dir, producer);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, index.write_to_bytes().unwrap()).unwrap();
    }

    fn index_with_definition(path: &str, symbol: &str) -> Index {
        let mut index = Index::new();
        let mut document = doc(path);
        document.symbols.push(info(
            symbol,
            "item",
            symbol_information::Kind::Function,
            "pub fn item()",
        ));
        document.occurrences.push(occ(symbol, vec![0, 0, 4], 1));
        index.documents.push(document);
        index
    }

    fn producer_run(name: &str, path_prefix: Option<&str>) -> ProducerRun {
        ProducerRun {
            name: name.to_string(),
            producer: name.to_string(),
            producer_version: "1".to_string(),
            mode: "scip".to_string(),
            documents: 1,
            occurrences: 1,
            path_prefix: path_prefix.map(str::to_string),
        }
    }

    fn multi_meta(producers: Vec<ProducerRun>) -> SubstrateMeta {
        SubstrateMeta {
            schema_version: crate::code::substrate::meta::LEGACY_SCHEMA_VERSION,
            indexed_commit: "unknown".to_string(),
            indexed_at: Utc::now(),
            indexed_started_at: None,
            generation: None,
            producers,
        }
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-substrate-resolver-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn substrate_with_occurrences(occurrences: Vec<Occurrence>) -> Substrate {
        let mut index = Index::new();
        let mut document = doc("src/lib.rs");
        for occurrence in &occurrences {
            let mut info = SymbolInformation::new();
            info.symbol = occurrence.symbol.clone();
            info.display_name = occurrence.symbol.clone();
            info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
            document.symbols.push(info);
        }
        document.occurrences = occurrences;
        index.documents.push(document);

        Substrate::from_index(index, meta(), true).unwrap()
    }

    fn doc(relative_path: &str) -> Document {
        let mut document = Document::new();
        document.relative_path = relative_path.to_string();
        document.position_encoding =
            EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        document
    }

    fn occ(symbol: &str, range: Vec<i32>, symbol_roles: i32) -> Occurrence {
        let mut occurrence = Occurrence::new();
        occurrence.symbol = symbol.to_string();
        occurrence.range = range;
        occurrence.symbol_roles = symbol_roles;
        occurrence.enclosing_range = vec![0, 0, 10, 0];
        occurrence
    }

    fn info(
        symbol: &str,
        display_name: &str,
        kind: symbol_information::Kind,
        signature: &str,
    ) -> SymbolInformation {
        let mut info = SymbolInformation::new();
        info.symbol = symbol.to_string();
        info.display_name = display_name.to_string();
        info.kind = EnumOrUnknown::new(kind);
        let mut signature_documentation = Signature::new();
        signature_documentation.text = signature.to_string();
        info.signature_documentation = MessageField::some(signature_documentation);
        info
    }

    fn meta() -> SubstrateMeta {
        meta_for("rust-analyzer")
    }

    fn meta_for(producer: &str) -> SubstrateMeta {
        SubstrateMeta::single(
            producer,
            "abc123",
            DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
                .unwrap()
                .with_timezone(&Utc),
            1,
            1,
        )
    }

    fn set_tool_name(index: &mut Index, name: &str) {
        let mut tool = ToolInfo::new();
        tool.name = name.to_string();
        let mut metadata = Metadata::new();
        metadata.tool_info = MessageField::some(tool);
        index.metadata = MessageField::some(metadata);
    }
}
