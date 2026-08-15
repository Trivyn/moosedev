//! Stable recipe, run, subject, and quiz data contracts.

use super::*;

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
    pub beats: Vec<StoryBeatRecipe>,
    pub status: StoryStatus,
    pub curator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn default_story_schema_version() -> u8 {
    STORY_SCHEMA_VERSION
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
    pub beat_count: usize,
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
            beat_count: recipe.beats.len(),
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
pub struct StoryGap {
    pub id: String,
    pub title: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat_intent: Option<StoryIntent>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_id: Option<String>,
    pub trust_state: StoryTrustState,
    pub narration_mode: NarrationMode,
    pub narration_outcome: NarrationOutcome,
    pub title: String,
    pub subject: StorySubject,
    pub goal: String,
    pub overview: String,
    pub beats: Vec<StoryBeat>,
    pub gaps: Vec<StoryGap>,
    pub checks: Vec<StoryCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoryCandidate {
    pub iri: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GradeResult {
    pub correct: bool,
    pub feedback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revisit_beat_id: Option<String>,
    pub evidence_iris: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CheckKind {
    Concerns,
    Realizes,
    RecordKind,
}

#[derive(Clone)]
pub(super) struct CheckGrant {
    pub(super) kind: CheckKind,
    pub(super) component_iri: String,
    pub(super) beat_id: String,
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
