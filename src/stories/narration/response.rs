use super::super::model::{StoryNarrativeSection, StoryParagraph, StoryRun};
use super::{NarrationFailure, NarrationFailureReason};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const MAX_LLM_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_NARRATED_PARAGRAPH_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarrationResponse {
    paragraphs: Vec<NarratedParagraph>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarratedParagraph {
    section_id: String,
    text: String,
    source_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarratedSection {
    section_id: String,
    #[serde(default)]
    paragraph: Option<String>,
    #[serde(default)]
    paragraphs: Option<Vec<String>>,
    source_ids: Vec<String>,
    #[serde(default, rename = "title")]
    _title: Option<String>,
}

pub(super) enum ValidationFailure {
    InvalidJson,
    SchemaMismatch(&'static str),
    CitationMismatch,
}

impl ValidationFailure {
    pub(super) fn into_narration_failure(self) -> NarrationFailure {
        match self {
            Self::InvalidJson => {
                NarrationFailure::invalid(NarrationFailureReason::InvalidJson, "invalid_json")
            }
            Self::SchemaMismatch(detail) => {
                NarrationFailure::invalid(NarrationFailureReason::SchemaMismatch, detail)
            }
            Self::CitationMismatch => NarrationFailure::invalid(
                NarrationFailureReason::CitationMismatch,
                "citation_mismatch",
            ),
        }
    }
}

pub(super) fn validate_packet_response(
    run: &StoryRun,
    raw: &str,
    citations_by_source: &BTreeMap<String, Vec<String>>,
    sections_by_source: &BTreeMap<String, String>,
) -> Result<Vec<StoryNarrativeSection>, ValidationFailure> {
    let values = parse_json_values(raw)?;
    let response = values.iter().find_map(parse_narration_response);
    let Some(response) = response else {
        let shapes = values.iter().map(json_shape).collect::<Vec<_>>();
        tracing::warn!(?shapes, "Story narration response shape was rejected");
        return Err(ValidationFailure::SchemaMismatch("response_shape"));
    };
    if response.paragraphs.is_empty() {
        return Err(ValidationFailure::SchemaMismatch("empty_paragraphs"));
    }
    let expected_sections = run
        .narrative
        .iter()
        .map(|section| section.id.as_str())
        .collect::<Vec<_>>();
    let actual_sections = response
        .paragraphs
        .iter()
        .map(|paragraph| paragraph.section_id.as_str())
        .fold(Vec::<&str>::new(), |mut sections, section| {
            if sections.last().copied() != Some(section) {
                sections.push(section);
            }
            sections
        });
    if actual_sections != expected_sections {
        return Err(ValidationFailure::SchemaMismatch("section_order"));
    }
    let allowed = citations_by_source
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut cited_sources = BTreeSet::<String>::new();
    let mut paragraphs_by_section = BTreeMap::<String, Vec<StoryParagraph>>::new();
    for paragraph in response.paragraphs {
        if paragraph.text.trim().is_empty() {
            return Err(ValidationFailure::SchemaMismatch("empty_text"));
        }
        if paragraph.text.len() > MAX_NARRATED_PARAGRAPH_BYTES {
            return Err(ValidationFailure::SchemaMismatch("paragraph_too_large"));
        }
        if paragraph.source_ids.is_empty() {
            return Err(ValidationFailure::SchemaMismatch("empty_source_ids"));
        }
        if paragraph
            .source_ids
            .iter()
            .any(|source| !allowed.contains(source.as_str()))
        {
            return Err(ValidationFailure::SchemaMismatch("unknown_source"));
        }
        if paragraph.source_ids.iter().any(|source| {
            sections_by_source.get(source).map(String::as_str)
                != Some(paragraph.section_id.as_str())
        }) {
            return Err(ValidationFailure::SchemaMismatch("cross_section_source"));
        }
        if contains_private_source_marker(&paragraph.text, &allowed) {
            return Err(ValidationFailure::SchemaMismatch("private_source_marker"));
        }
        cited_sources.extend(paragraph.source_ids.iter().cloned());
        let mut citation_iris = paragraph
            .source_ids
            .iter()
            .flat_map(|source| {
                citations_by_source
                    .get(source)
                    .into_iter()
                    .flatten()
                    .cloned()
            })
            .collect::<Vec<_>>();
        citation_iris.sort();
        citation_iris.dedup();
        paragraphs_by_section
            .entry(paragraph.section_id)
            .or_default()
            .push(StoryParagraph {
                text: paragraph.text,
                citation_iris,
            });
    }
    let expected_sources = citations_by_source.keys().cloned().collect::<BTreeSet<_>>();
    if cited_sources != expected_sources {
        return Err(ValidationFailure::CitationMismatch);
    }
    let expanded = paragraphs_by_section
        .values()
        .flatten()
        .flat_map(|paragraph| paragraph.citation_iris.iter().cloned())
        .collect::<BTreeSet<_>>();
    let expected_iris = citations_by_source
        .values()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    if expanded != expected_iris {
        return Err(ValidationFailure::CitationMismatch);
    }
    // Sources expand back to PUBLIC evidence IRIs only. compact_narration_evidence
    // mints private packet sources from code anchors the bounded dossier omitted;
    // they legitimately ground the prose, but they are absent from run.evidence, so
    // citing them ships a reference the reader cannot resolve to anything.
    let public_iris = run
        .evidence
        .iter()
        .map(|item| item.iri.as_str())
        .collect::<BTreeSet<_>>();
    for paragraphs in paragraphs_by_section.values_mut() {
        for paragraph in paragraphs {
            paragraph
                .citation_iris
                .retain(|iri| public_iris.contains(iri.as_str()));
        }
    }
    run.narrative
        .iter()
        .map(|section| {
            Ok(StoryNarrativeSection {
                id: section.id.clone(),
                kind: section.kind.clone(),
                title: section.title.clone(),
                paragraphs: paragraphs_by_section
                    .remove(&section.id)
                    .ok_or(ValidationFailure::SchemaMismatch("missing_section"))?,
            })
        })
        .collect()
}

fn parse_narration_response(value: &serde_json::Value) -> Option<NarrationResponse> {
    // OpenAI-compatible providers emit both shapes for the same requested
    // contract, so normalize them before applying identical grounding checks.
    serde_json::from_value::<NarrationResponse>(value.clone())
        .ok()
        .or_else(|| {
            serde_json::from_value::<Vec<NarratedSection>>(value.clone())
                .ok()
                .and_then(|sections| {
                    let mut paragraphs = Vec::new();
                    for section in sections {
                        let texts = match (section.paragraph, section.paragraphs) {
                            (Some(text), None) => vec![text],
                            (None, Some(texts)) if !texts.is_empty() => texts,
                            _ => return None,
                        };
                        for text in texts {
                            paragraphs.push(NarratedParagraph {
                                section_id: section.section_id.clone(),
                                text,
                                source_ids: section.source_ids.clone(),
                            });
                        }
                    }
                    Some(NarrationResponse { paragraphs })
                })
        })
}

fn json_shape(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(object) => {
            let keys = object.keys().cloned().collect::<Vec<_>>().join(",");
            let paragraphs = object.get("paragraphs").map_or_else(
                || "absent".to_string(),
                |value| match value {
                    serde_json::Value::Array(items) => {
                        let item_shapes = items
                            .iter()
                            .map(|item| match item {
                                serde_json::Value::Object(fields) => {
                                    fields.keys().cloned().collect::<Vec<_>>().join("+")
                                }
                                other => json_type(other).to_string(),
                            })
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>()
                            .join("|");
                        format!("array(len={},items={item_shapes})", items.len())
                    }
                    other => json_type(other).to_string(),
                },
            );
            format!("object(keys={keys},paragraphs={paragraphs})")
        }
        other => json_type(other).to_string(),
    }
}

fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn contains_private_source_marker(text: &str, source_ids: &BTreeSet<&str>) -> bool {
    source_ids.iter().any(|source| {
        text.contains(&format!("[{source}]")) || text.contains(&format!("({source})"))
    })
}

fn parse_json_values(raw: &str) -> Result<Vec<serde_json::Value>, ValidationFailure> {
    if raw.len() > MAX_LLM_RESPONSE_BYTES {
        return Err(ValidationFailure::InvalidJson);
    }
    let trimmed = raw.trim();
    let mut values = Vec::new();
    if let Some(value) = parse_json_candidate(trimmed) {
        values.push(value);
    }
    if let Some(extracted) = extract_json_object(trimmed) {
        if extracted != trimmed {
            if let Some(value) = parse_json_candidate(extracted) {
                values.push(value);
            }
        }
    }
    (!values.is_empty())
        .then_some(values)
        .ok_or(ValidationFailure::InvalidJson)
}

fn parse_json_candidate(candidate: &str) -> Option<serde_json::Value> {
    // Repair provider syntax only; schema and citation validation still run on
    // the repaired value before any prose is accepted.
    serde_json::from_str(candidate).ok().or_else(|| {
        let repaired = jsonrepair::repair_json(candidate, &jsonrepair::Options::default()).ok()?;
        serde_json::from_str(&repaired).ok()
    })
}

/// Return the first complete JSON object while ignoring prose, Markdown fences,
/// and trailing model chatter. Braces inside JSON strings do not affect depth.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in raw.as_bytes()[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return raw.get(start..=start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
pub(in crate::stories) fn apply_packet_response_for_test(
    run: &StoryRun,
    raw: &str,
    citations_by_source: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<StoryNarrativeSection>, NarrationFailureReason> {
    let sections_by_source = citations_by_source
        .keys()
        .zip(run.narrative.iter().map(|section| &section.id))
        .map(|(source, section)| (source.clone(), section.clone()))
        .collect::<BTreeMap<_, _>>();
    validate_packet_response(run, raw, citations_by_source, &sections_by_source).map_err(
        |failure| match failure {
            ValidationFailure::InvalidJson => NarrationFailureReason::InvalidJson,
            ValidationFailure::SchemaMismatch(_) => NarrationFailureReason::SchemaMismatch,
            ValidationFailure::CitationMismatch => NarrationFailureReason::CitationMismatch,
        },
    )
}
