//! Per-language registry for the substrate.
//!
//! Everything language-specific lives in one module per language: the SCIP
//! producer registration and its idiom hooks (visibility contract, symbol
//! canonicalization, signature fence) plus the tree-sitter fallback grammar
//! and its node tables, and the language server and linter the harness checks
//! edits with. The rest of the substrate, and the harness's checker, dispatch
//! through this registry, so adding a language is one new module here plus one
//! row in `LANGUAGES` — no edits to producer/resolver/scip/treesitter or to the
//! language-server client.

pub(crate) mod python;
pub(crate) mod rust;
pub(crate) mod typescript;

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

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
    /// The language servers the harness checks edits with, each started when
    /// installed: a type checker, and a linter that runs as its own server
    /// (ruff). Empty when the harness has none for this language yet.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub servers: &'static [ServerSpec],
    /// Programs a verification check runs that find their project by a
    /// manifest, searching upward from where they run.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub checks: &'static [CheckTool],
    /// How this language marks code as not written yet, and how to tell such
    /// a marker in code from one in a comment or a string. None when the
    /// language has no stub idiom: the finish gate then says nothing.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub stubs: Option<StubSyntax>,
    /// The tests a test runner's output reports failed, so the harness can
    /// tell the same failure coming back from a new one (badciv run 12:
    /// `grid_too_few_rows` failed on four runs with no edit between them).
    /// None when this build reads no runner of the language.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub test_failures: Option<fn(&str) -> Vec<FailedTest>>,
    /// The names a diagnostic says could not be resolved ("unresolved import
    /// `a::B`" names `B`), so the harness can point at a declaration of that
    /// name found by name. None when this build reads no such message of the
    /// language.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub unresolved_names: Option<fn(&str) -> Vec<String>>,
    /// The files a diagnostic or a failed run's output says a module
    /// declaration or import could not find (rustc's "file not found for
    /// module", pyright's unresolved import, Python's `ModuleNotFoundError`),
    /// as paths relative to the project root, most likely first. Read from
    /// the message, the compiler's full text when there is one, and the file
    /// that declares the module (the finding's file; empty when unknown). The
    /// harness asks the human whether a missing one joins the plan. None when
    /// this build reads no such message of the language.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub missing_modules: Option<MissingModules>,
    /// Whether a diagnostic's message says the file does not parse (rustc's
    /// "unknown start of token", rust-analyzer's "Syntax Error: …",
    /// pyright's "Expected expression", ruff's "Expected `)`, found newline").
    /// Read from the message alone: a finding keeps no rule code. A quick fix
    /// offered for such a file guesses at text the model meant to write, so
    /// the harness never applies one itself (badciv P5: rustc's "a keyword
    /// `fn` with a similar name" turned literal `\n\n` into `\fn\fn()`).
    /// None when this build reads no such message of the language.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub is_syntax_error: Option<fn(&str) -> bool>,
    /// The directory a module declared in a file lives in, by the
    /// language's own rule (Rust: `src/foo/` for `mod inner;` in
    /// `src/foo.rs`, the file's own directory for `lib.rs`, `main.rs` and
    /// `mod.rs`). A missing module file there is the project's even while
    /// that directory does not exist yet. None when the language has no
    /// such rule (Python's absolute imports name no directory by it).
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub module_dir: Option<fn(&str) -> String>,
}

/// A language's reader of missing module files: (message, the compiler's full
/// text, the declaring file) to paths relative to the project root.
pub(crate) type MissingModules = fn(&str, Option<&str>, &str) -> Vec<String>;

/// A test a runner reported failed: its name as the runner printed it
/// (`parse::tests::grid`, `tests/test_map.py::test_grid`) and, when the output
/// says, the file and line it failed at.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedTest {
    pub name: String,
    pub location: Option<(String, u32)>,
}

#[cfg_attr(not(feature = "harness"), allow(dead_code))]
impl FailedTest {
    /// The test function's own name: the last segment of a qualified name,
    /// without a parameterization (`test_grid[3]`).
    pub(crate) fn function(&self) -> &str {
        let last = self.name.rsplit("::").next().unwrap_or(&self.name);
        let last = last.rsplit('.').next().unwrap_or(last);
        last.split('[').next().unwrap_or(last)
    }
}

/// Add a failure to `failures` unless its name is already there, keeping the
/// first location any line gave it. A runner names one failure several times
/// (libtest: the `... FAILED` line, the panic, the summary list).
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
fn note_failed(failures: &mut Vec<FailedTest>, name: &str, location: Option<(String, u32)>) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    match failures.iter_mut().find(|failure| failure.name == name) {
        Some(failure) => {
            if failure.location.is_none() {
                failure.location = location;
            }
        }
        None => failures.push(FailedTest {
            name: name.to_string(),
            location,
        }),
    }
}

/// The failed tests every registered runner reads in `output`, in the order
/// first named. Output does not say which runner printed it, and the
/// languages' formats do not overlap, so every parser reads it.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn failed_tests(output: &str) -> Vec<FailedTest> {
    let mut failures = Vec::new();
    for parse in LANGUAGES
        .iter()
        .filter_map(|language| language.test_failures)
    {
        for failure in parse(output) {
            note_failed(&mut failures, &failure.name, failure.location);
        }
    }
    failures
}

/// The names every registered language's parser reads as unresolved in a
/// diagnostic `message`, in order, each once. The message does not say which
/// language's checker wrote it, and the formats do not overlap.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn unresolved_names(message: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for parse in LANGUAGES
        .iter()
        .filter_map(|language| language.unresolved_names)
    {
        for name in parse(message) {
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// The files every registered language's parser reads as missing modules in
/// a diagnostic (`message`, `detail`) found in `declaring_file`, in order, each
/// once. As for [`unresolved_names`], the formats do not overlap.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn missing_modules(
    message: &str,
    detail: Option<&str>,
    declaring_file: &str,
) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for parse in LANGUAGES
        .iter()
        .filter_map(|language| language.missing_modules)
    {
        for file in parse(message, detail, declaring_file) {
            if !file.is_empty() && !files.contains(&file) {
                files.push(file);
            }
        }
    }
    files
}

/// Whether any registered language reads a diagnostic `message` as a syntax
/// error (see [`LanguageSpec::is_syntax_error`]). As for
/// [`unresolved_names`], the message does not say which checker wrote it,
/// and the formats do not overlap.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn is_syntax_error(message: &str) -> bool {
    LANGUAGES
        .iter()
        .filter_map(|language| language.is_syntax_error)
        .any(|reads| reads(message))
}

/// The directory a module declared in `declaring_file` lives in, when its
/// language says (see [`LanguageSpec::module_dir`]).
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn declared_module_dir(declaring_file: &str) -> Option<String> {
    let module_dir = language_for_path(declaring_file)?.module_dir?;
    Some(module_dir(declaring_file))
}

/// `path`'s directory, `/`-separated, without a trailing `/`; empty at the
/// project root.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `dir` joined with the relative `path`, `/`-separated.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
fn join_path(dir: &str, path: &str) -> String {
    if dir.is_empty() {
        path.to_string()
    } else {
        format!("{dir}/{path}")
    }
}

/// The text between each pair of backticks in `text`, in order.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
fn backticked(text: &str) -> impl Iterator<Item = &str> {
    text.split('`').skip(1).step_by(2)
}

/// A language's stub idiom (`unimplemented!(`, `raise NotImplementedError`)
/// with just enough lexical syntax to read a line: a marker after a comment
/// opener or inside a string is not code. A finish with a stub left in a
/// planned file is sent back (badciv be128e71 finished with `parse_map`
/// stubbed).
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct StubSyntax {
    pub markers: &'static [&'static str],
    /// Messages that say code is not written yet ("not implemented"), matched
    /// without regard to case inside a string literal on a line that also
    /// holds one of `failure_constructs` as code: badciv P5 left
    /// `Err(MapError::Parse("Not implemented".to_string()))`.
    pub stub_messages: &'static [&'static str],
    /// How the language fails with a message (`Err(`, `panic!(`, `raise `).
    pub failure_constructs: &'static [&'static str],
    /// Openers of a comment that runs to the end of the line (`//`, `#`).
    pub line_comments: &'static [&'static str],
    /// Openers of a block comment, and a line that continues one (`/*`, `*`),
    /// for a line read alone.
    pub block_comments: &'static [&'static str],
    /// Comments and strings that may span lines, as `(opener, closer)`
    /// (`/*` … `*/`, `"""` … `"""`); a string's when its opener starts with
    /// one of `quotes`. Reading a whole file, what such a span covers is not
    /// code on the lines after the one it opens on either.
    pub multiline: &'static [(&'static str, &'static str)],
    /// String delimiters (`"`, `'`, `` ` ``).
    pub quotes: &'static [char],
}

/// A program a check runs that finds its project by a manifest at or above
/// the directory it runs in (`cargo` by `Cargo.toml`). A plan whose check
/// could find none, on disk or among the plan's files, is sent back.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckTool {
    pub program: &'static str,
    pub manifest: &'static str,
    /// The option that names a manifest file instead (`--manifest-path`).
    pub manifest_option: Option<&'static str>,
    /// Subcommands that need the project; any other is not judged
    /// (`cargo --version` needs none).
    pub subcommands: &'static [&'static str],
    /// Options that make it run elsewhere (`cargo -C`, `npm --prefix`); a
    /// check with one is not judged.
    pub directory_options: &'static [&'static str],
}

/// A language server the harness runs as a deterministic checker.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ServerSpec {
    /// Shown in prompts and the journal.
    pub name: &'static str,
    /// The language, for the human ("No linter for Rust").
    pub language: &'static str,
    /// Candidate commands, first found on the trusted PATH wins.
    pub commands: &'static [&'static [&'static str]],
    /// File extensions and the language id each is opened with.
    pub languages: &'static [(&'static str, &'static str)],
    /// Files whose creation or deletion changes the project's shape; the
    /// server restarts so it rediscovers the project.
    pub project_files: &'static [&'static str],
    /// The server reports `experimental/serverStatus` (rust-analyzer), whose
    /// `quiescent` flag says when indexing and checking are done.
    pub server_status: bool,
    /// When the server publishes diagnostics for an open document, which is
    /// what settling may wait for.
    pub publishes: Publishes,
    /// Sent as `initializationOptions` when the language has no linter.
    pub options: fn() -> Value,
    /// What this server leaves to another server of its language while that
    /// one runs for the task (ruff: undefined names and syntax errors, to the
    /// type checker). The other is listed before it in `servers`, so it has
    /// started, or not, by the time this one starts.
    pub defers_to: Option<Deferral>,
    /// The language's linter, run through the server when installed.
    pub linter: Option<LinterSpec>,
    /// The server is itself a linter (ruff): its warnings whose `source` is
    /// this are lints. Its errors stay errors.
    pub lint_source: Option<&'static str>,
    /// The answer to a `workspace/configuration` item, by its `section`
    /// (pyright asks for `python`); [`no_settings`] answers null to all.
    pub settings: fn(&str) -> Value,
}

/// When a language server publishes diagnostics for an open document.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Publishes {
    /// For every version, even an unchanged result and even of a file its
    /// configuration excludes (pyright): settling waits for its report on
    /// the version just sent, since it may say nothing while it analyzes.
    EveryVersion,
    /// For every version of a file it checks, and nothing for a file its
    /// configuration excludes (ruff): once it has published for a file,
    /// settling waits for its report on the version just sent; a file it has
    /// never published for may be one it excludes.
    CheckedFiles,
    /// Only when its result changes (rust-analyzer); other evidence
    /// (`server_status`) says when it is done.
    OnChange,
}

/// See [`ServerSpec::defers_to`].
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deferral {
    /// The [`ServerSpec::name`] of the server deferred to.
    pub to: &'static str,
    /// Sent as `initializationOptions` instead of [`ServerSpec::options`]
    /// while that server runs.
    pub options: fn() -> Value,
}

/// No settings for any section: the server keeps its defaults and its
/// initialization options.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn no_settings(_section: &str) -> Value {
    Value::Null
}

/// A linter the server runs for the harness. A missing one is reported to the
/// human and the checker runs without it; nothing stops.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct LinterSpec {
    pub name: &'static str,
    /// The `source` its diagnostics carry.
    pub source: &'static str,
    /// A command that succeeds only when it is installed.
    pub probe: &'static [&'static str],
    pub install_hint: &'static str,
    /// `initializationOptions` that run it.
    pub options: fn() -> Value,
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

/// The check tools of every language, in `LANGUAGES` order.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn check_tools() -> impl Iterator<Item = &'static CheckTool> {
    LANGUAGES.iter().flat_map(|language| language.checks.iter())
}

/// The language servers in `LANGUAGES` order, each language's in its order.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn language_servers() -> impl Iterator<Item = &'static ServerSpec> {
    LANGUAGES
        .iter()
        .flat_map(|language| language.servers.iter())
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

/// Whether a name reads as a file of a registered language (`lib.rs`,
/// `labels.py`) by its extension: text in a note that names a file, not code.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn is_language_file(path: &str) -> bool {
    language_for_path(path).is_some()
}

/// Messages that say code is not written yet, shared by the languages.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) const STUB_MESSAGES: &[&str] = &[
    "not implemented",
    "not yet implemented",
    "unimplemented",
    "todo",
];

/// The stub idiom of the language owning a path; None for an unknown language
/// or one without stubs.
#[cfg_attr(not(feature = "harness"), allow(dead_code))]
pub(crate) fn stub_syntax_for(path: &str) -> Option<&'static StubSyntax> {
    language_for_path(path).and_then(|language| language.stubs.as_ref())
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
    #[test]
    fn unresolved_names_union_every_language() {
        assert_eq!(
            super::unresolved_names(
                "unresolved imports `badciv_map::Terrain`, `badciv_map::Climate`"
            ),
            ["Terrain", "Climate"]
        );
        assert_eq!(super::unresolved_names("Undefined name `grid`"), ["grid"]);
        assert!(super::unresolved_names("mismatched types\nexpected `u8`").is_empty());
    }

    #[test]
    fn syntax_errors_union_every_language() {
        // badciv P5 attempt 3, verbatim: rust-analyzer's and rustc's.
        assert!(super::is_syntax_error(
            "Syntax Error: expected expression, item or let statement"
        ));
        assert!(super::is_syntax_error("unknown start of token: \\"));
        assert!(super::is_syntax_error("Expected expression"));
        assert!(super::is_syntax_error(
            "SyntaxError: Expected an expression"
        ));
        assert!(!super::is_syntax_error(
            "mismatched types\nexpected `u32`, found `usize`"
        ));
        assert!(!super::is_syntax_error("\"Grid\" is not defined"));
    }

    #[test]
    fn missing_modules_union_every_language() {
        assert_eq!(
            super::missing_modules(
                "unresolved module, can't find module file: parse.rs, or parse/mod.rs",
                None,
                "src/lib.rs"
            ),
            ["src/parse.rs", "src/parse/mod.rs"]
        );
        assert_eq!(
            super::missing_modules("Import \".grid\" could not be resolved", None, "pkg/a.py"),
            ["pkg/grid.py", "pkg/grid/__init__.py"]
        );
        assert!(super::missing_modules("mismatched types", None, "src/lib.rs").is_empty());
    }

    /// Rust puts the modules a file declares in its stem's directory, or in
    /// its own for `lib.rs`, `main.rs` and `mod.rs`; Python names none.
    #[test]
    fn a_declared_module_lives_in_the_declaring_files_module_directory() {
        let dir = super::declared_module_dir;
        assert_eq!(dir("src/foo.rs").as_deref(), Some("src/foo"));
        assert_eq!(dir("crates/a/src/lib.rs").as_deref(), Some("crates/a/src"));
        assert_eq!(dir("src/main.rs").as_deref(), Some("src"));
        assert_eq!(dir("src/foo/mod.rs").as_deref(), Some("src/foo"));
        assert_eq!(dir("lib.rs").as_deref(), Some(""));
        assert_eq!(dir("pkg/a.py"), None);
        assert_eq!(dir(""), None);
    }

    #[test]
    fn stub_syntax_follows_the_language_of_the_path() {
        let markers = |path| super::stub_syntax_for(path).map(|syntax| syntax.markers);
        assert!(markers("src/parse.rs")
            .unwrap()
            .contains(&"unimplemented!("));
        assert!(markers("pkg/labels.py")
            .unwrap()
            .contains(&"raise NotImplementedError"));
        assert!(markers("web/app.ts").is_some());
        assert!(markers("notes.md").is_none());
        assert!(markers("Makefile").is_none());
        // Rust has no `#` comment; Python has no `//` one.
        assert!(!super::stub_syntax_for("a.rs")
            .unwrap()
            .line_comments
            .contains(&"#"));
        assert!(!super::stub_syntax_for("a.py")
            .unwrap()
            .line_comments
            .contains(&"//"));
    }

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
