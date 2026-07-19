//! Rust: rust-analyzer SCIP producer + tree-sitter syntactic fallback.

use std::path::Path;
use std::process::Command;

use super::{FallbackSpec, LanguageSpec, ProducerHooks};
use crate::code::substrate::producer::{ProducerSpec, ProducerTarget};
use crate::code::substrate::scip::SymbolData;
use crate::code::substrate::treesitter::node_text;

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
};

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
