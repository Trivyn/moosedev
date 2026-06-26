//! Shared helpers for generated graph artifact renderers.

use std::collections::HashMap;

use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

pub(crate) fn select_rows(
    store: &Store,
    query: &str,
) -> anyhow::Result<Vec<HashMap<String, String>>> {
    let results = SparqlEvaluator::new()
        .parse_query(query)
        .map_err(|e| anyhow::anyhow!("parse query: {e}"))?
        .on_store(store)
        .execute()
        .map_err(|e| anyhow::anyhow!("execute query: {e}"))?;

    let QueryResults::Solutions(solutions) = results else {
        anyhow::bail!("expected SELECT query results");
    };
    let vars: Vec<String> = solutions
        .variables()
        .iter()
        .map(|var| var.as_str().to_string())
        .collect();
    let mut rows = Vec::new();
    for solution in solutions {
        let solution = solution?;
        let mut row = HashMap::new();
        for var in &vars {
            if let Some(term) = solution.get(var.as_str()) {
                row.insert(var.clone(), term_text(term));
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

pub(crate) fn required_value(row: &HashMap<String, String>, key: &str) -> anyhow::Result<String> {
    row.get(key)
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required SPARQL binding {key:?}"))
}

fn term_text(term: &Term) -> String {
    match term {
        Term::NamedNode(node) => node.as_str().to_string(),
        Term::BlankNode(node) => node.as_str().to_string(),
        Term::Literal(literal) => literal.value().to_string(),
        #[allow(unreachable_patterns)]
        _ => term.to_string(),
    }
}

pub(crate) fn slugify(title: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

pub(crate) fn date_only(timestamp: &str) -> String {
    if let Some((date, _)) = timestamp.split_once('T') {
        date.to_string()
    } else {
        timestamp.chars().take(10).collect()
    }
}

pub(crate) fn md_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

pub(crate) fn nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn not_recorded(value: &str) -> String {
    nonempty(value).unwrap_or("not recorded").to_string()
}

pub(crate) fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
