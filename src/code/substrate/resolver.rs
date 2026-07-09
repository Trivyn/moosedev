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
use super::{index_path, meta_path};

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

#[derive(Debug, Clone)]
pub struct Substrate {
    index: IngestedIndex,
    meta: SubstrateMeta,
    /// Computed once at load time and copied into every resolution.
    stale: bool,
}

impl Substrate {
    pub fn load(data_dir: &Path, repo_root: &Path) -> Result<Substrate> {
        let path = index_path(data_dir);
        let index = scip::read_index(&path).with_context(|| {
            format!(
                "failed to load substrate index {}; run `moosedev index`",
                path.display()
            )
        })?;
        let meta = SubstrateMeta::load(data_dir).with_context(|| {
            format!(
                "failed to load substrate metadata {}; run `moosedev index`",
                meta_path(data_dir).display()
            )
        })?;
        let current_head = SubstrateMeta::current_head(repo_root);
        let stale = current_head != meta.indexed_commit;
        Substrate::from_index(index, meta, stale)
    }

    pub fn resolve(&self, relative_path: &str, pos: Position) -> Option<Resolution> {
        let file = self.index.files.get(relative_path)?;
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
            stale: self.stale,
        })
    }

    pub fn meta(&self) -> &SubstrateMeta {
        &self.meta
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn stats(&self) -> SubstrateStats {
        SubstrateStats {
            documents: self.index.documents,
            occurrences: self.index.occurrences,
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

    pub fn definition_for_symbol(&self, symbol: &str) -> Option<DefinitionEntry> {
        self.index.symbols.iter().find_map(|data| {
            (data.symbol == symbol
                || symbols::normalize_symbol(&data.symbol).as_deref() == Some(symbol))
            .then(|| definition_entry(data))
            .flatten()
        })
    }

    /// Public so integration tests and tooling can inject a synthetic substrate;
    /// production code uses [`Substrate::load`].
    pub fn from_index(
        index: ::scip::types::Index,
        meta: SubstrateMeta,
        stale: bool,
    ) -> Result<Substrate> {
        let index = scip::ingest(&index)?;
        Ok(Substrate { index, meta, stale })
    }
}

fn definition_entry(symbol: &SymbolData) -> Option<DefinitionEntry> {
    if symbol.is_local {
        return None;
    }
    let file = symbol.defined_in.clone()?;
    let normalized_symbol = symbols::normalize_symbol(&symbol.symbol)?;
    let signature = symbol.signature.clone();
    // rust-analyzer renders Rust visibility as the signature prefix, so `pub`,
    // `pub(crate)`, and `pub(super)` all match. A substring check would
    // misclassify private items whose names or parameters contain "pub"
    // (e.g. `fn ..._publishes_...()`). Items with no rendered visibility
    // (trait/impl members) are treated as private; lazy minting covers them.
    let is_public = signature
        .as_deref()
        .is_some_and(|text| text.starts_with("pub"));

    Some(DefinitionEntry {
        symbol: symbol.symbol.clone(),
        normalized_symbol,
        display_name: symbol.display_name.clone(),
        kind: symbol.kind.clone(),
        signature,
        file,
        is_module: symbols::is_module_symbol(&symbol.symbol),
        is_public,
    })
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
    use chrono::{DateTime, Utc};
    use protobuf::EnumOrUnknown;
    use protobuf::MessageField;
    use scip::types::{
        symbol_information, Document, Index, Occurrence, PositionEncoding, Signature,
        SymbolInformation,
    };

    use super::{Position, SourceRange, Substrate, SubstrateMeta};
    use crate::code::substrate::meta::CURRENT_SCHEMA_VERSION;

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
        SubstrateMeta {
            schema_version: CURRENT_SCHEMA_VERSION,
            indexed_commit: "abc123".to_string(),
            indexed_at: DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
                .unwrap()
                .with_timezone(&Utc),
            producer: "rust-analyzer".to_string(),
            producer_version: "1.0.0".to_string(),
            mode: "scip".to_string(),
            documents: 1,
            occurrences: 1,
        }
    }
}
