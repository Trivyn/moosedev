//! Deterministic Requirement artifact rendering from the project knowledge graph.
//!
//! The graph remains the source of truth. This module renders a read-only
//! requirements listing/detail view for UI/API callers.

use std::collections::HashMap;
use std::io::{Cursor, Write};

use chrono::{SecondsFormat, Utc};
use oxigraph::store::Store;
use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::artifacts::{
    capitalize, date_only, md_cell, nonempty, not_recorded, required_value, select_rows,
};

pub const REQUIREMENTS_INDEX_FILENAME: &str = "0000-index.md";

const COUNT_QUERY: &str = r#"
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
SELECT (COUNT(?req) AS ?n) WHERE {
  GRAPH <https://moosedev.dev/kg/project> { ?req a :Requirement . }
}
"#;

const ENUM_QUERY: &str = r#"
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?req ?title ?status ?ts ?author ?desc WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    ?req a :Requirement ;
         rdfs:label ?title ;
         :hasLifecycleStatus ?status ;
         :hasTimestamp ?ts .
    OPTIONAL { ?req :hasAuthor ?author }
    OPTIONAL { ?req :hasDescription ?desc }
  }
} ORDER BY ?ts ?req
"#;

const RELATED_ADRS_QUERY_TEMPLATE: &str = r#"
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?req ?ad ?title ?status ?ts WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    VALUES ?req { __VALUES__ }
    ?ad a :ArchitecturalDecision ;
        :isMotivatedBy ?req ;
        rdfs:label ?title ;
        :hasLifecycleStatus ?status ;
        :hasTimestamp ?ts .
  }
} ORDER BY ?req ?ts ?ad
"#;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RequirementGenerationOptions {
    pub batch_size: usize,
}

impl Default for RequirementGenerationOptions {
    fn default() -> Self {
        Self { batch_size: 20 }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RequirementSet {
    pub generated_at: String,
    pub graph_requirements: usize,
    pub requirement_files: usize,
    pub index_filename: String,
    pub index_markdown: String,
    pub requirements: Vec<RequirementDocument>,
    pub warnings: RequirementWarnings,
}

impl RequirementSet {
    pub fn summaries(&self) -> Vec<RequirementSummary> {
        self.requirements
            .iter()
            .map(RequirementDocument::summary)
            .collect()
    }

    pub fn find_by_num(&self, num: &str) -> Option<&RequirementDocument> {
        self.requirements.iter().find(|req| req.num == num)
    }

    pub fn zip_archive(&self) -> anyhow::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o644);

            writer.start_file(REQUIREMENTS_INDEX_FILENAME, options)?;
            writer.write_all(self.index_markdown.as_bytes())?;
            for req in &self.requirements {
                writer.start_file(req.filename.as_str(), options)?;
                writer.write_all(req.markdown.as_bytes())?;
            }
            writer.finish()?;
        }
        Ok(cursor.into_inner())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RequirementSummary {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub related_adrs: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RequirementDocument {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub related_adrs: usize,
    pub markdown: String,
}

impl RequirementDocument {
    pub fn summary(&self) -> RequirementSummary {
        RequirementSummary {
            num: self.num.clone(),
            title: self.title.clone(),
            status: self.status.clone(),
            date: self.date.clone(),
            author: self.author.clone(),
            iri: self.iri.clone(),
            filename: self.filename.clone(),
            related_adrs: self.related_adrs,
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct RequirementWarnings {
    pub missing_description: Vec<String>,
    pub unlinked_requirements: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RequirementMeta {
    num: String,
    iri: String,
    title: String,
    status: String,
    ts: String,
    author: String,
    description: String,
    slug: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RelatedAdr {
    iri: String,
    title: String,
    status: String,
    ts: String,
}

pub fn generate_requirement_set(
    store: &Store,
    options: RequirementGenerationOptions,
) -> anyhow::Result<RequirementSet> {
    if options.batch_size == 0 {
        anyhow::bail!("Requirement generation batch size must be >= 1");
    }

    let count = count_requirements(store)?;
    let records = enumerate_requirements(store)?;
    let related = fetch_related_adrs(store, &records, options.batch_size)?;
    let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    if records.is_empty() {
        return Ok(RequirementSet {
            generated_at,
            graph_requirements: count,
            requirement_files: 0,
            index_filename: REQUIREMENTS_INDEX_FILENAME.to_string(),
            index_markdown: "# Requirements\n\nNo requirements recorded yet.\n".to_string(),
            requirements: Vec::new(),
            warnings: RequirementWarnings::default(),
        });
    }

    let requirements: Vec<RequirementDocument> = records
        .iter()
        .map(|meta| render_requirement_document(meta, &related))
        .collect();
    let warnings = summarize_warnings(&records, &related);
    let index_markdown = render_index(&records, &related, &generated_at);

    Ok(RequirementSet {
        generated_at,
        graph_requirements: count,
        requirement_files: requirements.len(),
        index_filename: REQUIREMENTS_INDEX_FILENAME.to_string(),
        index_markdown,
        requirements,
        warnings,
    })
}

fn count_requirements(store: &Store) -> anyhow::Result<usize> {
    let rows = select_rows(store, COUNT_QUERY)?;
    rows.first()
        .and_then(|row| row.get("n"))
        .unwrap_or(&"0".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("parse requirement count: {e}"))
}

fn enumerate_requirements(store: &Store) -> anyhow::Result<Vec<RequirementMeta>> {
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
            Ok(RequirementMeta {
                num: format!("{:04}", idx + 1),
                iri: required_value(&row, "req")?,
                title,
                status: row.get("status").cloned().unwrap_or_default(),
                ts: row.get("ts").cloned().unwrap_or_default(),
                author: row.get("author").cloned().unwrap_or_default(),
                description: row.get("desc").cloned().unwrap_or_default(),
                slug,
            })
        })
        .collect()
}

fn fetch_related_adrs(
    store: &Store,
    records: &[RequirementMeta],
    batch_size: usize,
) -> anyhow::Result<HashMap<String, Vec<RelatedAdr>>> {
    let mut related: HashMap<String, Vec<RelatedAdr>> = records
        .iter()
        .map(|record| (record.iri.clone(), Vec::new()))
        .collect();

    for batch in records.chunks(batch_size) {
        let values = batch
            .iter()
            .map(|record| format!("<{}>", record.iri))
            .collect::<Vec<_>>()
            .join(" ");
        let query = RELATED_ADRS_QUERY_TEMPLATE.replace("__VALUES__", &values);
        for row in select_rows(store, &query)? {
            let req = required_value(&row, "req")?;
            related.entry(req).or_default().push(RelatedAdr {
                iri: required_value(&row, "ad")?,
                title: row.get("title").cloned().unwrap_or_default(),
                status: row.get("status").cloned().unwrap_or_default(),
                ts: row.get("ts").cloned().unwrap_or_default(),
            });
        }
    }

    Ok(related)
}

fn render_requirement_document(
    meta: &RequirementMeta,
    related: &HashMap<String, Vec<RelatedAdr>>,
) -> RequirementDocument {
    let adrs = related_for(related, meta);
    let markdown = render_requirement(meta, adrs);
    RequirementDocument {
        num: meta.num.clone(),
        title: meta.title.clone(),
        status: render_status(&meta.status),
        date: date_only(&meta.ts),
        author: not_recorded(&meta.author),
        iri: meta.iri.clone(),
        filename: filename(meta),
        related_adrs: adrs.len(),
        markdown,
    }
}

fn render_requirement(meta: &RequirementMeta, related_adrs: &[RelatedAdr]) -> String {
    let mut lines = vec![
        format!("# REQ-{}. {}", meta.num, meta.title),
        String::new(),
        format!("- Status: {}", render_status(&meta.status)),
        format!("- Date: {}", date_only(&meta.ts)),
        format!("- Author: {}", not_recorded(&meta.author)),
        String::new(),
        "## Requirement".to_string(),
    ];

    if let Some(desc) = nonempty(&meta.description) {
        lines.push(desc.to_string());
    } else {
        lines.push("No requirement description recorded.".to_string());
    }

    lines.extend([String::new(), "## Related ADRs".to_string()]);
    if related_adrs.is_empty() {
        lines.push("No architectural decisions are linked to this requirement.".to_string());
    } else {
        for adr in related_adrs {
            lines.push(format!(
                "- {} ({}, {}) (`{}`)",
                adr.title,
                render_status(&adr.status),
                date_only(&adr.ts),
                adr.iri
            ));
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
    records: &[RequirementMeta],
    related: &HashMap<String, Vec<RelatedAdr>>,
    generated_at: &str,
) -> String {
    let date = generated_at.split('T').next().unwrap_or(generated_at);
    let mut lines = vec![
        "# Requirements".to_string(),
        String::new(),
        format!("> **Generated view.** Rendered from the MOOSEDev knowledge graph on {date}."),
        "> The graph is the source of truth - **regenerate, do not hand-edit.**".to_string(),
        String::new(),
        "| # | Title | Status | Date | ADRs |".to_string(),
        "|---|-------|--------|------|------|".to_string(),
    ];

    for meta in records {
        lines.push(format!(
            "| REQ-{} | [{}]({}) | {} | {} | {} |",
            meta.num,
            md_cell(&meta.title),
            filename(meta),
            md_cell(&render_status(&meta.status)),
            date_only(&meta.ts),
            related_for(related, meta).len()
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn summarize_warnings(
    records: &[RequirementMeta],
    related: &HashMap<String, Vec<RelatedAdr>>,
) -> RequirementWarnings {
    let mut warnings = RequirementWarnings::default();
    for meta in records {
        if nonempty(&meta.description).is_none() {
            warnings.missing_description.push(meta.num.clone());
        }
        if related_for(related, meta).is_empty() {
            warnings.unlinked_requirements.push(meta.num.clone());
        }
    }
    warnings
}

fn related_for<'a>(
    related: &'a HashMap<String, Vec<RelatedAdr>>,
    meta: &RequirementMeta,
) -> &'a [RelatedAdr] {
    related.get(&meta.iri).map(Vec::as_slice).unwrap_or(&[])
}

fn filename(meta: &RequirementMeta) -> String {
    format!("{}-{}.md", meta.num, meta.slug)
}

fn slugify(title: &str) -> String {
    crate::artifacts::slugify(title, "requirement")
}

fn render_status(status: &str) -> String {
    match status.to_ascii_lowercase().as_str() {
        "accepted" => "Accepted".to_string(),
        "proposed" => "Proposed".to_string(),
        "deprecated" => "Deprecated".to_string(),
        "superseded" => "Superseded".to_string(),
        "" => "not recorded".to_string(),
        other => capitalize(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_uses_requirement_fallback() {
        assert_eq!(slugify("Requirements page"), "requirements-page");
        assert_eq!(slugify(""), "requirement");
    }
}
