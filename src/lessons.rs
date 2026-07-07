//! Deterministic Lesson artifact rendering from the project knowledge graph.
//!
//! The graph remains the source of truth. This module renders a read-only
//! lessons listing/detail view for UI/API callers.

use std::collections::HashMap;
use std::io::{Cursor, Write};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::artifacts::{
    capitalize, date_only, md_cell, nonempty, not_recorded, required_value, select_rows,
};
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

pub const LESSONS_INDEX_FILENAME: &str = "0000-index.md";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LessonGenerationOptions {
    pub batch_size: usize,
}

impl Default for LessonGenerationOptions {
    fn default() -> Self {
        Self { batch_size: 20 }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct LessonSet {
    pub generated_at: String,
    pub graph_lessons: usize,
    pub lesson_files: usize,
    pub index_filename: String,
    pub index_markdown: String,
    pub lessons: Vec<LessonDocument>,
    pub warnings: LessonWarnings,
}

impl LessonSet {
    pub fn summaries(&self) -> Vec<LessonSummary> {
        self.lessons.iter().map(LessonDocument::summary).collect()
    }

    pub fn find_by_num(&self, num: &str) -> Option<&LessonDocument> {
        self.lessons.iter().find(|lesson| lesson.num == num)
    }

    pub fn zip_archive(&self) -> anyhow::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o644);

            writer.start_file(LESSONS_INDEX_FILENAME, options)?;
            writer.write_all(self.index_markdown.as_bytes())?;
            for lesson in &self.lessons {
                writer.start_file(lesson.filename.as_str(), options)?;
                writer.write_all(lesson.markdown.as_bytes())?;
            }
            writer.finish()?;
        }
        Ok(cursor.into_inner())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct LessonSummary {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub related_sources: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct LessonDocument {
    pub num: String,
    pub title: String,
    pub status: String,
    pub date: String,
    pub author: String,
    pub iri: String,
    pub filename: String,
    pub related_sources: usize,
    pub markdown: String,
}

impl LessonDocument {
    pub fn summary(&self) -> LessonSummary {
        LessonSummary {
            num: self.num.clone(),
            title: self.title.clone(),
            status: self.status.clone(),
            date: self.date.clone(),
            author: self.author.clone(),
            iri: self.iri.clone(),
            filename: self.filename.clone(),
            related_sources: self.related_sources,
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct LessonWarnings {
    pub missing_description: Vec<String>,
    pub unlinked_lessons: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct LessonMeta {
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
struct LessonSource {
    iri: String,
    title: String,
    status: String,
    ts: String,
}

pub fn generate_lesson_set(
    state: &AppState,
    options: LessonGenerationOptions,
) -> anyhow::Result<LessonSet> {
    if options.batch_size == 0 {
        anyhow::bail!("Lesson generation batch size must be >= 1");
    }

    let count = count_lessons(state)?;
    let records = enumerate_lessons(state)?;
    let related = fetch_related_sources(state, &records, options.batch_size)?;
    let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    if records.is_empty() {
        return Ok(LessonSet {
            generated_at,
            graph_lessons: count,
            lesson_files: 0,
            index_filename: LESSONS_INDEX_FILENAME.to_string(),
            index_markdown: "# Lessons\n\nNo lessons recorded yet.\n".to_string(),
            lessons: Vec::new(),
            warnings: LessonWarnings::default(),
        });
    }

    let lessons: Vec<LessonDocument> = records
        .iter()
        .map(|meta| render_lesson_document(meta, &related))
        .collect();
    let warnings = summarize_warnings(&records, &related);
    let index_markdown = render_index(&records, &related, &generated_at);

    Ok(LessonSet {
        generated_at,
        graph_lessons: count,
        lesson_files: lessons.len(),
        index_filename: LESSONS_INDEX_FILENAME.to_string(),
        index_markdown,
        lessons,
        warnings,
    })
}

fn count_lessons(state: &AppState) -> anyhow::Result<usize> {
    let lesson_class = state.resolve_class("Lesson")?;
    let query = format!(
        r#"
SELECT (COUNT(?lesson) AS ?n) WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{ ?lesson a <{lesson_class}> . }}
}}
"#
    );
    let rows = select_rows(&state.store, &query)?;
    rows.first()
        .and_then(|row| row.get("n"))
        .unwrap_or(&"0".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("parse lesson count: {e}"))
}

fn enumerate_lessons(state: &AppState) -> anyhow::Result<Vec<LessonMeta>> {
    let lesson_class = state.resolve_class("Lesson")?;
    let query = format!(
        r#"
SELECT ?lesson ?title ?status ?ts ?author ?desc WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{
    ?lesson a <{lesson_class}> .
    OPTIONAL {{ ?lesson <{}> ?title }}
    OPTIONAL {{ ?lesson <{}> ?status }}
    OPTIONAL {{ ?lesson <{}> ?ts }}
    OPTIONAL {{ ?lesson <{}> ?author }}
    OPTIONAL {{ ?lesson <{}> ?desc }}
  }}
}} ORDER BY ?ts ?lesson
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
            Ok(LessonMeta {
                num: format!("{:04}", idx + 1),
                iri: required_value(&row, "lesson")?,
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

fn fetch_related_sources(
    state: &AppState,
    records: &[LessonMeta],
    batch_size: usize,
) -> anyhow::Result<HashMap<String, Vec<LessonSource>>> {
    let mut related: HashMap<String, Vec<LessonSource>> = records
        .iter()
        .map(|record| (record.iri.clone(), Vec::new()))
        .collect();

    for batch in records.chunks(batch_size) {
        let values = batch
            .iter()
            .map(|record| format!("<{}>", record.iri))
            .collect::<Vec<_>>()
            .join(" ");
        let query = related_sources_query(state, &values)?;
        for row in select_rows(&state.store, &query)? {
            let lesson = required_value(&row, "lesson")?;
            let iri = required_value(&row, "src")?;
            let sources = related.entry(lesson).or_default();
            // Both link directions may record the same pair; keep one entry.
            if sources.iter().any(|source| source.iri == iri) {
                continue;
            }
            sources.push(LessonSource {
                iri,
                title: row.get("title").cloned().unwrap_or_default(),
                status: row.get("status").cloned().unwrap_or_default(),
                ts: row.get("ts").cloned().unwrap_or_default(),
            });
        }
    }

    Ok(related)
}

fn related_sources_query(state: &AppState, values: &str) -> anyhow::Result<String> {
    let learned_from = state.resolve_object_property("learnedFrom")?;
    let yields_lesson = state.resolve_object_property("yieldsLesson")?;
    Ok(format!(
        r#"
SELECT ?lesson ?src ?title ?status ?ts WHERE {{
  GRAPH <{PROJECT_KG_GRAPH_IRI}> {{
    VALUES ?lesson {{ {values} }}
    {{ ?lesson <{learned_from}> ?src }} UNION {{ ?src <{yields_lesson}> ?lesson }}
    OPTIONAL {{ ?src <{}> ?title }}
    OPTIONAL {{ ?src <{}> ?status }}
    OPTIONAL {{ ?src <{}> ?ts }}
  }}
}} ORDER BY ?lesson ?ts ?src
"#,
        moose::RDFS_LABEL,
        state.capture.status,
        state.capture.timestamp,
    ))
}

fn render_lesson_document(
    meta: &LessonMeta,
    related: &HashMap<String, Vec<LessonSource>>,
) -> LessonDocument {
    let sources = related_for(related, meta);
    let markdown = render_lesson(meta, sources);
    LessonDocument {
        num: meta.num.clone(),
        title: meta.title.clone(),
        status: render_status(&meta.status),
        date: date_only(&meta.ts),
        author: not_recorded(&meta.author),
        iri: meta.iri.clone(),
        filename: filename(meta),
        related_sources: sources.len(),
        markdown,
    }
}

fn render_lesson(meta: &LessonMeta, sources: &[LessonSource]) -> String {
    let mut lines = vec![
        format!("# LSN-{}. {}", meta.num, meta.title),
        String::new(),
        format!("- Status: {}", render_status(&meta.status)),
        format!("- Date: {}", date_only(&meta.ts)),
        format!("- Author: {}", not_recorded(&meta.author)),
        String::new(),
        "## Lesson".to_string(),
    ];

    if let Some(desc) = nonempty(&meta.description) {
        lines.push(desc.to_string());
    } else {
        lines.push("No lesson description recorded.".to_string());
    }

    lines.extend([String::new(), "## Learned from".to_string()]);
    if sources.is_empty() {
        lines.push("No source records are linked to this lesson.".to_string());
    } else {
        for source in sources {
            lines.push(format!(
                "- {} ({}, {}) (`{}`)",
                source.title,
                render_status(&source.status),
                date_only(&source.ts),
                source.iri
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
    records: &[LessonMeta],
    related: &HashMap<String, Vec<LessonSource>>,
    generated_at: &str,
) -> String {
    let date = generated_at.split('T').next().unwrap_or(generated_at);
    let mut lines = vec![
        "# Lessons".to_string(),
        String::new(),
        format!("> **Generated view.** Rendered from the MOOSEDev knowledge graph on {date}."),
        "> The graph is the source of truth - **regenerate, do not hand-edit.**".to_string(),
        String::new(),
        "| # | Title | Status | Date | Sources |".to_string(),
        "|---|-------|--------|------|---------|".to_string(),
    ];

    for meta in records {
        let sources = related_for(related, meta);
        lines.push(format!(
            "| LSN-{} | [{}]({}) | {} | {} | {} |",
            meta.num,
            md_cell(&meta.title),
            filename(meta),
            md_cell(&render_status(&meta.status)),
            date_only(&meta.ts),
            sources.len()
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn summarize_warnings(
    records: &[LessonMeta],
    related: &HashMap<String, Vec<LessonSource>>,
) -> LessonWarnings {
    let mut warnings = LessonWarnings::default();
    for meta in records {
        if nonempty(&meta.description).is_none() {
            warnings.missing_description.push(meta.num.clone());
        }
        if related_for(related, meta).is_empty() {
            warnings.unlinked_lessons.push(meta.num.clone());
        }
    }
    warnings
}

fn related_for<'a>(
    related: &'a HashMap<String, Vec<LessonSource>>,
    meta: &LessonMeta,
) -> &'a [LessonSource] {
    related.get(&meta.iri).map(Vec::as_slice).unwrap_or(&[])
}

fn filename(meta: &LessonMeta) -> String {
    format!("{}-{}.md", meta.num, meta.slug)
}

fn slugify(title: &str) -> String {
    crate::artifacts::slugify(title, "lesson")
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
    fn slugify_uses_lesson_fallback() {
        assert_eq!(slugify("Lessons page"), "lessons-page");
        assert_eq!(slugify(""), "lesson");
    }
}
