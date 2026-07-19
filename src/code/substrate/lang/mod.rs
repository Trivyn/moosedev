//! Per-language registry for the substrate.
//!
//! Everything language-specific lives in one module per language: the SCIP
//! producer registration and its idiom hooks (visibility contract, symbol
//! canonicalization, signature fence) plus the tree-sitter fallback grammar
//! and its node tables. The rest of the substrate dispatches through this
//! registry, so adding a language is one new module here plus one row in
//! `LANGUAGES` — no edits to producer/resolver/scip/treesitter.

pub(crate) mod python;
pub(crate) mod rust;
pub(crate) mod typescript;

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use super::producer::{ProducerSpec, ProducerTarget};
use super::scip::SymbolData;

pub(crate) struct LanguageSpec {
    /// SCIP producer half; None for fallback-only languages.
    pub producer: Option<ProducerHooks>,
    /// Tree-sitter syntactic fallback half; None when no grammar is registered.
    pub fallback: Option<FallbackSpec>,
}

pub(crate) struct ProducerHooks {
    /// Registry entry. `spec.name` doubles as the SCIP `tool_info.name` the
    /// producer stamps into its index — ingest-time hooks key on it.
    pub spec: ProducerSpec,
    /// Visibility contract for this producer's definitions (batch-mint gate).
    pub is_public: fn(&SymbolData) -> bool,
    /// Rewrite a producer-idiom symbol into canonical SCIP grammar (None =
    /// symbol unchanged). Applied at the shared identity boundary via
    /// `lang::canonical_symbol` — ingest, minting, and caller-provided symbol
    /// lookups all converge on the canonical form.
    pub canonical_symbol: Option<fn(&str) -> Option<String>>,
    /// Fence language when the producer renders declarations as fenced
    /// `documentation` blocks instead of `signature_documentation`.
    pub signature_fence: Option<&'static str>,
}

pub(crate) struct FallbackSpec {
    pub extensions: &'static [&'static str],
    /// Identity language tag: `ts:<tag>:<path>:<kind>:<qualified-name>`.
    pub tag: &'static str,
    pub grammar: fn() -> tree_sitter::Language,
    /// Tree-sitter node kind → identity kind for anchorable declarations.
    pub declaration_kind: fn(&str) -> Option<&'static str>,
    /// Identity kinds this language can emit (`parse_identity` validation).
    pub identity_kinds: &'static [&'static str],
    /// Language-specific declaration naming; a None result (or None hook)
    /// falls back to the node's `name` field.
    pub declaration_name: Option<fn(tree_sitter::Node<'_>, &str) -> Option<String>>,
}

static LANGUAGES: [&LanguageSpec; 3] = [&rust::LANGUAGE, &typescript::LANGUAGE, &python::LANGUAGE];

/// Producer registry in `LANGUAGES` order (stable: meta.json + tests rely on it).
pub(crate) fn producer_registry() -> &'static [ProducerSpec] {
    static SPECS: OnceLock<Vec<ProducerSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        LANGUAGES
            .iter()
            .filter_map(|language| language.producer.as_ref())
            .map(|hooks| hooks.spec)
            .collect()
    })
}

pub(crate) fn producer_hooks(producer_name: &str) -> Option<&'static ProducerHooks> {
    LANGUAGES
        .iter()
        .filter_map(|language| language.producer.as_ref())
        .find(|hooks| hooks.spec.name == producer_name)
}

/// Producer canonicalization at the identity boundary. A global SCIP symbol's
/// scheme (its first space-delimited token) is the producer name, so idiom
/// symbols (e.g. scip-python's `pkg/__init__:` module marker) rewrite
/// identically wherever a symbol enters — ingest, KG minting, and raw symbols
/// supplied by dossier/link/proposal callers.
pub(crate) fn canonical_symbol(raw: &str) -> Option<String> {
    let scheme = raw.split(' ').next()?;
    producer_hooks(scheme)?
        .canonical_symbol
        .and_then(|hook| hook(raw))
}

pub(crate) fn fallback_for_path(path: &Path) -> Option<&'static FallbackSpec> {
    let extension = path.extension()?.to_str()?;
    LANGUAGES
        .iter()
        .filter_map(|language| language.fallback.as_ref())
        .find(|fallback| fallback.extensions.contains(&extension))
}

pub(crate) fn fallback_for_tag(tag: &str) -> Option<&'static FallbackSpec> {
    LANGUAGES
        .iter()
        .filter_map(|language| language.fallback.as_ref())
        .find(|fallback| fallback.tag == tag)
}

/// Shared detect shape: the first (sorted) first-level subdirectory that is a
/// project, skipping `node_modules` and dotdirs. Root handling stays with the
/// caller because root markers differ per language.
pub(crate) fn first_matching_subdir(
    repo_root: &Path,
    is_project: fn(&Path) -> bool,
) -> Option<ProducerTarget> {
    let mut directories = fs::read_dir(repo_root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name != "node_modules" && !name.starts_with('.')
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| entry.file_name());

    directories.into_iter().find_map(|entry| {
        let project_dir = entry.path();
        is_project(&project_dir).then(|| ProducerTarget {
            project_dir,
            path_prefix: Some(format!("{}/", entry.file_name().to_string_lossy())),
        })
    })
}
