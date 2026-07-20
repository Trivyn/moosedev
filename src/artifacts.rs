//! Shared helpers for generated graph artifact renderers.

use std::collections::HashMap;

use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct LifecycleLinks {
    pub supersedes: Vec<String>,
    pub superseded_by: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ArtifactRecordLink {
    pub label: String,
    pub filename: String,
}

pub(crate) fn fetch_lifecycle_links<'a>(
    state: &AppState,
    record_iris: impl IntoIterator<Item = &'a str>,
    batch_size: usize,
) -> anyhow::Result<HashMap<String, LifecycleLinks>> {
    if batch_size == 0 {
        anyhow::bail!("lifecycle batch size must be >= 1");
    }

    let record_iris = record_iris.into_iter().collect::<Vec<_>>();
    if record_iris.is_empty() {
        return Ok(HashMap::new());
    }
    let mut links = record_iris
        .iter()
        .map(|iri| ((*iri).to_string(), LifecycleLinks::default()))
        .collect::<HashMap<_, _>>();
    let supersedes = state.resolve_object_property("supersedes")?;
    let superseded_by = state.resolve_object_property("isSupersededBy")?;

    for batch in record_iris.chunks(batch_size) {
        let values = batch
            .iter()
            .map(|iri| format!("<{iri}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let query = format!(
            r#"
SELECT ?record ?direction ?other WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{
    VALUES ?record {{ {values} }}
    {{ ?record <{supersedes}> ?other . BIND("supersedes" AS ?direction) }}
    UNION
    {{ ?record <{superseded_by}> ?other . BIND("supersededBy" AS ?direction) }}
    FILTER(isIRI(?other))
  }}
}} ORDER BY ?record ?direction ?other
"#
        );
        for row in select_rows(&state.store, &query)? {
            let record = required_value(&row, "record")?;
            let other = required_value(&row, "other")?;
            let record_links = links.entry(record).or_default();
            let targets = match row.get("direction").map(String::as_str) {
                Some("supersedes") => &mut record_links.supersedes,
                Some("supersededBy") => &mut record_links.superseded_by,
                _ => continue,
            };
            if !targets.contains(&other) {
                targets.push(other);
            }
        }
    }

    Ok(links)
}

pub(crate) fn lifecycle_for<'a>(
    lifecycle: &'a HashMap<String, LifecycleLinks>,
    iri: &str,
) -> &'a LifecycleLinks {
    static EMPTY: std::sync::OnceLock<LifecycleLinks> = std::sync::OnceLock::new();
    lifecycle
        .get(iri)
        .unwrap_or_else(|| EMPTY.get_or_init(LifecycleLinks::default))
}

pub(crate) fn render_lifecycle_status(
    status: &str,
    record_iri: &str,
    lifecycle: &HashMap<String, LifecycleLinks>,
    records: &HashMap<String, ArtifactRecordLink>,
    linked: bool,
) -> String {
    if status.eq_ignore_ascii_case("superseded") {
        return lifecycle_for(lifecycle, record_iri)
            .superseded_by
            .iter()
            .find_map(|iri| records.get(iri))
            .map(|successor| {
                let successor = if linked {
                    artifact_link(successor)
                } else {
                    successor.label.clone()
                };
                format!("Superseded by {successor}")
            })
            .unwrap_or_else(|| "Superseded (successor not recorded)".to_string());
    }
    render_plain_status(status)
}

pub(crate) fn render_supersedes_lines(
    record_iri: &str,
    lifecycle: &HashMap<String, LifecycleLinks>,
    records: &HashMap<String, ArtifactRecordLink>,
) -> Vec<String> {
    lifecycle_for(lifecycle, record_iri)
        .supersedes
        .iter()
        .filter_map(|iri| records.get(iri))
        .map(|older| format!("- Supersedes: {}", artifact_link(older)))
        .collect()
}

pub(crate) fn render_plain_status(status: &str) -> String {
    match status.to_ascii_lowercase().as_str() {
        "accepted" => "Accepted".to_string(),
        "proposed" => "Proposed".to_string(),
        "deprecated" => "Deprecated".to_string(),
        "superseded" => "Superseded".to_string(),
        "" => "not recorded".to_string(),
        other => capitalize(other),
    }
}

fn artifact_link(record: &ArtifactRecordLink) -> String {
    format!("[{}]({})", record.label, record.filename)
}

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
