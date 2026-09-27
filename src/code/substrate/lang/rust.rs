//! Rust: rust-analyzer SCIP producer + tree-sitter syntactic fallback.

use std::path::Path;
use std::process::Command;

use super::file_name;
use super::{CheckTool, FallbackSpec, LanguageSpec, LinterSpec, ProducerHooks, ServerSpec};
use crate::code::substrate::producer::{ProducerSpec, ProducerTarget};
use crate::code::substrate::scip::SymbolData;
use crate::code::substrate::treesitter::node_text;
use serde_json::json;

pub(crate) static LANGUAGE: LanguageSpec = LanguageSpec {
    producer: Some(ProducerHooks {
        spec: ProducerSpec {
            name: "rust-analyzer",
            detect,
            command,
            extensions: &["rs"],
        },
        is_public,
        canonical_symbol: None,
        signature_fence: None,
    }),
    fallback: Some(FallbackSpec {
        extensions: &["rs"],
        tag: "rust",
        grammar,
        declaration_kind,
        identity_kinds: &[
            "fn", "struct", "enum", "union", "trait", "impl", "mod", "const", "static", "type",
            "macro",
        ],
        declaration_name: Some(declaration_name),
    }),
    zed_languages: &["Rust"],
    is_test_path: Some(is_test_path),
    server: Some(ServerSpec {
        name: "rust-analyzer",
        language: "Rust",
        commands: &[&["rust-analyzer"]],
        languages: &[("rs", "rust")],
        project_files: &["Cargo.toml"],
        server_status: true,
        // Check with `cargo check` on save, so borrow and lifetime errors
        // arrive too, not only rust-analyzer's own analysis.
        options: || json!({"checkOnSave": true, "check": {"command": "check"}}),
        // Clippy as the on-save check: its lints arrive through the same
        // settled path as the compiler's errors, which it reports too.
        linter: Some(LinterSpec {
            name: "clippy",
            source: "clippy",
            probe: &["cargo", "clippy", "--version"],
            install_hint: "rustup component add clippy",
            options: || json!({"checkOnSave": true, "check": {"command": "clippy"}}),
        }),
    }),
    checks: &[CheckTool {
        program: "cargo",
        manifest: "Cargo.toml",
        manifest_option: Some("--manifest-path"),
        subcommands: &[
            "build", "check", "test", "run", "clippy", "bench", "doc", "fmt", "metadata", "tree",
        ],
        directory_options: &["-C"],
    }],
};

/// A `mod tests;` broken out into its own file. The inline `#[cfg(test)] mod
/// tests` form — far more common — is invisible to any path check; see the
/// known limit on [`lang::is_test_path`].
fn is_test_path(path: &str) -> bool {
    file_name(path) == "tests.rs"
}

fn detect(repo_root: &Path) -> Option<ProducerTarget> {
    repo_root
        .join("Cargo.toml")
        .is_file()
        .then(|| ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: None,
        })
}

fn command(target: &ProducerTarget, output_tmp: &Path) -> Command {
    let binary =
        std::env::var("MOOSEDEV_SCIP_PRODUCER").unwrap_or_else(|_| "rust-analyzer".to_string());
    let mut command = Command::new(binary);
    command
        .arg("scip")
        .arg(&target.project_dir)
        .arg("--output")
        .arg(output_tmp);
    command
}

fn is_public(symbol: &SymbolData) -> bool {
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

fn grammar() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn declaration_kind(node_kind: &str) -> Option<&'static str> {
    match node_kind {
        "function_item" | "function_signature_item" => Some("fn"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "union_item" => Some("union"),
        "trait_item" => Some("trait"),
        "impl_item" => Some("impl"),
        "mod_item" => Some("mod"),
        "const_item" => Some("const"),
        "static_item" => Some("static"),
        "type_item" => Some("type"),
        "macro_definition" => Some("macro"),
        _ => None,
    }
}

/// impl blocks are named by their type (and trait); everything else falls
/// through to the shared `name`-field default.
fn declaration_name(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "impl_item" {
        return None;
    }
    let ty = node_text(node.child_by_field_name("type")?, source)?;
    match node.child_by_field_name("trait") {
        Some(trait_node) => Some(format!("<{ty} as {}>", node_text(trait_node, source)?)),
        None => Some(ty.to_string()),
    }
}
