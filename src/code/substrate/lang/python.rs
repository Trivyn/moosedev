//! Python: scip-python SCIP producer + tree-sitter syntactic fallback.

use std::path::Path;
use std::process::Command;

use scip::symbol::{format_symbol, parse_symbol};
use scip::types::descriptor;

use super::{backticked, file_name, no_settings, note_failed, STUB_MESSAGES};
use super::{
    first_matching_subdir, FailedTest, FallbackSpec, LanguageSpec, ProducerHooks, ServerSpec,
    StubSyntax,
};
use crate::code::substrate::producer::{ProducerSpec, ProducerTarget};
use crate::code::substrate::scip::SymbolData;
use crate::code::substrate::symbols;
use serde_json::{json, Value};

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
    is_test_path: Some(is_test_path),
    // Two servers, both run when installed: a type checker, and ruff as the
    // linter, which is its own server rather than a check the type checker
    // runs (clippy through rust-analyzer).
    servers: &[
        ServerSpec {
            // basedpyright is a pyright fork that speaks the same protocol and
            // settings; either is the type checker.
            name: "pyright",
            language: "Python",
            commands: &[
                &["basedpyright-langserver", "--stdio"],
                &["pyright-langserver", "--stdio"],
            ],
            languages: SOURCE_FILES,
            project_files: &[
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
                "pyrightconfig.json",
            ],
            server_status: false,
            publishes_every_version: true,
            options: || Value::Null,
            linter: None,
            lint_source: None,
            settings: pyright_settings,
        },
        ServerSpec {
            name: "ruff",
            language: "Python",
            commands: &[&["ruff", "server"]],
            languages: SOURCE_FILES,
            project_files: &["pyproject.toml", "ruff.toml", ".ruff.toml"],
            server_status: false,
            publishes_every_version: false,
            // An undefined name (F821) and a syntax error are the type
            // checker's to report; reported by both, each would be listed
            // twice. The project's own ruff configuration still applies:
            // this ignore is added to its rule selection. No `# noqa`
            // comment is offered as a fix: it silences the lint, it does not
            // fix it.
            options: || {
                json!({"settings": {
                    "lint": {"ignore": ["F821"]},
                    "showSyntaxErrors": false,
                    "codeAction": {"disableRuleComment": {"enable": false}},
                }})
            },
            linter: None,
            lint_source: Some("Ruff"),
            settings: no_settings,
        },
    ],
    // pytest and python need no manifest to run.
    checks: &[],
    stubs: Some(StubSyntax {
        markers: &["raise NotImplementedError"],
        stub_messages: STUB_MESSAGES,
        failure_constructs: &["raise "],
        line_comments: &["#"],
        block_comments: &[],
        quotes: &['"', '\''],
    }),
    test_failures: Some(test_failures),
    unresolved_names: Some(unresolved_names),
};

/// Python sources and the language id each is opened with.
const SOURCE_FILES: &[(&str, &str)] = &[("py", "python"), ("pyi", "python")];

/// pyright's and basedpyright's settings, by the section each asks for:
/// pyright `python` (and `pyright`), basedpyright `python` and `basedpyright`;
/// the `.analysis` sections as older versions ask. Standard checking (what
/// pyright defaults to; basedpyright's default reports far more), only the
/// files the harness opened, which are the files the task edited, and no
/// complaint that an import's stubs were found without its source: the
/// mirror has no virtual environment. A project's own pyright configuration
/// overrides these.
fn pyright_settings(section: &str) -> Value {
    let analysis = json!({
        "typeCheckingMode": "standard",
        "diagnosticMode": "openFilesOnly",
        "diagnosticSeverityOverrides": {"reportMissingModuleSource": "none"},
    });
    match section {
        "python.analysis" | "basedpyright.analysis" => analysis,
        "python" | "basedpyright" => json!({ "analysis": analysis }),
        _ => Value::Null,
    }
}

/// The name ruff's F821 ("Undefined name `X`") or pyright ("\"X\" is not
/// defined") says is not defined.
fn unresolved_names(message: &str) -> Vec<String> {
    let first = message.lines().next().unwrap_or_default().trim();
    let name = if first.to_ascii_lowercase().starts_with("undefined name ") {
        backticked(first).next()
    } else {
        first
            .strip_suffix(" is not defined")
            .and_then(|quoted| quoted.strip_prefix('"')?.strip_suffix('"'))
    };
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| vec![name.to_owned()])
        .unwrap_or_default()
}

/// pytest's and unittest's reports of failed tests. pytest names a node id in
/// its short summary (`FAILED tests/test_map.py::test_grid - AssertionError`)
/// and, verbose, after it (`tests/test_map.py::test_grid FAILED [ 50%]`);
/// unittest names the method and its class (`FAIL: test_grid
/// (tests.test_map.MapTests)`, since 3.11 with the method repeated inside).
/// An `ERROR` is a failure too: the test did not pass.
fn test_failures(output: &str) -> Vec<FailedTest> {
    let mut failures = Vec::new();
    for line in output.lines().map(str::trim) {
        if let Some(rest) = line
            .strip_prefix("FAILED ")
            .or_else(|| line.strip_prefix("ERROR "))
        {
            let id = rest.split(" - ").next().unwrap_or(rest).trim();
            if id.contains("::") {
                note_failed(&mut failures, id, None);
            }
        } else if let Some(rest) = line
            .strip_prefix("FAIL: ")
            .or_else(|| line.strip_prefix("ERROR: "))
        {
            let Some((method, context)) = rest.split_once(" (") else {
                continue;
            };
            let context = context.trim_end_matches(')');
            if context.ends_with(&format!(".{method}")) {
                note_failed(&mut failures, context, None);
            } else {
                note_failed(&mut failures, &format!("{context}.{method}"), None);
            }
        } else if let Some((id, verdict)) = line.split_once(' ') {
            if id.contains("::") && (verdict.starts_with("FAILED") || verdict.starts_with("ERROR"))
            {
                note_failed(&mut failures, id, None);
            }
        }
    }
    failures
}

/// pytest's default discovery: `test_*.py` and `*_test.py`, plus the `conftest`
/// fixture module. None of these sit in a test DIRECTORY by convention, so a
/// shared path rule misses Python's primary idiom entirely.
fn is_test_path(path: &str) -> bool {
    let file_name = file_name(path);
    let stem = file_name
        .strip_suffix(".py")
        .or_else(|| file_name.strip_suffix(".pyi"));
    stem.is_some_and(|stem| {
        stem.starts_with("test_") || stem.ends_with("_test") || stem == "conftest"
    })
}

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
    #[test]
    fn undefined_names_are_read_from_ruff_and_pyright() {
        assert_eq!(super::unresolved_names("Undefined name `Grid`"), ["Grid"]);
        assert_eq!(super::unresolved_names("\"Grid\" is not defined"), ["Grid"]);
        assert!(super::unresolved_names("\"Grid\" is not accessed").is_empty());
        assert!(super::unresolved_names("Import \"os\" could not be resolved").is_empty());
    }

    /// A type checker (basedpyright before pyright) and ruff as the linter,
    /// which leaves undefined names and syntax errors to the type checker.
    #[test]
    fn python_runs_a_type_checker_and_ruff_as_its_linter() {
        let [types, lints] = super::LANGUAGE.servers else {
            panic!("two Python servers");
        };
        assert_eq!((types.name, lints.name), ("pyright", "ruff"));
        let programs: Vec<&str> = types.commands.iter().map(|argv| argv[0]).collect();
        assert_eq!(programs, ["basedpyright-langserver", "pyright-langserver"]);
        assert_eq!((types.lint_source, lints.lint_source), (None, Some("Ruff")));
        // pyright reports on every version, even of an excluded file; ruff
        // says nothing about a file its configuration excludes.
        assert!(types.publishes_every_version && !lints.publishes_every_version);
        for server in [types, lints] {
            assert_eq!(server.languages, [("py", "python"), ("pyi", "python")]);
            assert!(server.project_files.contains(&"pyproject.toml"));
            assert!(server.linter.is_none() && !server.server_status);
        }
        let options = (lints.options)();
        assert_eq!(
            options["settings"]["lint"]["ignore"],
            serde_json::json!(["F821"])
        );
        assert_eq!(options["settings"]["showSyntaxErrors"], false);
        assert_eq!(
            options["settings"]["codeAction"]["disableRuleComment"]["enable"],
            false
        );

        // Each section pyright or basedpyright asks for, shaped as asked.
        let settings = types.settings;
        for section in ["python", "basedpyright"] {
            let analysis = &settings(section)["analysis"];
            assert_eq!(analysis["typeCheckingMode"], "standard", "{section}");
            assert_eq!(analysis["diagnosticMode"], "openFilesOnly", "{section}");
            assert_eq!(
                analysis["diagnosticSeverityOverrides"]["reportMissingModuleSource"],
                "none"
            );
            assert_eq!(settings(&format!("{section}.analysis")), *analysis);
        }
        assert!(settings("pyright").is_null());
        assert!((lints.settings)("python").is_null());
    }

    #[test]
    fn pytest_and_unittest_failures_are_named() {
        let pytest = "tests/test_map.py::test_ok PASSED                    [ 33%]
tests/test_map.py::test_grid FAILED                  [ 66%]
=========================== short test summary info ============================
FAILED tests/test_map.py::test_grid - AssertionError: assert 3 == 4
FAILED tests/test_map.py::MapTests::test_rows[2] - ValueError
========================= 2 failed, 1 passed in 0.12s ==========================
";
        let failures = super::test_failures(pytest);
        let names: Vec<&str> = failures
            .iter()
            .map(|failure| failure.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "tests/test_map.py::test_grid",
                "tests/test_map.py::MapTests::test_rows[2]"
            ]
        );
        assert_eq!(failures[0].function(), "test_grid");
        assert_eq!(failures[1].function(), "test_rows");

        let unittest = "======================================================================
FAIL: test_grid (tests.test_map.MapTests)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"/work/tests/test_map.py\", line 12, in test_grid
    self.assertEqual(rows, 4)
AssertionError: 3 != 4

======================================================================
ERROR: test_rows (tests.test_map.MapTests.test_rows)
----------------------------------------------------------------------
FAILED (failures=1, errors=1)
";
        let failures = super::test_failures(unittest);
        let names: Vec<&str> = failures
            .iter()
            .map(|failure| failure.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "tests.test_map.MapTests.test_grid",
                "tests.test_map.MapTests.test_rows"
            ]
        );
        assert_eq!(failures[0].function(), "test_grid");
    }

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
