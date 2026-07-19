//! Syntactic position fallback for files absent from the SCIP substrate.
//!
//! Tree-sitter anchors only named declarations. It never promotes an arbitrary
//! syntax node (or a whole file) into an identity.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tree_sitter::{Node, Parser, Point, Tree};

use super::lang::{self, FallbackSpec};
use super::resolver::{Position, SourceRange};

const CACHE_CAPACITY: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntacticResolution {
    pub identity: String,
    pub name: String,
    pub kind: String,
    pub range: SourceRange,
}

#[derive(Debug)]
pub(crate) struct TreeSitterFallback {
    cache: Mutex<HashMap<PathBuf, (SystemTime, String, Tree)>>,
}

impl Default for TreeSitterFallback {
    fn default() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl TreeSitterFallback {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn supports_path(path: &Path) -> bool {
        lang::fallback_for_path(path).is_some()
    }

    pub(crate) fn resolve_position(
        &self,
        repo_root: &Path,
        relative_path: &str,
        pos: Position,
    ) -> Option<SyntacticResolution> {
        let relative = safe_relative_path(relative_path)?;
        let fallback = lang::fallback_for_path(relative)?;
        let absolute = repo_root.join(relative);
        let (source, tree) = self.parse_file(fallback, &absolute)?;
        let point = Point::new(pos.line as usize, pos.col as usize);
        let mut node = tree
            .root_node()
            .named_descendant_for_point_range(point, point)?;

        loop {
            if (fallback.declaration_kind)(node.kind()).is_some() {
                return resolution_for_node(fallback, relative_path, &source, node);
            }
            node = node.parent()?;
        }
    }

    /// `None` means the identity cannot be verified by this fallback. `false`
    /// is returned only when the file or exact declaration is positively gone.
    pub(crate) fn identity_alive(&self, repo_root: &Path, identity: &str) -> Option<bool> {
        let parsed = parse_identity(identity)?;
        let relative = safe_relative_path(parsed.path)?;
        let fallback = lang::fallback_for_path(relative)?;
        // A tag that disagrees with the path's language is unverifiable, not
        // positively gone — never orphan on malformed evidence.
        if fallback.tag != parsed.tag {
            return None;
        }
        let absolute = repo_root.join(relative);
        if !absolute.is_file() {
            return Some(false);
        }
        let (source, tree) = self.parse_file(fallback, &absolute)?;
        Some(tree_contains_identity(
            fallback,
            tree.root_node(),
            parsed.path,
            &source,
            identity,
        ))
    }

    fn parse_file(&self, fallback: &FallbackSpec, absolute: &Path) -> Option<(String, Tree)> {
        let modified = fs::metadata(absolute).ok()?.modified().ok()?;
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached_mtime, source, tree)) = cache.get(absolute) {
            if *cached_mtime == modified {
                return Some((source.clone(), tree.clone()));
            }
        }

        let source = fs::read_to_string(absolute).ok()?;
        let mut parser = Parser::new();
        parser.set_language(&(fallback.grammar)()).ok()?;
        let tree = parser.parse(&source, None)?;
        if cache.len() >= CACHE_CAPACITY && !cache.contains_key(absolute) {
            cache.clear();
        }
        cache.insert(
            absolute.to_path_buf(),
            (modified, source.clone(), tree.clone()),
        );
        Some((source, tree))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedIdentity<'a> {
    pub tag: &'a str,
    pub path: &'a str,
    pub kind: &'a str,
    pub qualified_name: &'a str,
}

pub(crate) fn parse_identity(identity: &str) -> Option<ParsedIdentity<'_>> {
    let mut fields = identity.splitn(5, ':');
    if fields.next()? != "ts" {
        return None;
    }
    let tag = fields.next()?;
    let fallback = lang::fallback_for_tag(tag)?;
    let parsed = ParsedIdentity {
        tag,
        path: fields.next()?,
        kind: fields.next()?,
        qualified_name: fields.next()?,
    };
    (!parsed.path.is_empty()
        && fallback.identity_kinds.contains(&parsed.kind)
        && !parsed.qualified_name.is_empty())
    .then_some(parsed)
}

fn safe_relative_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (!path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_))))
    .then_some(path)
}

fn resolution_for_node(
    fallback: &FallbackSpec,
    relative_path: &str,
    source: &str,
    node: Node<'_>,
) -> Option<SyntacticResolution> {
    let language = fallback.tag;
    let kind = (fallback.declaration_kind)(node.kind())?;
    let name = declaration_name(fallback, node, source)?;
    let qualified_name = qualified_name(fallback, node, source)?;
    let start = node.start_position();
    let end = node.end_position();
    Some(SyntacticResolution {
        identity: format!("ts:{language}:{relative_path}:{kind}:{qualified_name}"),
        name,
        kind: kind.to_string(),
        range: SourceRange {
            start: Position {
                line: u32::try_from(start.row).ok()?,
                col: u32::try_from(start.column).ok()?,
            },
            end: Position {
                line: u32::try_from(end.row).ok()?,
                col: u32::try_from(end.column).ok()?,
            },
        },
    })
}

fn declaration_name(fallback: &FallbackSpec, node: Node<'_>, source: &str) -> Option<String> {
    if let Some(language_name) = fallback.declaration_name {
        if let Some(name) = language_name(node, source) {
            return Some(name);
        }
    }
    Some(node_text(node.child_by_field_name("name")?, source)?.to_string())
}

fn qualified_name(fallback: &FallbackSpec, node: Node<'_>, source: &str) -> Option<String> {
    let mut names = Vec::new();
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        if (fallback.declaration_kind)(current.kind()).is_some() {
            names.push(declaration_name(fallback, current, source)?);
        }
        cursor = current.parent();
    }
    names.reverse();
    Some(names.join("::"))
}

pub(crate) fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.byte_range())
}

fn tree_contains_identity(
    fallback: &FallbackSpec,
    node: Node<'_>,
    path: &str,
    source: &str,
    identity: &str,
) -> bool {
    if (fallback.declaration_kind)(node.kind()).is_some()
        && resolution_for_node(fallback, path, source, node)
            .is_some_and(|resolution| resolution.identity == identity)
    {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|child| tree_contains_identity(fallback, child, path, source, identity));
    found
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    const FIXTURE_PATH: &str = "tests/fixtures/ts_fallback.rs";
    const PY_FIXTURE_PATH: &str = "tests/fixtures/ts_fallback.py";

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn position_in(path: &str, needle: &str) -> Position {
        let source = fs::read_to_string(root().join(path)).unwrap();
        let offset = source.find(needle).unwrap();
        let before = &source[..offset];
        Position {
            line: before.bytes().filter(|byte| *byte == b'\n').count() as u32,
            col: before.rsplit('\n').next().unwrap().len() as u32,
        }
    }

    fn resolve_in(path: &str, needle: &str) -> Option<SyntacticResolution> {
        TreeSitterFallback::default().resolve_position(&root(), path, position_in(path, needle))
    }

    fn resolve(needle: &str) -> Option<SyntacticResolution> {
        resolve_in(FIXTURE_PATH, needle)
    }

    #[test]
    fn resolves_smallest_enclosing_declarations() {
        let cases = [
            ("a + b", "fn:top_level"),
            ("label: String", "struct:Widget"),
            ("Dark { level", "enum:Shade"),
            ("fn render(&self) -> String;", "fn:Render::render"),
            ("\"default body\"", "fn:Render::hint"),
            ("let local", "fn:<Widget as Render>::render"),
            ("42", "fn:outer::inner::nested_fn"),
            ("impl Widget", "impl:Widget"),
            ("macro_rules! shout", "macro:shout"),
            ("pub struct Hidden", "struct:outer::Hidden"),
        ];
        for (needle, suffix) in cases {
            let resolution = resolve(needle).unwrap_or_else(|| panic!("no hit for {needle}"));
            assert!(
                resolution.identity.ends_with(suffix),
                "{} did not end in {suffix}",
                resolution.identity
            );
        }
    }

    #[test]
    fn non_declaration_positions_are_misses() {
        for needle in [
            "//! Fixture",
            "use std::collections",
            "\npub const MAX_DEPTH",
        ] {
            assert!(resolve(needle).is_none(), "unexpected hit for {needle:?}");
        }
        let fallback = TreeSitterFallback::default();
        assert!(fallback
            .resolve_position(
                &root(),
                FIXTURE_PATH,
                Position {
                    line: u32::MAX,
                    col: 0,
                },
            )
            .is_none());
    }

    #[test]
    fn resolves_smallest_enclosing_python_declarations() {
        let cases = [
            ("a + b", "fn:top_level"),
            ("label = ", "class:Widget"),
            ("local = 1", "fn:Widget::render"),
            ("return 42", "fn:outer::nested_fn"),
            ("decorated body", "fn:decorated"),
        ];
        for (needle, suffix) in cases {
            let resolution = resolve_in(PY_FIXTURE_PATH, needle)
                .unwrap_or_else(|| panic!("no hit for {needle}"));
            assert_eq!(
                resolution.identity,
                format!("ts:python:{PY_FIXTURE_PATH}:{suffix}")
            );
        }
    }

    #[test]
    fn python_non_declaration_positions_are_misses() {
        for needle in ["\"\"\"Fixture", "import collections", "MAX_DEPTH = 3"] {
            assert!(
                resolve_in(PY_FIXTURE_PATH, needle).is_none(),
                "unexpected hit for {needle:?}"
            );
        }
    }

    #[test]
    fn python_stub_files_resolve_like_python_sources() {
        let stub = "tests/fixtures/ts_fallback.pyi";
        let resolution = resolve_in(stub, "-> str").expect("stub method resolves");
        assert_eq!(
            resolution.identity,
            format!("ts:python:{stub}:fn:Widget::render")
        );
        let resolution = resolve_in(stub, "a: int").expect("stub function resolves");
        assert_eq!(
            resolution.identity,
            format!("ts:python:{stub}:fn:top_level")
        );
    }

    #[test]
    fn python_identities_parse_and_reject_unknown_languages() {
        let identity = "ts:python:tests/fixtures/ts_fallback.py:fn:Widget::render";
        let parsed = parse_identity(identity).unwrap();
        assert_eq!(parsed.path, PY_FIXTURE_PATH);
        assert_eq!(parsed.kind, "fn");
        assert_eq!(parsed.qualified_name, "Widget::render");
        assert!(parse_identity("ts:python:tests/fixtures/ts_fallback.py:class:Widget").is_some());
        assert!(parse_identity("ts:go:tests/fixtures/ts_fallback.py:fn:main").is_none());
    }

    #[test]
    fn python_identity_alive_requires_exact_declaration() {
        let fallback = TreeSitterFallback::default();
        let identity = resolve_in(PY_FIXTURE_PATH, "local = 1").unwrap().identity;
        assert_eq!(fallback.identity_alive(&root(), &identity), Some(true));
        assert_eq!(
            fallback.identity_alive(
                &root(),
                "ts:python:tests/fixtures/ts_fallback.py:fn:no_such_fn"
            ),
            Some(false)
        );
    }

    #[test]
    fn identities_with_qualified_names_parse_boundedly() {
        let identity = concat!(
            "ts:rust:tests/fixtures/ts_fallback.rs:fn:",
            "<Widget as Render>::render"
        );
        let parsed = parse_identity(identity).unwrap();
        assert_eq!(parsed.path, FIXTURE_PATH);
        assert_eq!(parsed.kind, "fn");
        assert_eq!(parsed.qualified_name, "<Widget as Render>::render");
    }

    #[test]
    fn identity_alive_requires_an_exact_declaration() {
        let fallback = TreeSitterFallback::default();
        let identity = resolve("let local").unwrap().identity;
        assert_eq!(fallback.identity_alive(&root(), &identity), Some(true));
        assert_eq!(
            fallback.identity_alive(
                &root(),
                "ts:rust:tests/fixtures/ts_fallback.rs:fn:no_such_fn"
            ),
            Some(false)
        );
    }

    #[test]
    fn identity_is_stable_after_cache_invalidating_rewrite() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "moosedev-ts-identity-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = "fn stable() { let value = 1; }\n";
        fs::write(root.join("fixture.rs"), source).unwrap();

        let fallback = TreeSitterFallback::default();
        let position = Position { line: 0, col: 20 };
        let first = fallback
            .resolve_position(&root, "fixture.rs", position)
            .unwrap();

        // Some filesystems expose mtimes at one-second granularity. Waiting past
        // that boundary makes the identical rewrite deterministically invalidate.
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(root.join("fixture.rs"), source).unwrap();
        let second = fallback
            .resolve_position(&root, "fixture.rs", position)
            .unwrap();

        assert_eq!(first.identity, second.identity);
        fs::remove_dir_all(root).unwrap();
    }
}
