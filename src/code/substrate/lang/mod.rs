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
    /// Zed language names this language covers. Zed is the one client that
    /// bakes a language list into its extension manifest (every other client
    /// attaches broadly and relies on server-side silence for non-substrate
    /// files), so the names live here — in the registry — and a test keeps
    /// `clients/zed/extension.toml` from drifting.
    #[cfg_attr(not(test), allow(dead_code))] // read by the extension.toml sync test
    pub zed_languages: &'static [&'static str],
    /// Whether a repo-relative path is THIS language's test code, for idioms a
    /// shared rule cannot express: pytest's `test_*.py`, Jest's `*.spec.ts`,
    /// Rust's `tests.rs`. `None` when the language adds nothing to the shared
    /// directory conventions.
    pub is_test_path: Option<fn(&str) -> bool>,
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

/// Whether a repo-relative path is test code.
///
/// DIRECTORY conventions are broadly shared, so they are answered here. NAMING
/// idioms are not — `test_*.py`, `*.spec.ts`, `*_test.go` all mean the same
/// thing in different languages and nothing in another — so each language
/// answers for its own. A path whose extension the registry does not recognize
/// gets the shared rules only, which is the honest answer for a language whose
/// idioms this build does not know.
///
/// KNOWN LIMIT: Rust's dominant convention is an inline `#[cfg(test)] mod
/// tests`, which is not a path at all. No path predicate can see it — it is
/// visible only in a symbol's module descriptor, so a symbol-level check would
/// be needed to exclude it.
pub(crate) fn is_test_path(path: &str) -> bool {
    if shared_test_path(path) {
        return true;
    }
    language_for_path(path)
        .and_then(|language| language.is_test_path)
        .is_some_and(|hook| hook(path))
}

/// Final path segment. The languages' test-naming hooks all key on it.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Directory conventions that mean test code in essentially every language.
///
/// Deliberately NOT included: a `spec`/`specs` segment. It is a test directory
/// in Ruby but an interface-description directory elsewhere — this repository's
/// own `spec/` holds specifications, not tests — so it is left to the languages
/// that actually mean tests by it.
fn shared_test_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path
            .split('/')
            .any(|segment| matches!(segment, "test" | "tests" | "__tests__"))
}

/// The registered language owning a path, by extension. Checks both halves so a
/// producer-only or fallback-only language still resolves.
fn language_for_path(path: &str) -> Option<&'static LanguageSpec> {
    let extension = Path::new(path).extension()?.to_str()?;
    LANGUAGES.iter().copied().find(|language| {
        language
            .fallback
            .as_ref()
            .is_some_and(|fallback| fallback.extensions.contains(&extension))
            || language
                .producer
                .as_ref()
                .is_some_and(|producer| producer.spec.extensions.contains(&extension))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `clients/zed/extension.toml` must carry exactly the registry's Zed
    /// language names: adding a language is one module + one `LANGUAGES` row,
    /// and this test points at the single client file that cannot derive its
    /// list at runtime.
    #[test]
    fn zed_extension_languages_match_registry() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("clients/zed/extension.toml");
        let manifest = fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
        let languages_line = manifest
            .lines()
            .find(|line| line.trim_start().starts_with("languages"))
            .expect("extension.toml declares a languages list");
        let declared: Vec<&str> = languages_line.split('"').skip(1).step_by(2).collect();

        let registry: Vec<&str> = LANGUAGES
            .iter()
            .flat_map(|language| language.zed_languages.iter().copied())
            .collect();

        for name in &registry {
            assert!(
                declared.contains(name),
                "clients/zed/extension.toml is missing {name:?} — update its languages list \
                 to match the lang registry: {registry:?}"
            );
        }
        for name in &declared {
            assert!(
                registry.contains(name),
                "clients/zed/extension.toml declares {name:?}, which no registered language \
                 claims — remove it or register the language here"
            );
        }
    }
}

#[cfg(test)]
mod test_path_tests {
    use super::is_test_path;

    #[test]
    fn shared_directory_conventions_hold_for_every_language() {
        for path in [
            "tests/api.rs",
            "src/deep/tests/helper.rs",
            "app/test/thing.py",
            "src/components/__tests__/Button.tsx",
        ] {
            assert!(is_test_path(path), "{path}");
        }
    }

    #[test]
    fn python_naming_idioms_are_recognized() {
        // pytest discovers by FILE NAME, so none of these sit in a test
        // directory — the shared rules alone miss Python's primary convention.
        for path in [
            "pkg/test_client.py",
            "pkg/client_test.py",
            "pkg/conftest.py",
            "pkg/test_client.pyi",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        assert!(!is_test_path("pkg/latest_client.py"));
        assert!(!is_test_path("pkg/contest.py"));
    }

    #[test]
    fn javascript_naming_idioms_are_recognized() {
        assert!(is_test_path("src/api/client.test.ts"));
        assert!(is_test_path("src/api/client.spec.tsx"));
        assert!(!is_test_path("src/api/client.ts"));
    }

    #[test]
    fn rust_recognizes_a_broken_out_test_module() {
        assert!(is_test_path("src/graph/tests.rs"));
        assert!(!is_test_path("src/graph/store.rs"));
    }

    #[test]
    fn naming_idioms_do_not_leak_across_languages() {
        // `.spec.` is a JS convention and means nothing in Rust or Python; a
        // shared filename rule would classify these as tests in every language.
        assert!(!is_test_path("src/openapi.spec.rs"));
        assert!(!is_test_path("pkg/openapi.spec.py"));
        // Python's `test_` prefix is likewise not a Rust convention.
        assert!(!is_test_path("src/test_harness.rs"));
    }

    #[test]
    fn an_unregistered_language_gets_the_shared_rules_only() {
        assert!(is_test_path("tests/smoke.go"));
        // `*_test.go` is Go's idiom, which this build has no language for. It
        // reports false rather than guessing — the honest answer.
        assert!(!is_test_path("pkg/client_test.go"));
    }

    #[test]
    fn a_spec_directory_is_not_assumed_to_be_tests() {
        // This repository's own `spec/` holds specifications. Treating the
        // segment as a test directory would silently drop real source.
        assert!(!is_test_path("spec/protocol.py"));
    }
}
