//! Grounding against the code index. Edit-time: before an edit applies, find the
//! attributes of function parameters it compares with string literals
//! (`param.name`, or `getattr(param, 'name'[, default])` with a literal name).
//! Plan-time: read the same kinds of names, and the quoted literals written after
//! them, from a disputed plan's text. Either way, look those names up across the
//! code index and report where they are defined and whether a compared literal
//! is missing from a definition's values. Deterministic and read-only: no model,
//! no graph write. Python first; other languages yield no keys yet.
use super::*;
use crate::code::substrate::symbols::{descriptor_role, DescriptorRole};
use crate::code::substrate::{self, FileDefinition};
use tree_sitter::Node;

const MAX_KEYS: usize = 8;
const MAX_DEFINITIONS: usize = 3;
const PREVIEW_LINES: usize = 12;
const PREVIEW_BYTES: usize = 256;
const PREVIEW_TOTAL_BYTES: usize = 1024;
/// Plan text read for names; prose past this is ignored.
const PLAN_TEXT_BYTES: usize = 16 * 1024;
/// A quoted literal counts as compared with a name written at most this many
/// bytes before it, in the same clause.
const LITERAL_WINDOW: usize = 64;
/// Longest quoted literal read from plan text.
const LITERAL_BYTES: usize = 64;
/// A dotted token ending in one of these is a file name, not an attribute.
const FILE_EXTENSIONS: &[&str] = &[
    "py", "rs", "ts", "tsx", "js", "jsx", "md", "txt", "json", "toml", "yaml", "yml", "cfg", "ini",
    "html", "css", "sh",
];

pub async fn ground(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GroundRequest>,
) -> Result<Json<GroundResponse>, ApiError> {
    Ok(Json(
        tokio::task::spawn_blocking(move || ground_edit(&state, &request))
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

pub fn ground_edit(state: &AppState, request: &GroundRequest) -> anyhow::Result<GroundResponse> {
    validate_path(&request.file)?;
    let mut response = GroundResponse::default();
    if request.ranges_coalesced || !request.file.ends_with(".py") {
        return Ok(response);
    }
    response.keys = edit_keys(&request.file, &request.after, &request.ranges);
    resolve_keys(state, &mut response, std::slice::from_ref(&request.file));
    Ok(response)
}

pub async fn ground_plan(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PlanGroundRequest>,
) -> Result<Json<GroundResponse>, ApiError> {
    Ok(Json(
        tokio::task::spawn_blocking(move || ground_plan_text(&state, &request))
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

/// Ground a disputed plan: the names its text reads and the literals it compares
/// them with ([`plan_keys`]), resolved like an edit's keys. The plan's own files
/// are the change, not what it compares against, so their definitions are
/// skipped.
pub fn ground_plan_text(
    state: &AppState,
    request: &PlanGroundRequest,
) -> anyhow::Result<GroundResponse> {
    for file in &request.files {
        validate_path(file)?;
    }
    let mut response = GroundResponse {
        keys: plan_keys(&request.text, &request.files),
        ..GroundResponse::default()
    };
    resolve_keys(state, &mut response, &request.files);
    Ok(response)
}

/// Look each key up by definition name, skipping definitions in `excluded`
/// files: at most [`MAX_DEFINITIONS`], each previewed only from proven source,
/// and a compared literal missing from a preview that quotes other values is a
/// mismatch.
fn resolve_keys(state: &AppState, response: &mut GroundResponse, excluded: &[String]) {
    let Some(substrate) = state.substrate() else {
        return;
    };
    let mut preview_bytes = 0usize;
    'keys: for key in &response.keys {
        for definition in substrate.definitions_named(&key.attribute) {
            if response.definitions.len() >= MAX_DEFINITIONS {
                break 'keys;
            }
            let Some(found) = grounded_definition(excluded, &definition) else {
                continue;
            };
            let budget = PREVIEW_BYTES.min(PREVIEW_TOTAL_BYTES.saturating_sub(preview_bytes));
            let preview = if budget == 0 {
                String::new()
            } else {
                substrate
                    .definition_preview(&definition, PREVIEW_LINES, budget)
                    .unwrap_or_default()
            };
            preview_bytes += preview.len();
            let values = string_literals(&preview);
            if !values.is_empty() {
                for literal in &key.literals {
                    if !values.contains(literal) {
                        response.mismatches.push(GroundMismatch {
                            key: key.attribute.clone(),
                            literal: literal.clone(),
                            file: definition.entry.file.clone(),
                            definition: found.0.clone(),
                        });
                    }
                }
            }
            response.definitions.push(GroundDefinition {
                key: key.attribute.clone(),
                name: found.0,
                file: definition.entry.file.clone(),
                symbol: definition.entry.symbol.clone(),
                role: found.1.into(),
                preview,
            });
        }
    }
}

/// A definition's name and role when it may ground: not in an excluded (edited
/// or planned) file, not test code or harness state, and not a parameter or
/// local.
fn grounded_definition(
    excluded: &[String],
    definition: &FileDefinition,
) -> Option<(String, &'static str)> {
    let file = &definition.entry.file;
    if excluded.iter().any(|excluded| excluded == file)
        || substrate::is_test_path(file)
        || file.starts_with(".moosedev")
    {
        return None;
    }
    let role = match descriptor_role(&definition.entry.symbol) {
        Some(DescriptorRole::Parameter) | Some(DescriptorRole::Local) => return None,
        Some(DescriptorRole::TypeMember) => "type_member",
        Some(DescriptorRole::Declaration) | None => "declaration",
    };
    Some((
        definition.entry.display_name.clone().unwrap_or_default(),
        role,
    ))
}

/// Attributes of the enclosing function's parameters, read as `param.name` or as
/// `getattr(param, 'name'[, default])` with a literal name, that the changed ranges
/// compare with string literals (`==`, `!=`, `in`, `not in`, `match`/`case`),
/// deduplicated in source order, at most [`MAX_KEYS`]. A syntax error or no
/// ranges yields no keys.
pub fn edit_keys(file: &str, source: &str, ranges: &[HarnessSourceRange]) -> Vec<GroundKey> {
    let Some(tree) = substrate::parse_source(file, source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    if root.has_error() || ranges.is_empty() {
        return Vec::new();
    }
    let mut keys: Vec<GroundKey> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
        let Some((object, name)) = accessed_attribute(node, source) else {
            continue;
        };
        let owner = text(object, source);
        if object.kind() != "identifier" || owner == "self" || owner == "cls" {
            continue;
        }
        let row = node.start_position().row as u32;
        // Hunk ranges are end-exclusive: an end at column 0 excludes its line.
        if !ranges.iter().any(|range| {
            row >= range.start.line
                && (row < range.end.line || (row == range.end.line && range.end.col > 0))
        }) {
            continue;
        }
        if !enclosing_parameters(node, source)
            .iter()
            .any(|name| name == owner)
        {
            continue;
        }
        let literals = compared_literals(node, source);
        if literals.is_empty() {
            continue;
        }
        if let Some(existing) = keys.iter_mut().find(|key| key.attribute == name) {
            for literal in literals {
                if !existing.literals.contains(&literal) {
                    existing.literals.push(literal);
                }
            }
        } else if keys.len() < MAX_KEYS {
            keys.push(GroundKey {
                attribute: name,
                literals,
            });
        }
    }
    keys
}

/// Names a plan's prose says it reads, with the quoted literals it compares
/// them with. Plan text is prose, so it is scanned, not parsed:
/// - `x.name` (the last segment of a dotted token) and `getattr(x, 'name'[, default])`
///   with a literal name read `name`, unless `x` is `self` or `cls`, the name is a
///   single character, or the token is a file name (a plan file or a known
///   extension);
/// - a quoted literal (single or double quotes, no whitespace, not an apostrophe
///   inside a word) is compared with the nearest name written at most
///   [`LITERAL_WINDOW`] bytes before it in the same clause.
///
/// Names compared with a literal come first, in text order, then the rest; at
/// most [`MAX_KEYS`].
pub fn plan_keys(text: &str, files: &[String]) -> Vec<GroundKey> {
    let mut end = text.len().min(PLAN_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = &text[..end];
    let bytes = text.as_bytes();
    // (start, end, name) of each read name; spans of getattr name literals.
    let mut accesses: Vec<(usize, usize, String)> = Vec::new();
    let mut name_literals: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let starts_word = is_identifier_start(bytes[i])
            && (i == 0 || !(is_identifier(bytes[i - 1]) || bytes[i - 1] == b'.'));
        if !starts_word {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = identifier_end(bytes, i);
        let object = &text[start..j];
        if object == "getattr" {
            if let Some((end, owner, name, literal)) = getattr_access(text, j) {
                if owner != "self" && owner != "cls" && name.len() > 1 {
                    accesses.push((start, end, name));
                }
                name_literals.push(literal);
                i = end;
                continue;
            }
        }
        let mut last = None;
        while j + 1 < bytes.len() && bytes[j] == b'.' && is_identifier_start(bytes[j + 1]) {
            let segment_end = identifier_end(bytes, j + 1);
            last = Some((j + 1, segment_end));
            j = segment_end;
        }
        if let Some((name_start, name_end)) = last {
            let name = &text[name_start..name_end];
            let token = &text[start..j];
            let file_name = FILE_EXTENSIONS.contains(&name)
                || files
                    .iter()
                    .any(|file| file == token || file.ends_with(&format!("/{token}")));
            if object != "self" && object != "cls" && name.len() > 1 && !file_name {
                accesses.push((start, j, name.to_string()));
            }
        }
        i = j;
    }
    let mut compared: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let quote = bytes[i];
        let opens =
            (quote == b'\'' || quote == b'"') && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric());
        if !opens {
            i += 1;
            continue;
        }
        let limit = (i + 1 + LITERAL_BYTES + 1).min(bytes.len());
        let Some(close) = (i + 1..limit).find(|&k| bytes[k] == quote) else {
            i += 1;
            continue;
        };
        let content = &text[i + 1..close];
        let closes = close + 1 == bytes.len() || !bytes[close + 1].is_ascii_alphanumeric();
        let in_name = name_literals
            .iter()
            .any(|&(from, to)| i >= from && close < to);
        if closes && !content.is_empty() && !content.chars().any(char::is_whitespace) && !in_name {
            let owner = accesses
                .iter()
                .enumerate()
                .filter(|(_, access)| access.1 <= i && i - access.1 <= LITERAL_WINDOW)
                .filter(|(_, access)| !ends_clause(&text[access.1..i]))
                .map(|(index, _)| index)
                .next_back();
            if let Some(owner) = owner {
                compared.push((owner, content.to_string()));
            }
            i = close + 1;
        } else {
            i += 1;
        }
    }
    let mut keys: Vec<GroundKey> = Vec::new();
    for (index, (_, _, name)) in accesses.iter().enumerate() {
        let literals = compared
            .iter()
            .filter(|(owner, _)| *owner == index)
            .map(|(_, literal)| literal.clone());
        let key = match keys.iter_mut().position(|key| &key.attribute == name) {
            Some(position) => &mut keys[position],
            None => {
                keys.push(GroundKey {
                    attribute: name.clone(),
                    literals: Vec::new(),
                });
                keys.last_mut().expect("pushed")
            }
        };
        for literal in literals {
            if !key.literals.contains(&literal) {
                key.literals.push(literal);
            }
        }
    }
    keys.sort_by_key(|key| key.literals.is_empty());
    keys.truncate(MAX_KEYS);
    keys
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn identifier_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && is_identifier(bytes[end]) {
        end += 1;
    }
    end
}

/// `getattr(owner, 'name'[, default])` starting just after the word `getattr`:
/// the end of the call, the owner, the literal name and the span of its quotes.
fn getattr_access(text: &str, from: usize) -> Option<(usize, String, String, (usize, usize))> {
    let bytes = text.as_bytes();
    let skip = |mut at: usize| {
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        at
    };
    let mut at = skip(from);
    if bytes.get(at) != Some(&b'(') {
        return None;
    }
    at = skip(at + 1);
    if !bytes.get(at).copied().is_some_and(is_identifier_start) {
        return None;
    }
    let owner_end = identifier_end(bytes, at);
    let owner = text[at..owner_end].to_string();
    at = skip(owner_end);
    if bytes.get(at) != Some(&b',') {
        return None;
    }
    at = skip(at + 1);
    let quote = *bytes.get(at)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let name_end = identifier_end(bytes, at + 1);
    if name_end == at + 1 || bytes.get(name_end) != Some(&quote) {
        return None;
    }
    let name = text[at + 1..name_end].to_string();
    let literal = (at, name_end + 1);
    let close = (name_end + 1..bytes.len().min(name_end + 1 + LITERAL_WINDOW))
        .find(|&k| bytes[k] == b')')?;
    Some((close + 1, owner, name, literal))
}

/// True when the text between a name and a literal crosses a clause boundary.
fn ends_clause(between: &str) -> bool {
    let bytes = between.as_bytes();
    bytes.iter().enumerate().any(|(index, &byte)| {
        byte == b';'
            || byte == b'\n'
            || (matches!(byte, b'.' | b'?' | b'!')
                && bytes
                    .get(index + 1)
                    .is_none_or(|next| next.is_ascii_whitespace()))
    })
}

fn text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// The object and attribute name of an attribute access (`order.channel`) or of
/// a `getattr` whose name is a plain string literal (`getattr(order, 'channel')`,
/// with or without a default). A computed or interpolated name reads nothing.
fn accessed_attribute<'tree>(node: Node<'tree>, source: &str) -> Option<(Node<'tree>, String)> {
    match node.kind() {
        "attribute" => {
            let object = node.child_by_field_name("object")?;
            let attribute = node.child_by_field_name("attribute")?;
            Some((object, text(attribute, source).to_string()))
        }
        "call" => {
            let function = node.child_by_field_name("function")?;
            if function.kind() != "identifier" || text(function, source) != "getattr" {
                return None;
            }
            let arguments = node.child_by_field_name("arguments")?;
            let mut cursor = arguments.walk();
            let operands: Vec<Node> = arguments.named_children(&mut cursor).collect();
            if !(2..=3).contains(&operands.len()) || operands[1].kind() != "string" {
                return None;
            }
            let mut parts = operands[1].walk();
            let mut name = String::new();
            for part in operands[1].named_children(&mut parts) {
                match part.kind() {
                    "string_content" => name.push_str(text(part, source)),
                    "string_start" | "string_end" => {}
                    _ => return None,
                }
            }
            let identifier =
                !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
            identifier.then_some((operands[0], name))
        }
        _ => None,
    }
}

/// Parameter names of the nearest enclosing function definition.
fn enclosing_parameters(node: Node, source: &str) -> Vec<String> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if candidate.kind() == "function_definition" {
            let Some(parameters) = candidate.child_by_field_name("parameters") else {
                return Vec::new();
            };
            let mut cursor = parameters.walk();
            return parameters
                .named_children(&mut cursor)
                .filter_map(|parameter| {
                    if parameter.kind() == "identifier" {
                        return Some(text(parameter, source).to_string());
                    }
                    let name = parameter.child_by_field_name("name").or_else(|| {
                        let mut inner = parameter.walk();
                        let found = parameter
                            .named_children(&mut inner)
                            .find(|child| child.kind() == "identifier");
                        found
                    })?;
                    Some(text(name, source).to_string())
                })
                .collect();
        }
        current = candidate.parent();
    }
    Vec::new()
}

/// String literals the attribute access (or `getattr` call) is compared with: the
/// other operands of its comparison, or the case patterns of a match on it.
fn compared_literals(attribute: Node, source: &str) -> Vec<String> {
    let Some(parent) = attribute.parent() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match parent.kind() {
        "comparison_operator" => {
            let mut cursor = parent.walk();
            for operand in parent.named_children(&mut cursor) {
                if operand.id() != attribute.id() {
                    collect_strings(operand, source, &mut out);
                }
            }
        }
        "match_statement"
            if parent.child_by_field_name("subject").map(|s| s.id()) == Some(attribute.id()) =>
        {
            let mut stack = vec![parent];
            while let Some(node) = stack.pop() {
                if node.kind() == "case_pattern" {
                    collect_strings(node, source, &mut out);
                    continue;
                }
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
        }
        _ => {}
    }
    out.sort();
    out.dedup();
    out
}

fn collect_strings(node: Node, source: &str, out: &mut Vec<String>) {
    if node.kind() == "string" {
        let mut cursor = node.walk();
        let content: String = node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "string_content")
            .map(|child| text(child, source))
            .collect();
        out.push(content);
        return;
    }
    if !matches!(
        node.kind(),
        "tuple" | "list" | "set" | "parenthesized_expression" | "case_pattern" | "union_pattern"
    ) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_strings(child, source, out);
    }
}

/// Quoted string values in a preview, single or double quoted.
pub fn string_literals(preview: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut chars = preview.char_indices().peekable();
    while let Some((start, quote)) = chars.next() {
        if quote != '"' && quote != '\'' {
            continue;
        }
        let mut value = String::new();
        let mut closed = false;
        for (_, c) in chars.by_ref() {
            if c == quote {
                closed = true;
                break;
            }
            if c == '\n' {
                break;
            }
            value.push(c);
        }
        let _ = start;
        if closed {
            out.insert(value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(first: u32, last: u32) -> Vec<HarnessSourceRange> {
        vec![HarnessSourceRange {
            start: HarnessSourcePosition {
                line: first,
                col: 0,
            },
            end: HarnessSourcePosition { line: last, col: 0 },
        }]
    }

    const SOURCE: &str = "def price(order, today):\n    if order.channel == 'wire' or order.channel in (\"cash\", \"card\"):\n        return 0\n    match order.region:\n        case \"north\":\n            return 1\n    note = lookup(today)\n    if note.kind == 'x':\n        return 2\n    return order.amount + today\n";

    #[test]
    fn keys_are_parameter_attributes_compared_with_literals() {
        let keys = edit_keys("price.py", SOURCE, &lines(0, 10));
        let found: Vec<(String, Vec<String>)> = keys
            .iter()
            .map(|key| {
                let mut literals = key.literals.clone();
                literals.sort();
                (key.attribute.clone(), literals)
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    "channel".to_string(),
                    vec!["card".to_string(), "cash".to_string(), "wire".to_string()]
                ),
                ("region".to_string(), vec!["north".to_string()]),
            ],
            "a local's attribute and an uncompared access are not keys"
        );
    }

    #[test]
    fn ranges_limit_keys_and_syntax_errors_yield_none() {
        let keys = edit_keys("price.py", SOURCE, &lines(3, 5));
        assert_eq!(
            keys.iter()
                .map(|k| k.attribute.as_str())
                .collect::<Vec<_>>(),
            vec!["region"]
        );
        assert!(edit_keys(
            "price.py",
            "def broken(order:\n    order.kind == 'a'\n",
            &lines(0, 3)
        )
        .is_empty());
        assert!(edit_keys("price.py", SOURCE, &[]).is_empty());
        assert!(edit_keys("price.rs", SOURCE, &lines(0, 10)).is_empty());
    }

    #[test]
    fn preview_literals_are_quoted_values() {
        let values =
            string_literals("TABLE = {\n    \"wire\": \"Wire\",\n    'card': 'Card',\n}\n");
        assert!(values.contains("wire") && values.contains("card") && values.contains("Wire"));
        assert!(!values.contains("cash"));
        assert!(string_literals("limit = 5").is_empty());
    }
}
