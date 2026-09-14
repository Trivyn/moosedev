//! Edit-time grounding: before an edit applies, find the attributes of function
//! parameters it compares with string literals, look those names up across the
//! code index, and report where they are defined and whether a compared literal
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
    let Some(substrate) = state.substrate() else {
        return Ok(response);
    };
    let mut preview_bytes = 0usize;
    'keys: for key in &response.keys {
        for definition in substrate.definitions_named(&key.attribute) {
            if response.definitions.len() >= MAX_DEFINITIONS {
                break 'keys;
            }
            let Some(found) = grounded_definition(&request.file, &definition) else {
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
    Ok(response)
}

/// A definition's name and role when it may ground an edit: not in the edited
/// file, not test code or harness state, and not a parameter or local.
fn grounded_definition(
    edited: &str,
    definition: &FileDefinition,
) -> Option<(String, &'static str)> {
    let file = &definition.entry.file;
    if file == edited || substrate::is_test_path(file) || file.starts_with(".moosedev") {
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

/// Attributes of the enclosing function's parameters that the changed ranges
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
        if node.kind() != "attribute" {
            continue;
        }
        let (Some(object), Some(attribute)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("attribute"),
        ) else {
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
        let name = text(attribute, source).to_string();
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

fn text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
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

/// String literals the attribute is compared with: the other operands of its
/// comparison, or the case patterns of a match on it.
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
