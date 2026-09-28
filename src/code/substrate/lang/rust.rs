//! Rust: rust-analyzer SCIP producer + tree-sitter syntactic fallback.

use std::path::Path;
use std::process::Command;

use super::{file_name, note_failed};
use super::{
    CheckTool, FailedTest, FallbackSpec, LanguageSpec, LinterSpec, ProducerHooks, ServerSpec,
    StubSyntax,
};
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
    stubs: Some(StubSyntax {
        markers: &["unimplemented!(", "todo!("],
        line_comments: &["//"],
        block_comments: &["/*", "*"],
        quotes: &['"'],
    }),
    test_failures: Some(test_failures),
};

/// libtest's report of failed tests. Each is named by its `test NAME ...
/// FAILED` line, its `---- NAME stdout ----` header and the closing
/// `failures:` list, any of which a `| tail` may have cut; the panic line
/// (`thread 'NAME' (ID) panicked at FILE:LINE:COL:`) says where it failed.
/// A panic names a test only when no other line names any: a thread a test
/// spawned panics under its own name, and a `main` or unnamed thread's panic
/// is a program's.
fn test_failures(output: &str) -> Vec<FailedTest> {
    let mut failures = Vec::new();
    let mut panics = Vec::new();
    let mut listing = false;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed == "failures:" {
            listing = true;
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
        {
            note_failed(&mut failures, name, None);
        } else if let Some(name) = trimmed
            .strip_prefix("---- ")
            .and_then(|rest| rest.strip_suffix(" stdout ----"))
        {
            note_failed(&mut failures, name, None);
        } else if let Some((name, location)) = panic_line(trimmed) {
            panics.push((name, location));
        } else if listing && line.starts_with("    ") && !trimmed.contains(char::is_whitespace) {
            note_failed(&mut failures, trimmed, None);
        }
        // The list is indented under its header and ends at the first line
        // that is not.
        if !trimmed.is_empty() && !line.starts_with(char::is_whitespace) {
            listing = false;
        }
    }
    let named = !failures.is_empty();
    for (name, location) in panics {
        if failures.iter().any(|failure| failure.name == name)
            || (!named && name != "main" && name != "<unnamed>")
        {
            note_failed(&mut failures, name, location);
        }
    }
    failures
}

/// A panic line's thread name and location. Since Rust 1.73 the location ends
/// the line (`panicked at FILE:LINE:COL:`); before, it followed the message
/// (`panicked at 'MSG', FILE:LINE:COL`).
fn panic_line(line: &str) -> Option<(&str, Option<(String, u32)>)> {
    let (name, rest) = line.strip_prefix("thread '")?.split_once('\'')?;
    let (_, at) = rest.split_once("panicked at ")?;
    let place = if at.starts_with('\'') {
        at.rsplit_once(", ").map_or("", |(_, place)| place)
    } else {
        at
    };
    let mut parts = place.trim_end_matches(':').rsplitn(3, ':');
    let location = match (parts.next(), parts.next(), parts.next()) {
        (Some(_column), Some(line), Some(file)) if !file.is_empty() => {
            line.parse().ok().map(|line| (file.to_string(), line))
        }
        _ => None,
    };
    Some((name, location))
}

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

#[cfg(test)]
mod tests {
    use super::test_failures;

    #[test]
    fn libtest_failures_are_named_and_located() {
        // badciv run 12: the real `cargo test` output, trimmed.
        let output = "     Running tests/parse_errors.rs (target/debug/deps/parse_errors-1a2b)

running 3 tests
test grid_bad_width ... ok
test grid_too_few_rows ... FAILED
test unknown_section ... ok

failures:

---- grid_too_few_rows stdout ----

thread 'grid_too_few_rows' (16726628) panicked at badciv-map/tests/parse_errors.rs:41:5:
assertion `left == right` failed
  left: Ok(Map { width: 8 })
 right: Err(GridTooFewRows)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    grid_too_few_rows

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--test parse_errors`
";
        let failures = test_failures(output);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].name, "grid_too_few_rows");
        assert_eq!(
            failures[0].location,
            Some(("badciv-map/tests/parse_errors.rs".to_string(), 41))
        );
    }

    #[test]
    fn a_tail_of_the_output_still_names_the_failure() {
        let tail = "failures:\n    parse::tests::grid\n    write::tests::round\n\ntest result: FAILED. 0 passed; 2 failed\n";
        let names: Vec<String> = test_failures(tail)
            .into_iter()
            .map(|failure| failure.name)
            .collect();
        assert_eq!(names, ["parse::tests::grid", "write::tests::round"]);
        // The module path is kept; the function is its last segment.
        assert_eq!(test_failures(tail)[0].function(), "grid");
    }

    #[test]
    fn older_panic_lines_and_program_panics() {
        let old = "thread 'grid' panicked at 'index out of bounds', src/parse.rs:12:9\n";
        assert_eq!(
            test_failures(old)[0].location,
            Some(("src/parse.rs".to_string(), 12))
        );
        // `cargo run` panicking is not a failed test.
        assert!(test_failures("thread 'main' panicked at src/main.rs:3:5:\nboom\n").is_empty());
        assert!(test_failures("test result: FAILED. 1 passed; 1 failed\n").is_empty());
    }

    #[test]
    fn a_worker_panic_beside_a_failed_test_is_not_a_test() {
        // One test catches a spawned worker's panic and passes; another fails.
        let output = "running 2 tests\nthread 'worker' (7) panicked at src/pool.rs:9:5:\nboom\ntest pool::tests::recovers ... ok\ntest grid ... FAILED\n\nfailures:\n\n---- grid stdout ----\nthread 'grid' (8) panicked at src/parse.rs:4:5:\nno\n";
        let failures = test_failures(output);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].name, "grid");
        assert_eq!(failures[0].location, Some(("src/parse.rs".to_string(), 4)));
    }
}
