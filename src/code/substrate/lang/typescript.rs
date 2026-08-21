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
    // scip-typescript 0.4.0 does not encode export-ness, so this structural
    // gate is a documented over-approximation: private top-level declarations
    // batch-mint too. Members and parameters stay lazy-mint-only, which
    // `is_top_level_declaration`'s declaration-suffix allowlist enforces —
    // scip-typescript parents a PropertyAssignment straight to the file
    // namespace (FileIndexer.ts), so an `sx={{ height: 8 }}` key has
    // all-namespace ancestors and is told apart from a declaration only by its
    // `Meta` suffix.
    !symbol.is_local && symbols::is_top_level_declaration(&symbol.symbol)
}

#[cfg(test)]
mod tests {
    use super::is_public;
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

    // Every fixture below is copied verbatim out of this repository's own
    // scip-typescript index. Hand-written symbol shapes are what let the
    // object-literal defect survive a guard written specifically for it: the
    // guard's test asserted on a `.` tail, while the producer emits `:`.

    #[test]
    fn declared_surface_mints_across_every_declaration_suffix() {
        for raw in [
            // Method
            "scip-typescript npm moosedev-ui 0.8.0 src/pages/`StoriesPage.tsx`/StoriesPage().",
            // Type
            "scip-typescript npm moosedev-ui 0.8.0 src/pages/`StoriesPage.tsx`/StoriesPageProps#",
            // Term
            "scip-typescript npm moosedev-ui 0.8.0 src/api/`client.ts`/api.",
        ] {
            assert!(is_public(&symbol(raw)), "{raw}");
        }
    }

    #[test]
    fn object_literal_keys_do_not_mint_as_project_api() {
        // scip-typescript emits one symbol per object-literal member, parented
        // to the file namespace. These minted 746 entities on this repository —
        // 30% of the catalog — every one an `sx` prop or a CSS key, none of
        // them addressable as project API. Note `'& code'0`, which reached the
        // Story subject selector as a browsable subject.
        for raw in [
            "scip-typescript npm moosedev-ui 0.8.0 src/components/layout/`AppShell.tsx`/height0:",
            "scip-typescript npm moosedev-ui 0.8.0 src/api/`client.ts`/`'Content-Type'0`:",
            "scip-typescript npm moosedev-ui 0.8.0 src/components/artifacts/`GeneratedArtifactPage.tsx`/`'& code'0`:",
        ] {
            assert!(!is_public(&symbol(raw)), "{raw}");
        }
    }

    #[test]
    fn members_of_a_declared_type_stay_lazy_mint_only() {
        // A Term, but with a Type ancestor — rejected on the ancestor rule
        // rather than the suffix allowlist.
        let raw = concat!(
            "scip-typescript npm moosedev-ui 0.8.0 ",
            "src/pages/`StoriesPage.tsx`/StoriesPageProps#onNavigateRecord."
        );
        assert!(!is_public(&symbol(raw)), "{raw}");
    }

    #[test]
    fn local_symbols_never_mint() {
        let mut local = symbol("scip-typescript npm moosedev-ui 0.8.0 src/api/`client.ts`/api.");
        local.is_local = true;
        assert!(!is_public(&local));
    }
}
