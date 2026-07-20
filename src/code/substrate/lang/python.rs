//! Python: scip-python SCIP producer + tree-sitter syntactic fallback.

use std::path::Path;
use std::process::Command;

use scip::symbol::{format_symbol, parse_symbol};
use scip::types::descriptor;

use super::{first_matching_subdir, FallbackSpec, LanguageSpec, ProducerHooks};
use crate::code::substrate::producer::{ProducerSpec, ProducerTarget};
use crate::code::substrate::scip::SymbolData;
use crate::code::substrate::symbols;

pub(crate) static LANGUAGE: LanguageSpec = LanguageSpec {
    producer: Some(ProducerHooks {
        spec: ProducerSpec {
            name: "scip-python",
            detect,
            command,
            extensions: &["py", "pyi"],
        },
        is_public,
        canonical_symbol: Some(canonical_symbol),
        // scip-python leaves signature_documentation empty and renders the
        // declaration as a ```python fenced block in `documentation`.
        signature_fence: Some("python"),
    }),
    fallback: Some(FallbackSpec {
        extensions: &["py", "pyi"],
        tag: "python",
        grammar,
        declaration_kind,
        identity_kinds: &["fn", "class"],
        declaration_name: None,
    }),
    zed_languages: &["Python"],
};

fn detect(repo_root: &Path) -> Option<ProducerTarget> {
    // Root-level requirements.txt counts as a marker (the historical plain-pip
    // application layout), but at subdir level only the strong project markers
    // do: tooling-only requirements.txt files in subdirectories are common
    // (moosedev's own bench/requirements.txt) and must not spawn scip-python
    // over a directory that is not a Python project.
    if is_project(repo_root) || repo_root.join("requirements.txt").is_file() {
        return Some(ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: None,
        });
    }
    first_matching_subdir(repo_root, is_project)
}

fn is_project(path: &Path) -> bool {
    ["pyproject.toml", "setup.py", "setup.cfg"]
        .iter()
        .any(|marker| path.join(marker).is_file())
}

fn command(target: &ProducerTarget, output_tmp: &Path) -> Command {
    let mut command = match std::env::var_os("MOOSEDEV_SCIP_PYTHON") {
        Some(binary) => {
            let mut command = Command::new(binary);
            command.arg("index");
            command
        }
        None => {
            let mut command = Command::new("npx");
            command.args(["--yes", "@sourcegraph/scip-python", "index"]);
            command
        }
    };
    // No --project-name: the empty default keeps symbols repo-local, and the
    // git-revision --project-version default is elided by normalize_symbol.
    command
        .arg("--output")
        .arg(output_tmp)
        .current_dir(&target.project_dir);
    command
}

fn is_public(symbol: &SymbolData) -> bool {
    // scip-python 0.6.x encodes no export information, so the contract is the
    // structural top-level gate plus the PEP 8 underscore convention (which
    // also excludes top-level dunders such as `__all__`). Module symbols fail
    // the top-level gate (their last descriptor is a namespace after
    // canonical_symbol rewrites the `__init__:` marker) and are batch-minted
    // through the module path instead. Class members stay lazy-mint-only,
    // mirroring TypeScript.
    !symbol.is_local
        && symbols::is_top_level_declaration(&symbol.symbol)
        && symbols::last_descriptor_name(&symbol.symbol).is_some_and(|name| !name.starts_with('_'))
}

/// scip-python renders a module definition as a trailing Meta descriptor
/// (`` `pkg.mod`/__init__: ``). Canonicalize it to the standard namespace
/// module symbol (`` `pkg.mod`/ ``) at ingest so module classification,
/// display naming, logical paths, and batch minting need no Python-specific
/// handling downstream. Real `__init__` methods/functions are Method
/// descriptors (`__init__().`) and are never rewritten.
fn canonical_symbol(raw: &str) -> Option<String> {
    let mut symbol = parse_symbol(raw).ok()?;
    let (last, ancestors) = symbol.descriptors.split_last()?;
    let is_namespace = |descriptor: &scip::types::Descriptor| {
        descriptor.suffix.enum_value().ok() == Some(descriptor::Suffix::Namespace)
    };
    let is_module_marker = last.suffix.enum_value().ok() == Some(descriptor::Suffix::Meta)
        && last.name == "__init__"
        && !ancestors.is_empty()
        && ancestors.iter().all(is_namespace);
    if !is_module_marker {
        return None;
    }
    symbol.descriptors.pop();
    Some(format_symbol(symbol))
}

fn grammar() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

fn declaration_kind(node_kind: &str) -> Option<&'static str> {
    // `decorated_definition` is intentionally absent: walking up from a
    // decorator body lands on the inner definition, and the decorator itself
    // is not a named declaration.
    match node_kind {
        "function_definition" => Some("fn"),
        "class_definition" => Some("class"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_marker_canonicalizes_to_namespace_symbol() {
        assert_eq!(
            canonical_symbol("scip-python python scratch 0.1.0 `scratch.app`/__init__:").as_deref(),
            Some("scip-python python scratch 0.1.0 `scratch.app`/")
        );
    }

    #[test]
    fn non_marker_symbols_are_not_rewritten() {
        for raw in [
            // real __init__ method / function: Method descriptor, not Meta
            "scip-python python scratch 0.1.0 `scratch.app`/Example#__init__().",
            "scip-python python scratch 0.1.0 `scratch.app`/greet().",
            // bare marker with no namespace ancestor stays untouched
            "scip-python python scratch 0.1.0 __init__:",
            // already-canonical module symbol
            "scip-python python scratch 0.1.0 `scratch.app`/",
        ] {
            assert_eq!(canonical_symbol(raw), None, "{raw}");
        }
    }
}
