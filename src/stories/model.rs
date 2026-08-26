//! Stable recipe, run, subject, and quiz data contracts.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// Keeps a topic a focused selector rather than an arbitrary pasted document.
const MAX_TOPIC_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StoryStatus {
    Draft,
    Published,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum StoryIntent {
    Purpose,
    Boundary,
    CoreCode,
    Governance,
    Risk,
}

impl StoryIntent {
    pub(super) fn id(&self) -> &'static str {
        match self {
            Self::Purpose => "purpose",
            Self::Boundary => "boundary",
            Self::CoreCode => "core-code",
            Self::Governance => "governance",
            Self::Risk => "risk",
        }
    }
}

pub(super) fn friendly_record_kind(kind: &str) -> &str {
    match kind {
        "Requirement" => "requirement",
        "ArchitecturalDecision" => "architecture decision",
        "Constraint" => "constraint",
        "Pattern" => "recommended pattern",
        "AntiPattern" => "practice to avoid",
        "Lesson" => "lesson learned",
        "Consequence" => "recorded consequence",
        "Rationale" => "recorded rationale",
        "CodeEntity" => "code entity",
        "SystemComponent" => "system component",
        "InformationRecord" => "project record",
        other => other,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StorySectionKind {
    Orientation,
    Evolution,
    CurrentState,
    Implementation,
    Implications,
}

impl StorySectionKind {
    pub(super) fn id(&self) -> &'static str {
        match self {
            Self::Orientation => "orientation",
            Self::Evolution => "evolution",
            Self::CurrentState => "current-state",
            Self::Implementation => "implementation",
            Self::Implications => "implications",
        }
    }
}

impl From<&StoryIntent> for StorySectionKind {
    fn from(value: &StoryIntent) -> Self {
        match value {
            StoryIntent::Purpose => Self::Orientation,
            StoryIntent::Boundary => Self::CurrentState,
            StoryIntent::CoreCode => Self::Implementation,
            StoryIntent::Governance => Self::Evolution,
            StoryIntent::Risk => Self::Implications,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoryBeatRecipe {
    pub id: String,
    pub title: String,
    pub intent: StoryIntent,
    #[serde(default)]
    pub record_iris: Vec<String>,
    #[serde(default)]
    pub code_symbols: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator_note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoryFocus {
    #[serde(default)]
    pub include_record_iris: Vec<String>,
    #[serde(default)]
    pub exclude_record_iris: Vec<String>,
    #[serde(default)]
    pub include_code_symbols: Vec<String>,
    #[serde(default)]
    pub exclude_code_symbols: Vec<String>,
    #[serde(default)]
    pub emphasis: Vec<StorySectionKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoryRecipe {
    pub id: String,
    pub title: String,
    #[serde(default = "default_story_schema_version")]
    pub schema_version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<StoryRecipeSubject>,
    /// Legacy v1 component subject. Reads remain compatible; normalized writes
    /// always emit `subject` and omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_component_iri: Option<String>,
    pub goal: String,
    pub audience: String,
    #[serde(default)]
    pub focus: StoryFocus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator_context: Option<String>,
    /// Legacy v1/v2 curation outline. It is accepted on read and converted to
    /// v3 focus metadata, but never written again.
    #[serde(default, skip_serializing)]
    pub beats: Vec<StoryBeatRecipe>,
    pub status: StoryStatus,
    pub curator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn default_story_schema_version() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoryRecipeSubject {
    Entity { iri: String },
    Topic { query: String },
}

impl StoryRecipeSubject {
    pub(super) fn identity_key(&self) -> String {
        match self {
            Self::Entity { iri } => format!("entity:{iri}"),
            Self::Topic { query } => format!("topic:{}", normalize_topic(query)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorySummary {
    pub id: String,
    pub title: String,
    pub subject: StoryRecipeSubject,
    pub subject_label: String,
    pub subject_kind: String,
    pub goal: String,
    pub audience: String,
    pub status: StoryStatus,
    pub curator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub drifted: bool,
}

impl From<&StoryRecipe> for StorySummary {
    fn from(recipe: &StoryRecipe) -> Self {
        let subject = recipe
            .resolved_subject()
            .expect("stored Story recipes are validated before summarization")
            .clone();
        Self {
            id: recipe.id.clone(),
            title: recipe.title.clone(),
            subject_label: subject_display_fallback(&subject),
            subject_kind: subject_kind_fallback(&subject).to_string(),
            subject,
            goal: recipe.goal.clone(),
            audience: recipe.audience.clone(),
            status: recipe.status.clone(),
            curator: recipe.curator.clone(),
            updated_at: recipe.updated_at.clone(),
            drifted: false,
        }
    }
}

impl StoryRecipe {
    pub fn resolved_subject(&self) -> anyhow::Result<&StoryRecipeSubject> {
        match (&self.subject, &self.subject_component_iri) {
            (Some(subject), None) => Ok(subject),
            (None, Some(_)) => anyhow::bail!("legacy Story subject was not normalized"),
            (Some(_), Some(_)) => anyhow::bail!("Story recipe contains two subject selectors"),
            (None, None) => anyhow::bail!("Story recipe requires a subject"),
        }
    }

    pub fn subject_identity_key(&self) -> anyhow::Result<String> {
        Ok(self.resolved_subject()?.identity_key())
    }
}

pub(super) fn normalize_topic(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(super) fn validate_topic(query: &str) -> anyhow::Result<()> {
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() < 2 {
        anyhow::bail!("Story topic must contain at least two characters");
    }
    if normalized.chars().count() > MAX_TOPIC_CHARS {
        anyhow::bail!("Story topic may contain at most {MAX_TOPIC_CHARS} characters");
    }
    Ok(())
}

pub(super) fn subject_display_fallback(subject: &StoryRecipeSubject) -> String {
    match subject {
        StoryRecipeSubject::Entity { iri } => iri.clone(),
        StoryRecipeSubject::Topic { query } => query.clone(),
    }
}

pub(super) fn subject_kind_fallback(subject: &StoryRecipeSubject) -> &'static str {
    match subject {
        StoryRecipeSubject::Entity { .. } => "Entity",
        StoryRecipeSubject::Topic { .. } => "Topic",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoryTrustState {
    Generated,
    Draft,
    Published,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NarrationMode {
    Symbolic,
    Llm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NarrationStrategy {
    Symbolic,
    SinglePass,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NarrationOutcome {
    NotRequested,
    Succeeded,
    Unconfigured,
    Ineligible,
    Timeout,
    ProviderError,
    InvalidResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NarrationFailureReason {
    PacketTooLarge,
    InvalidJson,
    SchemaMismatch,
    CitationMismatch,
    StructuredOutputUnsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryNarrationCoverage {
    pub eligible_entities: usize,
    pub included_entities: usize,
    pub source_groups: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StorySubject {
    Entity {
        iri: String,
        kind: String,
        label: String,
    },
    Topic {
        query: String,
        label: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryEvidence {
    pub iri: String,
    pub title: String,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoryRelationDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryEvidenceRelation {
    pub predicate: String,
    pub label: String,
    pub direction: StoryRelationDirection,
    pub target_iri: String,
    pub target_label: String,
    pub target_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct StoryLiteralProperty {
    pub predicate: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryEvidenceDetail {
    pub iri: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    /// Curator-suppressed evidence is retained only so lifecycle chronology
    /// does not become misleading. It is excluded from narrative prompts.
    #[serde(default)]
    pub suppressed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Complete literal projection for the entity, including type-specific
    /// fields not known to the Story layer.
    #[serde(default)]
    pub properties: Vec<StoryLiteralProperty>,
    #[serde(default)]
    pub relations: Vec<StoryEvidenceRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryCodeAnchor {
    pub symbol: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_iri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryBeat {
    pub id: String,
    pub title: String,
    pub intent: StoryIntent,
    pub narrative: String,
    pub evidence: Vec<StoryEvidence>,
    pub code_anchors: Vec<StoryCodeAnchor>,
    /// Human-authored presentation guidance. It stays separate from both
    /// authoritative evidence and LLM-authored narrative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryParagraph {
    pub text: String,
    #[serde(default)]
    pub citation_iris: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryNarrativeSection {
    pub id: String,
    pub kind: StorySectionKind,
    pub title: String,
    pub paragraphs: Vec<StoryParagraph>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryTimelineEvent {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub evidence_iri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    #[serde(default)]
    pub predecessor_iris: Vec<String>,
    #[serde(default)]
    pub successor_iris: Vec<String>,
    #[serde(default)]
    pub rationale_iris: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryCoverage {
    pub entity_count: usize,
    pub dossier_bytes: usize,
    pub current_count: usize,
    pub historical_count: usize,
    pub proposed_count: usize,
    pub code_anchor_count: usize,
    #[serde(default)]
    pub subject_families: Vec<String>,
    #[serde(default)]
    pub outline_sections: Vec<StorySectionKind>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryGap {
    pub id: String,
    pub title: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_kind: Option<StorySectionKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryCheckOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryCheck {
    pub id: String,
    pub question: String,
    pub options: Vec<StoryCheckOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryRun {
    pub schema_version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_id: Option<String>,
    pub trust_state: StoryTrustState,
    pub narration_mode: NarrationMode,
    pub narration_strategy: NarrationStrategy,
    pub narration_outcome: NarrationOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration_failure_reason: Option<NarrationFailureReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration_coverage: Option<StoryNarrationCoverage>,
    pub title: String,
    pub subject: StorySubject,
    pub goal: String,
    /// Verbatim human guidance, visibly separate from evidence and narration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator_context: Option<String>,
    pub brief: StoryParagraph,
    pub narrative: Vec<StoryNarrativeSection>,
    pub timeline: Vec<StoryTimelineEvent>,
    pub evidence: Vec<StoryEvidenceDetail>,
    pub code_anchors: Vec<StoryCodeAnchor>,
    pub coverage: StoryCoverage,
    pub gaps: Vec<StoryGap>,
    pub checks: Vec<StoryCheck>,
    /// Internal deterministic planning and quiz scaffolding; never serialized.
    #[serde(skip)]
    pub(super) beats: Vec<StoryBeat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryCandidate {
    pub iri: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// True when the project graph records nothing about this subject beyond
    /// its own existence, so its Story closes over the subject and the
    /// component it realizes and nothing else. Only ever set for CodeEntity
    /// subjects: they are the only kind the indexer mints in bulk.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_recorded_knowledge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GradeResult {
    pub correct: bool,
    pub feedback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revisit_section_id: Option<String>,
    pub evidence_iris: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CheckKind {
    Concerns,
    Realizes,
    RecordKind,
    /// Which record replaced this one — the "why we moved" probe.
    Supersedes,
    /// Which approach a decision rejected.
    Weighs,
}

#[derive(Clone)]
pub(super) struct CheckGrant {
    pub(super) kind: CheckKind,
    /// The relationship's OTHER endpoint, whose meaning follows `kind`: a
    /// SystemComponent for `Concerns`/`Realizes`, the superseded record for
    /// `Supersedes`, the deciding record for `Weighs`, unused for `RecordKind`.
    pub(super) counterpart_iri: String,
    pub(super) section_id: String,
    pub(super) correct_option_token: String,
    pub(super) option_entities: BTreeMap<String, String>,
    pub(super) correct_entity_iri: String,
    pub(super) correct_kind: Option<String>,
    pub(super) subject_entity: Option<(String, String)>,
    pub(super) expires_at: std::time::Instant,
}

#[derive(Default)]
pub struct StoryCheckRegistry {
    pub(super) grants: HashMap<String, CheckGrant>,
    pub(super) retired: VecDeque<(String, RetiredCheckKind)>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum RetiredCheckKind {
    Expired,
    Evicted,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoryCheckError {
    Malformed,
    Unknown,
    Expired,
    Evicted,
    ForeignOption,
    Stale,
}

impl std::fmt::Display for StoryCheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "malformed Story check handle",
            Self::Unknown => "unknown Story check handle",
            Self::Expired => "Story check handle expired; generate the Story again",
            Self::Evicted => "Story check is no longer available; generate the Story again",
            Self::ForeignOption => "selected option does not belong to this Story check",
            Self::Stale => "Story check evidence changed; generate the Story again",
        })
    }
}

impl std::error::Error for StoryCheckError {}

#[derive(Debug)]
pub enum ResolveOutcome {
    Resolved(StoryCandidate),
    Ambiguous(Vec<StoryCandidate>),
}
