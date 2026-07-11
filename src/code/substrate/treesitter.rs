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
        grammar_for(path).is_some()
    }

    pub(crate) fn resolve_position(
        &self,
        repo_root: &Path,
        relative_path: &str,
        pos: Position,
    ) -> Option<SyntacticResolution> {
        let relative = safe_relative_path(relative_path)?;
        if !Self::supports_path(relative) {
            return None;
        }
        let absolute = repo_root.join(relative);
        let (source, tree) = self.parse_file(&absolute)?;
        let point = Point::new(pos.line as usize, pos.col as usize);
        let mut node = tree
            .root_node()
            .named_descendant_for_point_range(point, point)?;

        loop {
            if declaration_kind(node.kind()).is_some() {
                return resolution_for_node(relative_path, &source, node);
            }
            node = node.parent()?;
        }
    }

    /// `None` means the identity cannot be verified by this fallback. `false`
    /// is returned only when the file or exact declaration is positively gone.
    pub(crate) fn identity_alive(&self, repo_root: &Path, identity: &str) -> Option<bool> {
        let parsed = parse_identity(identity)?;
        let relative = safe_relative_path(parsed.path)?;
        if !Self::supports_path(relative) {
            return None;
        }
        let absolute = repo_root.join(relative);
        if !absolute.is_file() {
            return Some(false);
        }
        let (source, tree) = self.parse_file(&absolute)?;
        Some(tree_contains_identity(
            tree.root_node(),
            parsed.path,
            &source,
            identity,
        ))
    }

    fn parse_file(&self, absolute: &Path) -> Option<(String, Tree)> {
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
        parser.set_language(&grammar_for(absolute)?).ok()?;
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

/// Extension-to-grammar registry. Adding another language is one match row plus
/// its identity prefix; Rust is intentionally the only supported row in this slice.
fn grammar_for(path: &Path) -> Option<tree_sitter::Language> {
    match path.extension()?.to_str()? {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedIdentity<'a> {
    pub path: &'a str,
    pub kind: &'a str,
    pub qualified_name: &'a str,
}

pub(crate) fn parse_identity(identity: &str) -> Option<ParsedIdentity<'_>> {
    let mut fields = identity.splitn(5, ':');
    if fields.next()? != "ts" || fields.next()? != "rust" {
        return None;
    }
    let parsed = ParsedIdentity {
        path: fields.next()?,
        kind: fields.next()?,
        qualified_name: fields.next()?,
    };
    (!parsed.path.is_empty()
        && declaration_kind_for_identity(parsed.kind)
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

fn declaration_kind_for_identity(kind: &str) -> bool {
    matches!(
        kind,
        "fn" | "struct"
            | "enum"
            | "union"
            | "trait"
            | "impl"
            | "mod"
            | "const"
            | "static"
            | "type"
            | "macro"
    )
}

fn resolution_for_node(
    relative_path: &str,
    source: &str,
    node: Node<'_>,
) -> Option<SyntacticResolution> {
    let kind = declaration_kind(node.kind())?;
    let name = declaration_name(node, source)?;
    let qualified_name = qualified_name(node, source)?;
    let start = node.start_position();
    let end = node.end_position();
    Some(SyntacticResolution {
        identity: format!("ts:rust:{relative_path}:{kind}:{qualified_name}"),
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

fn declaration_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "impl_item" {
        let ty = node_text(node.child_by_field_name("type")?, source)?;
        return match node.child_by_field_name("trait") {
            Some(trait_node) => Some(format!("<{ty} as {}>", node_text(trait_node, source)?)),
            None => Some(ty.to_string()),
        };
    }
    Some(node_text(node.child_by_field_name("name")?, source)?.to_string())
}

fn qualified_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut names = Vec::new();
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        if declaration_kind(current.kind()).is_some() {
            names.push(declaration_name(current, source)?);
        }
        cursor = current.parent();
    }
    names.reverse();
    Some(names.join("::"))
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.byte_range())
}

fn tree_contains_identity(node: Node<'_>, path: &str, source: &str, identity: &str) -> bool {
    if declaration_kind(node.kind()).is_some()
        && resolution_for_node(path, source, node)
            .is_some_and(|resolution| resolution.identity == identity)
    {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|child| tree_contains_identity(child, path, source, identity));
    found
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    const FIXTURE_PATH: &str = "tests/fixtures/ts_fallback.rs";

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn position_of(needle: &str) -> Position {
        let source = fs::read_to_string(root().join(FIXTURE_PATH)).unwrap();
        let offset = source.find(needle).unwrap();
        let before = &source[..offset];
        Position {
            line: before.bytes().filter(|byte| *byte == b'\n').count() as u32,
            col: before.rsplit('\n').next().unwrap().len() as u32,
        }
    }

    fn resolve(needle: &str) -> Option<SyntacticResolution> {
        TreeSitterFallback::default().resolve_position(&root(), FIXTURE_PATH, position_of(needle))
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
