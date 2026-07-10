//! Deterministic Constraint artifact rendering from the project knowledge graph.
//!
//! The graph remains the source of truth. This module renders a read-only
//! constraints listing/detail view for UI/API callers.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::artifacts::{
    capitalize, date_only, md_cell, nonempty, not_recorded, required_value, select_rows,
};
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

pub const CONSTRAINTS_INDEX_FILENAME: &str = "0000-index.md";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConstraintGenerationOptions {
    pub batch_size: usize,
}

impl Default for ConstraintGenerationOptions {
    fn default() -> Self {
        Self { batch_size: 20 }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ConstraintSet {
    pub generated_at: String,
    pub graph_constraints: usize,
    pub constraint_files: usize,
    pub index_filename: String,
    pub index_markdown: String,
    pub constraints: Vec<ConstraintDocument>,
    pub warnings: ConstraintWarnings,
}

impl ConstraintSet {
    pub fn summaries(&self) -> Vec<ConstraintSummary> {
        self.constraints
            .iter()
            .map(ConstraintDocument::summary)
            .collect()
    }

    pub fn find_by_num(&self, num: &str) -> Option<&ConstraintDocument> {
        self.constraints
            .iter()
            .find(|constraint| constraint.num == num)
    }

    pub fn zip_archive(&self) -> anyhow::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o644);

            writer.start_file(CONSTRAINTS_INDEX_FILENAME, options)?;
            writer.write_all(self.index_markdown.as_bytes())?;
            for constraint in &self.constraints {
                writer.start_file(constraint.filename.as_str(), options)?;
                writer.write_all(constraint.markdown.as_bytes())?;
            }
            writer.finish()?;
        }
        Ok(cursor.into_inner())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ConstraintSummary {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub related_targets: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ConstraintDocument {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub related_targets: usize,
    pub markdown: String,
}

impl ConstraintDocument {
    pub fn summary(&self) -> ConstraintSummary {
        ConstraintSummary {
            num: self.num.clone(),
            title: self.title.clone(),
            status: self.status.clone(),
            date: self.date.clone(),
            author: self.author.clone(),
            iri: self.iri.clone(),
            filename: self.filename.clone(),
            related_targets: self.related_targets,
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct ConstraintWarnings {
    pub missing_description: Vec<String>,
    pub unlinked_constraints: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ConstraintMeta {
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
struct RelatedTarget {
    iri: String,
    title: String,
    status: String,
    ts: String,
}

pub fn generate_constraint_set(
    state: &AppState,
    options: ConstraintGenerationOptions,
) -> anyhow::Result<ConstraintSet> {
    if options.batch_size == 0 {
        anyhow::bail!("Constraint generation batch size must be >= 1");
    }

    let count = count_constraints(state)?;
    let records = enumerate_constraints(state)?;
    let related = fetch_related_targets(state, &records, options.batch_size)?;
    let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    if records.is_empty() {
        return Ok(ConstraintSet {
            generated_at,
            graph_constraints: count,
            constraint_files: 0,
            index_filename: CONSTRAINTS_INDEX_FILENAME.to_string(),
            index_markdown: "# Constraints\n\nNo constraints recorded yet.\n".to_string(),
            constraints: Vec::new(),
            warnings: ConstraintWarnings::default(),
        });
    }

    let constraints = records
        .iter()
        .map(|meta| render_constraint_document(meta, &related))
        .collect();
    let warnings = summarize_warnings(&records, &related);
    let index_markdown = render_index(&records, &related, &generated_at);

    Ok(ConstraintSet {
        generated_at,
        graph_constraints: count,
        constraint_files: records.len(),
        index_filename: CONSTRAINTS_INDEX_FILENAME.to_string(),
        index_markdown,
        constraints,
        warnings,
    })
}

fn count_constraints(state: &AppState) -> anyhow::Result<usize> {
    let constraint_class = state.resolve_class("Constraint")?;
    let query = format!(
        r#"
SELECT (COUNT(?constraint) AS ?n) WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{ ?constraint a <{constraint_class}> . }}
}}
"#
    );
    let rows = select_rows(&state.store, &query)?;
    rows.first()
        .and_then(|row| row.get("n"))
        .unwrap_or(&"0".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("parse constraint count: {e}"))
}

fn enumerate_constraints(state: &AppState) -> anyhow::Result<Vec<ConstraintMeta>> {
    let constraint_class = state.resolve_class("Constraint")?;
    let query = format!(
        r#"
SELECT ?constraint ?title ?status ?ts ?author ?desc WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{
    ?constraint a <{constraint_class}> .
    OPTIONAL {{ ?constraint <{}> ?title }}
    OPTIONAL {{ ?constraint <{}> ?status }}
    OPTIONAL {{ ?constraint <{}> ?ts }}
    OPTIONAL {{ ?constraint <{}> ?author }}
    OPTIONAL {{ ?constraint <{}> ?desc }}
  }}
}} ORDER BY ?ts ?constraint
"#,
        moose::RDFS_LABEL,
        state.capture.status,
        state.capture.timestamp,
        state.capture.author,
        state.capture.description,
    );
    let mut seen_slugs: HashMap<String, usize> = HashMap::new();
    select_rows(&state.store, &query)?
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
            Ok(ConstraintMeta {
                num: format!("{:04}", idx + 1),
                iri: required_value(&row, "constraint")?,
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

fn fetch_related_targets(
    state: &AppState,
    records: &[ConstraintMeta],
    batch_size: usize,
) -> anyhow::Result<HashMap<String, Vec<RelatedTarget>>> {
    let mut related: HashMap<String, Vec<RelatedTarget>> = records
        .iter()
        .map(|record| (record.iri.clone(), Vec::new()))
        .collect();
    let mut seen = HashSet::new();

    for batch in records.chunks(batch_size) {
        let values = batch
            .iter()
            .map(|record| format!("<{}>", record.iri))
            .collect::<Vec<_>>()
            .join(" ");
        let query = related_targets_query(state, &values)?;
        for row in select_rows(&state.store, &query)? {
            let constraint = required_value(&row, "constraint")?;
            let target = required_value(&row, "target")?;
            if !seen.insert((constraint.clone(), target.clone())) {
                continue;
            }
            related.entry(constraint).or_default().push(RelatedTarget {
                iri: target,
                title: row.get("title").cloned().unwrap_or_default(),
                status: row.get("status").cloned().unwrap_or_default(),
                ts: row.get("ts").cloned().unwrap_or_default(),
            });
        }
    }

    Ok(related)
}

fn related_targets_query(state: &AppState, values: &str) -> anyhow::Result<String> {
    let constrains = state.resolve_object_property("constrains")?;
    let constrained_by = state.resolve_object_property("isConstrainedBy")?;
    Ok(format!(
        r#"
SELECT ?constraint ?target ?title ?status ?ts WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{
    VALUES ?constraint {{ {values} }}
    {{ ?constraint <{constrains}> ?target . }}
    UNION
    {{ ?target <{constrained_by}> ?constraint . }}
    OPTIONAL {{ ?target <{}> ?title }}
    OPTIONAL {{ ?target <{}> ?status }}
    OPTIONAL {{ ?target <{}> ?ts }}
  }}
}} ORDER BY ?constraint ?ts ?target
"#,
        moose::RDFS_LABEL,
        state.capture.status,
        state.capture.timestamp,
    ))
}

fn render_constraint_document(
    meta: &ConstraintMeta,
    related: &HashMap<String, Vec<RelatedTarget>>,
) -> ConstraintDocument {
    let targets = related_for(related, meta);
    ConstraintDocument {
        num: meta.num.clone(),
        title: meta.title.clone(),
        status: render_status(&meta.status),
        date: date_only(&meta.ts),
        author: not_recorded(&meta.author),
        iri: meta.iri.clone(),
        filename: filename(meta),
        related_targets: targets.len(),
        markdown: render_constraint(meta, targets),
    }
}

fn render_constraint(meta: &ConstraintMeta, related_targets: &[RelatedTarget]) -> String {
    let mut lines = vec![
        format!("# CST-{}. {}", meta.num, meta.title),
        String::new(),
        format!("- Status: {}", render_status(&meta.status)),
        format!("- Date: {}", date_only(&meta.ts)),
        format!("- Author: {}", not_recorded(&meta.author)),
        String::new(),
        "## Constraint".to_string(),
    ];

    if let Some(desc) = nonempty(&meta.description) {
        lines.push(desc.to_string());
    } else {
        lines.push("No constraint description recorded.".to_string());
    }

    lines.extend([String::new(), "## Constrains".to_string()]);
    if related_targets.is_empty() {
        lines.push("No targets are linked to this constraint.".to_string());
    } else {
        for target in related_targets {
            lines.push(format!(
                "- {} ({}, {}) (`{}`)",
                not_recorded(&target.title),
                render_status(&target.status),
                date_only(&target.ts),
                target.iri
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
    records: &[ConstraintMeta],
    related: &HashMap<String, Vec<RelatedTarget>>,
    generated_at: &str,
) -> String {
    let date = generated_at.split('T').next().unwrap_or(generated_at);
    let mut lines = vec![
        "# Constraints".to_string(),
        String::new(),
        format!("> **Generated view.** Rendered from the MOOSEDev knowledge graph on {date}."),
        "> The graph is the source of truth - **regenerate, do not hand-edit.**".to_string(),
        String::new(),
        "| # | Title | Status | Date | Targets |".to_string(),
        "|---|-------|--------|------|---------|".to_string(),
    ];

    for meta in records {
        lines.push(format!(
            "| CST-{} | [{}]({}) | {} | {} | {} |",
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
    records: &[ConstraintMeta],
    related: &HashMap<String, Vec<RelatedTarget>>,
) -> ConstraintWarnings {
    let mut warnings = ConstraintWarnings::default();
    for meta in records {
        if nonempty(&meta.description).is_none() {
            warnings.missing_description.push(meta.num.clone());
        }
        if related_for(related, meta).is_empty() {
            warnings.unlinked_constraints.push(meta.num.clone());
        }
    }
    warnings
}

fn related_for<'a>(
    related: &'a HashMap<String, Vec<RelatedTarget>>,
    meta: &ConstraintMeta,
) -> &'a [RelatedTarget] {
    related.get(&meta.iri).map(Vec::as_slice).unwrap_or(&[])
}

fn filename(meta: &ConstraintMeta) -> String {
    format!("{}-{}.md", meta.num, meta.slug)
}

fn slugify(title: &str) -> String {
    crate::artifacts::slugify(title, "constraint")
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
    fn slugify_uses_constraint_fallback() {
        assert_eq!(slugify("Constraints page"), "constraints-page");
        assert_eq!(slugify(""), "constraint");
    }
}
