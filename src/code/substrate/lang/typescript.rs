//! TypeScript/JavaScript: scip-typescript SCIP producer. No tree-sitter
//! fallback grammar is registered yet.

use std::path::Path;
use std::process::Command;

use super::file_name;
use super::{first_matching_subdir, LanguageSpec, ProducerHooks};
use crate::code::substrate::producer::{ProducerSpec, ProducerTarget};
use crate::code::substrate::scip::SymbolData;
use crate::code::substrate::symbols;

pub(crate) static LANGUAGE: LanguageSpec = LanguageSpec {
    producer: Some(ProducerHooks {
        spec: ProducerSpec {
            name: "scip-typescript",
            detect,
            command,
            // scip-typescript also indexes JS under allowJs; over-triggering on
            // repos without it is bounded by the reindex debounce.
            extensions: &["ts", "tsx", "js", "jsx", "mts", "cts"],
        },
        is_public,
        canonical_symbol: None,
        // scip-typescript leaves signature_documentation empty and renders the
        // declaration as a ```ts fenced block in `documentation`.
        signature_fence: Some("ts"),
    }),
    fallback: None,
    // scip-typescript indexes JS too (allowJs), so JavaScript buffers are a
    // real substrate surface, not over-claiming.
    zed_languages: &["TypeScript", "TSX", "JavaScript"],
    is_test_path: Some(is_test_path),
};

/// The `*.test.*` / `*.spec.*` infix every JS test runner recognizes. A JS
/// idiom, not a universal one: `.spec.` means nothing in Rust or Python.
fn is_test_path(path: &str) -> bool {
    let file_name = file_name(path);
    file_name.contains(".test.") || file_name.contains(".spec.")
}

fn detect(repo_root: &Path) -> Option<ProducerTarget> {
    if is_project(repo_root) {
        return Some(ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: None,
        });
    }
    first_matching_subdir(repo_root, is_project)
}

fn is_project(path: &Path) -> bool {
    path.join("tsconfig.json").is_file() && path.join("package.json").is_file()
}

fn command(target: &ProducerTarget, output_tmp: &Path) -> Command {
    let mut command = match std::env::var_os("MOOSEDEV_SCIP_TYPESCRIPT") {
        Some(binary) => {
            let mut command = Command::new(binary);
            command.arg("index");
            command
        }
        None => {
            let mut command = Command::new("npx");
            command.args(["--yes", "@sourcegraph/scip-typescript", "index"]);
            command
        }
    };
    command
        .arg("--output")
        .arg(output_tmp)
        .current_dir(&target.project_dir);
    command
}

fn is_public(symbol: &SymbolData) -> bool {
    // scip-typescript 0.4.0 does not encode export-ness. This structural
    // over-approximation therefore includes private top-level declarations,
    // while members and parameters remain lazy-mint-only.
    !symbol.is_local
        && symbols::is_top_level_declaration(&symbol.symbol)
        && names_an_identifier(&symbol.symbol)
}

/// Whether the terminal descriptor is a real identifier.
///
/// scip-typescript emits object-literal and interface keys as descriptors with
/// their quotes intact — `'& h1'`, `'background-color'`, `'Content-Type'` — and
/// a top-level styled/theme object puts them directly under the file namespace,
/// where [`symbols::is_top_level_declaration`] cannot tell them from a declared
/// surface. They are properties of a value, addressable by name from nowhere, so
/// minting them as project API produced entities like `'& h1'0`.
fn names_an_identifier(raw: &str) -> bool {
    symbols::last_descriptor_name(raw).is_some_and(|name| {
        let mut characters = name.chars();
        characters
            .next()
            .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
            && characters.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    })
}

#[cfg(test)]
mod tests {
    use super::{is_public, names_an_identifier};
    use crate::code::substrate::scip::SymbolData;

    fn symbol(raw: &str) -> SymbolData {
        SymbolData {
            symbol: raw.to_string(),
            display_name: None,
            kind: None,
            signature: None,
            defined_in: None,
            is_local: false,
            producer: "scip-typescript".to_string(),
        }
    }

    #[test]
    fn a_declared_surface_still_mints() {
        let raw = "scip-typescript npm moosedev-ui 0.6.3 src/pages/`RecordPage.tsx`/RecordPage().";
        assert!(names_an_identifier(raw));
        assert!(is_public(&symbol(raw)));
    }

    #[test]
    fn quoted_object_keys_do_not_mint_as_project_api() {
        // A styled/theme object at file scope puts its CSS keys directly under
        // the file namespace, where the structural top-level test cannot tell
        // them from a declaration. These produced entities like `'& h1'0`.
        for raw in [
            "scip-typescript npm moosedev-ui 0.6.3 src/styles/`theme.ts`/`'& h1'0`.",
            "scip-typescript npm moosedev-ui 0.6.3 src/styles/`theme.ts`/`'background-color'1`.",
            "scip-typescript npm moosedev-ui 0.6.3 src/api/`client.ts`/`'Content-Type'0`.",
        ] {
            assert!(!names_an_identifier(raw), "{raw}");
            assert!(!is_public(&symbol(raw)), "{raw}");
        }
    }

    #[test]
    fn identifier_shapes_typescript_actually_uses_are_kept() {
        assert!(names_an_identifier(
            "scip-typescript npm ui 1.0.0 src/`a.ts`/$dollarNamed."
        ));
        assert!(names_an_identifier(
            "scip-typescript npm ui 1.0.0 src/`a.ts`/_private2."
        ));
    }
}
