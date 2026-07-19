//! TypeScript/JavaScript: scip-typescript SCIP producer. No tree-sitter
//! fallback grammar is registered yet.

use std::path::Path;
use std::process::Command;

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
};

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
    !symbol.is_local && symbols::is_top_level_declaration(&symbol.symbol)
}
