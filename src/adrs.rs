//! Deterministic ADR artifact rendering from the project knowledge graph.
//!
//! The graph remains the source of truth. This module renders a read-only ADR
//! view for UI/API callers; writing `docs/adr` is intentionally left to the
//! checked-in script/skill workflow.

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Write};

use chrono::{SecondsFormat, Utc};
use oxigraph::store::Store;
use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::artifacts::{
    capitalize, date_only, md_cell, nonempty, not_recorded, required_value, select_rows,
};

pub const INDEX_FILENAME: &str = "0000-index.md";

const COUNT_QUERY: &str = r#"
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
SELECT (COUNT(?ad) AS ?n) WHERE {
  GRAPH <https://moosedev.dev/kg/project> { ?ad a :ArchitecturalDecision . }
}
"#;

const ENUM_QUERY: &str = r#"
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?ad ?title ?status ?ts ?author WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    ?ad a :ArchitecturalDecision ;
        rdfs:label ?title ;
        :hasLifecycleStatus ?status ;
        :hasTimestamp ?ts .
    OPTIONAL { ?ad :hasAuthor ?author }
  }
} ORDER BY ?ts ?ad
"#;

const CLUSTER_QUERY_TEMPLATE: &str = r#"
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?ad ?dir ?rel ?node ?nlabel ?ndesc WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    VALUES ?ad { __VALUES__ }
    {
      ?ad ?p ?node . FILTER(isIRI(?node))
      BIND("out" AS ?dir)
      BIND(REPLACE(STR(?p), "^.*[/#]", "") AS ?rel)
      FILTER(?rel IN ("isMotivatedBy","weighs","resultsIn","concerns",
                      "hasRationale","supersedes","isSupersededBy"))
      OPTIONAL { ?node rdfs:label ?nlabel }
      OPTIONAL { ?node :hasDescription ?ndesc }
    } UNION {
      ?node :constrains ?ad .
      BIND("in" AS ?dir) BIND("constrains" AS ?rel)
      OPTIONAL { ?node rdfs:label ?nlabel }
      OPTIONAL { ?node :hasDescription ?ndesc }
    } UNION {
      ?ad :hasDescription ?ndesc .
      BIND("self" AS ?dir) BIND("hasDescription" AS ?rel) BIND(?ad AS ?node)
    }
  }
} ORDER BY ?ad ?dir ?rel
"#;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AdrGenerationOptions {
    pub batch_size: usize,
}

impl Default for AdrGenerationOptions {
    fn default() -> Self {
        Self { batch_size: 20 }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AdrSet {
    pub generated_at: String,
    pub graph_decisions: usize,
    pub adr_files: usize,
    pub index_filename: String,
    pub index_markdown: String,
    pub adrs: Vec<AdrDocument>,
    pub warnings: AdrWarnings,
}

impl AdrSet {
    pub fn summaries(&self) -> Vec<AdrSummary> {
        self.adrs.iter().map(AdrDocument::summary).collect()
    }

    pub fn find_by_num(&self, num: &str) -> Option<&AdrDocument> {
        self.adrs.iter().find(|adr| adr.num == num)
    }

    pub fn zip_archive(&self) -> anyhow::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o644);

            writer.start_file(INDEX_FILENAME, options)?;
            writer.write_all(self.index_markdown.as_bytes())?;
            for adr in &self.adrs {
                writer.start_file(adr.filename.as_str(), options)?;
                writer.write_all(adr.markdown.as_bytes())?;
            }
            writer.finish()?;
        }
        Ok(cursor.into_inner())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AdrSummary {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AdrDocument {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub markdown: String,
}

impl AdrDocument {
    pub fn summary(&self) -> AdrSummary {
        AdrSummary {
            num: self.num.clone(),
            title: self.title.clone(),
            status: self.status.clone(),
            date: self.date.clone(),
            author: self.author.clone(),
            iri: self.iri.clone(),
            filename: self.filename.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct AdrWarnings {
    pub missing_context: Vec<String>,
    pub missing_decision: Vec<String>,
    pub missing_successor: Vec<String>,
    pub missing_reciprocal: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Meta {
    num: String,
    iri: String,
    title: String,
    status: String,
    ts: String,
    author: String,
    slug: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct Cluster {
    rows: BTreeMap<String, Vec<Row>>,
}

impl Cluster {
    fn push(&mut self, row: Row) {
        self.rows.entry(row.rel.clone()).or_default().push(row);
    }

    fn get(&self, rel: &str) -> &[Row] {
        self.rows.get(rel).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Row {
    ad: String,
    rel: String,
    node: String,
    nlabel: String,
    ndesc: String,
}

pub fn generate_adr_set(store: &Store, options: AdrGenerationOptions) -> anyhow::Result<AdrSet> {
    if options.batch_size == 0 {
        anyhow::bail!("ADR generation batch size must be >= 1");
    }

    let count = count_decisions(store)?;
    let records = enumerate_records(store)?;
    let by_iri: HashMap<String, Meta> = records
        .iter()
        .map(|record| (record.iri.clone(), record.clone()))
        .collect();
    let clusters = fetch_clusters(store, &records, options.batch_size)?;
    let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    if records.is_empty() {
        return Ok(AdrSet {
            generated_at,
            graph_decisions: count,
            adr_files: 0,
            index_filename: INDEX_FILENAME.to_string(),
            index_markdown:
                "# Architecture Decision Records\n\nNo architectural decisions recorded yet.\n"
                    .to_string(),
            adrs: Vec::new(),
            warnings: AdrWarnings::default(),
        });
    }

    let adrs: Vec<AdrDocument> = records
        .iter()
        .map(|meta| render_adr_document(meta, &clusters, &by_iri))
        .collect();
    let warnings = summarize_warnings(&records, &clusters, &by_iri);
    let index_markdown = render_index(&records, &clusters, &by_iri, &generated_at);

    Ok(AdrSet {
        generated_at,
        graph_decisions: count,
        adr_files: adrs.len(),
        index_filename: INDEX_FILENAME.to_string(),
        index_markdown,
        adrs,
        warnings,
    })
}

fn count_decisions(store: &Store) -> anyhow::Result<usize> {
    let rows = select_rows(store, COUNT_QUERY)?;
    rows.first()
        .and_then(|row| row.get("n"))
        .unwrap_or(&"0".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("parse decision count: {e}"))
}

fn enumerate_records(store: &Store) -> anyhow::Result<Vec<Meta>> {
    let mut seen_slugs: HashMap<String, usize> = HashMap::new();
    select_rows(store, ENUM_QUERY)?
        .into_iter()
        .enumerate()
        .map(|(idx, row)| {
            let title = row.get("title").cloned().unwrap_or_default();
            let base_slug = slugify(&title);
            let slug_count = seen_slugs.entry(base_slug.clone()).or_insert(0);
            *slug_count += 1;
            let slug = if *slug_count == 1 {
                base_slug
            } else {
                format!("{base_slug}-{slug_count}")
            };
            Ok(Meta {
                num: format!("{:04}", idx + 1),
                iri: required_value(&row, "ad")?,
                title,
                status: row.get("status").cloned().unwrap_or_default(),
                ts: row.get("ts").cloned().unwrap_or_default(),
                author: row.get("author").cloned().unwrap_or_default(),
                slug,
            })
        })
        .collect()
}

fn fetch_clusters(
    store: &Store,
    records: &[Meta],
    batch_size: usize,
) -> anyhow::Result<HashMap<String, Cluster>> {
    let mut clusters: HashMap<String, Cluster> = records
        .iter()
        .map(|record| (record.iri.clone(), Cluster::default()))
        .collect();

    for batch in records.chunks(batch_size) {
        let values = batch
            .iter()
            .map(|record| format!("<{}>", record.iri))
            .collect::<Vec<_>>()
            .join(" ");
        let query = CLUSTER_QUERY_TEMPLATE.replace("__VALUES__", &values);
        for row in select_rows(store, &query)? {
            let ad = required_value(&row, "ad")?;
            let rel = required_value(&row, "rel")?;
            let cluster = clusters.entry(ad.clone()).or_default();
            cluster.push(Row {
                ad,
                rel,
                node: row.get("node").cloned().unwrap_or_default(),
                nlabel: row.get("nlabel").cloned().unwrap_or_default(),
                ndesc: row.get("ndesc").cloned().unwrap_or_default(),
            });
        }
    }

    Ok(clusters)
}

fn render_adr_document(
    meta: &Meta,
    clusters: &HashMap<String, Cluster>,
    by_iri: &HashMap<String, Meta>,
) -> AdrDocument {
    let markdown = render_adr(meta, clusters, by_iri);
    AdrDocument {
        num: meta.num.clone(),
        title: meta.title.clone(),
        status: render_status(meta, clusters, by_iri),
        date: date_only(&meta.ts),
        author: not_recorded(&meta.author),
        iri: meta.iri.clone(),
        filename: filename(meta),
        markdown,
    }
}

fn render_adr(
    meta: &Meta,
    clusters: &HashMap<String, Cluster>,
    by_iri: &HashMap<String, Meta>,
) -> String {
    let cluster = cluster_for(clusters, meta);
    let mut lines = vec![
        format!("# {}. {}", meta.num, meta.title),
        String::new(),
        format!("- Status: {}", render_status(meta, clusters, by_iri)),
        format!("- Date: {}", date_only(&meta.ts)),
        format!("- Author: {}", not_recorded(&meta.author)),
    ];

    for older in cluster
        .get("supersedes")
        .iter()
        .filter_map(|row| by_iri.get(&row.node))
    {
        lines.push(format!("- Supersedes: {}", adr_link(older)));
    }

    lines.extend([String::new(), "## Context".to_string()]);
    let context_rows = cluster
        .get("isMotivatedBy")
        .iter()
        .chain(cluster.get("constrains").iter());
    let mut context_written = false;
    for row in context_rows {
        lines.push(node_bullet(row));
        context_written = true;
    }
    if !context_written {
        lines.push("No motivating requirement or constraint recorded.".to_string());
    }

    lines.extend([String::new(), "## Decision".to_string()]);
    let self_descs: Vec<&str> = cluster
        .get("hasDescription")
        .iter()
        .filter_map(|row| nonempty(&row.ndesc))
        .collect();
    let rationales: Vec<&str> = cluster
        .get("hasRationale")
        .iter()
        .filter_map(|row| nonempty(&row.ndesc))
        .collect();
    if self_descs.is_empty() {
        lines.push("No decision description recorded.".to_string());
    } else {
        lines.push(self_descs.join("\n\n"));
    }
    lines.push(String::new());
    if rationales.is_empty() {
        lines.push("No separate rationale recorded.".to_string());
    } else {
        lines.push(rationales.join("\n\n"));
    }

    lines.extend([String::new(), "## Considered Options".to_string()]);
    if cluster.get("weighs").is_empty() {
        lines.push("No alternatives recorded.".to_string());
    } else {
        lines.extend(cluster.get("weighs").iter().map(node_bullet));
    }

    lines.extend([String::new(), "## Consequences".to_string()]);
    if cluster.get("resultsIn").is_empty() {
        lines.push("No consequences recorded.".to_string());
    } else {
        lines.extend(cluster.get("resultsIn").iter().map(node_bullet));
    }

    if !cluster.get("concerns").is_empty() {
        lines.extend([String::new(), "## Affects".to_string()]);
        for row in cluster.get("concerns") {
            let label = nonempty(&row.nlabel).unwrap_or(&row.node);
            lines.push(format!("- {} (`{}`)", label, row.node));
        }
    }

    lines.extend([
        String::new(),
        "---".to_string(),
        format!(
            "Source: graph record `{}`. Generated view - regenerate from the graph; do not hand-edit.",
            meta.iri
        ),
        String::new(),
    ]);
    lines.join("\n")
}

fn render_index(
    records: &[Meta],
    clusters: &HashMap<String, Cluster>,
    by_iri: &HashMap<String, Meta>,
    generated_at: &str,
) -> String {
    let date = generated_at.split('T').next().unwrap_or(generated_at);
    let mut lines = vec![
        "# Architecture Decision Records".to_string(),
        String::new(),
        format!("> **Generated view.** Rendered from the MOOSEDev knowledge graph on {date}."),
        "> The graph is the source of truth - **regenerate, do not hand-edit.** Scope: architectural"
            .to_string(),
        "> decisions only; constraints, patterns, and lessons are rendered by sibling artifact skills."
            .to_string(),
        String::new(),
        "| # | Title | Status | Date |".to_string(),
        "|---|-------|--------|------|".to_string(),
    ];

    for meta in records {
        let status = render_status(meta, clusters, by_iri);
        let status = status
            .split("](")
            .next()
            .map(|prefix| prefix.trim_start_matches("Superseded by [").to_string())
            .filter(|prefix| prefix.starts_with("ADR-"))
            .map(|adr| format!("Superseded by {adr}"))
            .unwrap_or(status);
        lines.push(format!(
            "| {} | [{}]({}) | {} | {} |",
            meta.num,
            md_cell(&meta.title),
            filename(meta),
            md_cell(&status),
            date_only(&meta.ts)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn summarize_warnings(
    records: &[Meta],
    clusters: &HashMap<String, Cluster>,
    by_iri: &HashMap<String, Meta>,
) -> AdrWarnings {
    let mut warnings = AdrWarnings::default();
    for meta in records {
        let cluster = cluster_for(clusters, meta);

        for row in cluster.get("supersedes") {
            if let Some(older) = by_iri.get(&row.node) {
                let older_cluster = cluster_for(clusters, older);
                if !older_cluster
                    .get("isSupersededBy")
                    .iter()
                    .any(|inverse| inverse.node == meta.iri)
                {
                    warnings
                        .missing_reciprocal
                        .push(format!("{} -> {}", older.num, meta.num));
                }
            }
        }

        if cluster.get("isMotivatedBy").is_empty() && cluster.get("constrains").is_empty() {
            warnings.missing_context.push(meta.num.clone());
        }
        if cluster.get("hasDescription").is_empty() {
            warnings.missing_decision.push(meta.num.clone());
        }
        if meta.status.eq_ignore_ascii_case("superseded")
            && cluster.get("isSupersededBy").is_empty()
        {
            warnings.missing_successor.push(meta.num.clone());
        }
    }
    warnings
}

fn render_status(
    meta: &Meta,
    clusters: &HashMap<String, Cluster>,
    by_iri: &HashMap<String, Meta>,
) -> String {
    match meta.status.to_ascii_lowercase().as_str() {
        "accepted" => "Accepted".to_string(),
        "proposed" => "Proposed".to_string(),
        "deprecated" => "Deprecated".to_string(),
        "superseded" => cluster_for(clusters, meta)
            .get("isSupersededBy")
            .iter()
            .find_map(|row| by_iri.get(&row.node))
            .map(|successor| format!("Superseded by {}", adr_link(successor)))
            .unwrap_or_else(|| "Superseded (successor not recorded)".to_string()),
        "" => "not recorded".to_string(),
        other => capitalize(other),
    }
}

fn cluster_for<'a>(clusters: &'a HashMap<String, Cluster>, meta: &Meta) -> &'a Cluster {
    static EMPTY_CLUSTER: std::sync::OnceLock<Cluster> = std::sync::OnceLock::new();
    clusters
        .get(&meta.iri)
        .unwrap_or_else(|| EMPTY_CLUSTER.get_or_init(Cluster::default))
}

fn filename(meta: &Meta) -> String {
    format!("{}-{}.md", meta.num, meta.slug)
}

fn adr_link(meta: &Meta) -> String {
    format!("[ADR-{}]({})", meta.num, filename(meta))
}

fn slugify(title: &str) -> String {
    crate::artifacts::slugify(title, "decision")
}

fn node_bullet(row: &Row) -> String {
    match (nonempty(&row.nlabel), nonempty(&row.ndesc)) {
        (Some(label), Some(desc)) => format!("- {label}: {desc} (`{}`)", row.node),
        (Some(label), None) => format!("- {label} (`{}`)", row.node),
        (None, Some(desc)) => format!("- {desc} (`{}`)", row.node),
        (None, None) => format!("- `{}`", row.node),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_disambiguates_empty_and_repeated_titles() {
        assert_eq!(
            slugify("ADR generation uses a checked-in script"),
            "adr-generation-uses-a-checked-in-script"
        );
        assert_eq!(slugify(""), "decision");
    }

    #[test]
    fn render_status_links_superseded_successor() {
        let old = Meta {
            num: "0001".to_string(),
            iri: "https://example.test/old".to_string(),
            title: "Old".to_string(),
            status: "superseded".to_string(),
            ts: "2026-06-25T00:00:00Z".to_string(),
            author: "test".to_string(),
            slug: "old".to_string(),
        };
        let new = Meta {
            num: "0002".to_string(),
            iri: "https://example.test/new".to_string(),
            title: "New".to_string(),
            status: "accepted".to_string(),
            ts: "2026-06-26T00:00:00Z".to_string(),
            author: "test".to_string(),
            slug: "new".to_string(),
        };
        let mut clusters = HashMap::new();
        let mut old_cluster = Cluster::default();
        old_cluster.push(Row {
            ad: old.iri.clone(),
            rel: "isSupersededBy".to_string(),
            node: new.iri.clone(),
            nlabel: String::new(),
            ndesc: String::new(),
        });
        clusters.insert(old.iri.clone(), old_cluster);
        clusters.insert(new.iri.clone(), Cluster::default());
        let by_iri = HashMap::from([(old.iri.clone(), old.clone()), (new.iri.clone(), new)]);

        assert_eq!(
            render_status(&old, &clusters, &by_iri),
            "Superseded by [ADR-0002](0002-new.md)"
        );
    }
}
