//! Human-readable subsystem Stories projected from authoritative project knowledge.
//!
//! Recipes are version-controlled presentation metadata under `stories/`. Story
//! runs are read-only projections: this module never writes the project graph.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use moose::traits::LlmClient;
use moose::types::{LlmAssistLevel, LlmParams};
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNode, Term};
use serde::{Deserialize, Serialize};

use crate::graph::{
    asserted_project_types, first_literal, in_working_set, load_components, local_name,
    resolve_component_query, AppState, CodeTerms, ComponentEntry, PROJECT_KG_GRAPH_IRI,
};

const MIN_PUBLISHED_BEATS: usize = 3;
const MAX_BEATS: usize = 5;
const MAX_ANCHORS_PER_BEAT: usize = 6;
const MAX_CHECK_GRANTS: usize = 1_024;
const MAX_RETIRED_CHECK_HANDLES: usize = 1_024;
const MAX_CHECK_OPTIONS: usize = 3;
const MAX_LLM_FIELD_BYTES: usize = 512;
const MAX_LLM_PROMPT_BYTES: usize = 32 * 1024;
const CHECK_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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
    fn id(&self) -> &'static str {
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
    pub subject_component_iri: String,
    pub goal: String,
    pub audience: String,
    pub beats: Vec<StoryBeatRecipe>,
    pub status: StoryStatus,
    pub curator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorySummary {
    pub id: String,
    pub title: String,
    pub subject_component_iri: String,
    pub subject_label: String,
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
        Self {
            id: recipe.id.clone(),
            title: recipe.title.clone(),
            subject_component_iri: recipe.subject_component_iri.clone(),
            subject_label: recipe.subject_component_iri.clone(),
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
pub struct StorySubject {
    pub iri: String,
    pub label: String,
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
enum CheckKind {
    Concerns,
    Realizes,
}

#[derive(Clone)]
struct CheckGrant {
    kind: CheckKind,
    component_iri: String,
    beat_id: String,
    correct_option_token: String,
    option_entities: BTreeMap<String, String>,
    correct_entity_iri: String,
    expires_at: std::time::Instant,
}

#[derive(Default)]
pub struct StoryCheckRegistry {
    grants: HashMap<String, CheckGrant>,
    retired: VecDeque<(String, RetiredCheckKind)>,
}

#[derive(Debug, Clone, Copy)]
enum RetiredCheckKind {
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

/// File-backed recipe repository. Claims stay in the KG; recipes contain only
/// presentation metadata and stable references.
#[derive(Debug, Clone)]
pub struct StoryRepository {
    project_root: PathBuf,
    root: PathBuf,
}

static STORY_WRITER: OnceLock<Mutex<()>> = OnceLock::new();
static LAST_STORY_REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct StoryConflict(pub String);

#[derive(Debug)]
pub struct StoryNotFound(pub String);

#[derive(Debug)]
pub struct StoryCorrupt(pub String);

#[derive(Debug)]
pub struct StoryInternal(pub String);

impl std::fmt::Display for StoryConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StoryConflict {}

macro_rules! impl_story_error {
    ($kind:ident) => {
        impl std::fmt::Display for $kind {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::error::Error for $kind {}
    };
}

impl_story_error!(StoryNotFound);
impl_story_error!(StoryCorrupt);
impl_story_error!(StoryInternal);

impl StoryRepository {
    pub fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            root: project_root.join("stories"),
        }
    }

    pub fn list_recipes(&self) -> anyhow::Result<Vec<StoryRecipe>> {
        if !self.safe_root_exists()? {
            return Ok(Vec::new());
        }
        let mut recipes = Vec::new();
        for entry in std::fs::read_dir(&self.root)
            .with_context(|| format!("read story directory {}", self.root.display()))?
        {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_symlink() {
                tracing::warn!("quarantining symlinked Story recipe {}", path.display());
                continue;
            }
            let Some(file_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                tracing::warn!(
                    "ignoring Story recipe with non-UTF-8 filename: {}",
                    path.display()
                );
                continue;
            };
            match self.read_path(&path, Some(file_id)) {
                Ok(recipe) => recipes.push(recipe),
                Err(error) => tracing::warn!(
                    "quarantining invalid Story recipe {}: {error}",
                    path.display()
                ),
            }
        }
        recipes.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        Ok(recipes)
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<StoryRecipe>> {
        validate_id(id)?;
        let path = self.path(id);
        if !self.safe_root_exists()? || !self.safe_recipe_exists(&path)? {
            return Ok(None);
        }
        self.read_path(&path, Some(id)).map(Some)
    }

    /// Compare-and-swap one recipe under the process-wide Story writer lock.
    /// `updated_at` is the version token: create requires `None`; update requires
    /// the exact token returned by the last read/write.
    pub fn save(&self, route_id: &str, recipe: StoryRecipe) -> anyhow::Result<StoryRecipe> {
        self.save_checked(route_id, recipe, |_| Ok(()))
    }

    /// Save after checking graph-dependent invariants inside the same critical
    /// section as the version comparison and atomic replace.
    pub fn save_checked<F>(
        &self,
        route_id: &str,
        recipe: StoryRecipe,
        validate_current: F,
    ) -> anyhow::Result<StoryRecipe>
    where
        F: FnOnce(&StoryRecipe) -> anyhow::Result<()>,
    {
        validate_id(route_id)?;
        let recipe = normalize_recipe(recipe);
        if recipe.id != route_id {
            anyhow::bail!("recipe id must match route id");
        }
        validate_recipe(&recipe, recipe.status == StoryStatus::Published)?;
        let _writer = STORY_WRITER
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = self.get(route_id)?;
        require_current_version(route_id, current.as_ref(), recipe.updated_at.as_deref())?;
        validate_current(&recipe)?;
        if recipe.status == StoryStatus::Published {
            self.ensure_unique_published_subject(route_id, &recipe.subject_component_iri)?;
        }
        self.write_recipe(
            route_id,
            recipe,
            current
                .as_ref()
                .and_then(|current| current.updated_at.as_deref()),
        )
    }

    fn write_recipe(
        &self,
        route_id: &str,
        mut recipe: StoryRecipe,
        previous_revision: Option<&str>,
    ) -> anyhow::Result<StoryRecipe> {
        recipe.updated_at = Some(next_revision(previous_revision));
        std::fs::create_dir_all(&self.project_root)
            .with_context(|| format!("create project directory {}", self.project_root.display()))?;
        self.ensure_safe_project_root()?;
        if !self.safe_root_exists()? {
            std::fs::create_dir_all(&self.root)
                .with_context(|| format!("create story directory {}", self.root.display()))?;
        }
        self.ensure_safe_root()?;
        let path = self.path(route_id);
        self.safe_recipe_exists(&path)?;
        let tmp = self
            .root
            .join(format!(".{route_id}.{}.json.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(&recipe)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
        drop(file);
        if let Err(error) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("promote story recipe {}", path.display()));
        }
        Ok(recipe)
    }

    /// Publish under the same writer critical section used for the version
    /// comparison and atomic replace. The callback validates the exact current
    /// recipe against current graph state while no Story writer can replace it.
    pub fn publish_checked<F>(
        &self,
        id: &str,
        expected_updated_at: &str,
        validate_current: F,
    ) -> anyhow::Result<StoryRecipe>
    where
        F: FnOnce(&StoryRecipe) -> anyhow::Result<()>,
    {
        validate_id(id)?;
        let _writer = STORY_WRITER
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut current = self
            .get(id)?
            .ok_or_else(|| anyhow::Error::new(StoryNotFound(format!("Story {id:?} not found"))))?;
        require_current_version(id, Some(&current), Some(expected_updated_at))?;
        validate_current(&current)?;
        current.status = StoryStatus::Published;
        validate_recipe(&current, true)?;
        self.ensure_unique_published_subject(id, &current.subject_component_iri)?;
        self.write_recipe(id, current, Some(expected_updated_at))
    }

    pub fn published_for_subject(&self, subject_iri: &str) -> anyhow::Result<Option<StoryRecipe>> {
        let mut matches = self.list_recipes()?.into_iter().filter(|recipe| {
            recipe.status == StoryStatus::Published && recipe.subject_component_iri == subject_iri
        });
        let first = matches.next();
        if matches.next().is_some() {
            anyhow::bail!(
                "multiple published Stories target subject {subject_iri}; repair the Story library"
            );
        }
        Ok(first)
    }

    fn ensure_unique_published_subject(&self, id: &str, subject_iri: &str) -> anyhow::Result<()> {
        if let Some(existing) = self.list_recipes()?.into_iter().find(|recipe| {
            recipe.id != id
                && recipe.status == StoryStatus::Published
                && recipe.subject_component_iri == subject_iri
        }) {
            return Err(anyhow::Error::new(StoryConflict(format!(
                "Story {:?} is already published for subject {subject_iri}",
                existing.id
            ))));
        }
        Ok(())
    }

    fn path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    fn ensure_safe_project_root(&self) -> anyhow::Result<()> {
        let metadata = std::fs::symlink_metadata(&self.project_root)
            .with_context(|| format!("inspect project root {}", self.project_root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(anyhow::Error::new(StoryInternal(format!(
                "Story project root must be a real directory: {}",
                self.project_root.display()
            ))));
        }
        Ok(())
    }

    fn safe_root_exists(&self) -> anyhow::Result<bool> {
        match std::fs::symlink_metadata(&self.root) {
            Ok(_) => {
                self.ensure_safe_root()?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("inspect story directory {}", self.root.display())),
        }
    }

    fn safe_recipe_exists(&self, path: &Path) -> anyhow::Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                self.ensure_safe_recipe_path(path)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("inspect story recipe {}", path.display()))
            }
        }
    }

    fn ensure_safe_root(&self) -> anyhow::Result<()> {
        self.ensure_safe_project_root()?;
        let metadata = std::fs::symlink_metadata(&self.root)
            .with_context(|| format!("inspect story directory {}", self.root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(anyhow::Error::new(StoryInternal(format!(
                "Story repository must be a real directory inside the project: {}",
                self.root.display()
            ))));
        }
        let project = self.project_root.canonicalize().with_context(|| {
            format!("canonicalize project root {}", self.project_root.display())
        })?;
        let stories = self
            .root
            .canonicalize()
            .with_context(|| format!("canonicalize story directory {}", self.root.display()))?;
        if stories.parent() != Some(project.as_path()) {
            return Err(anyhow::Error::new(StoryInternal(format!(
                "Story repository escapes project root: {}",
                self.root.display()
            ))));
        }
        Ok(())
    }

    fn ensure_safe_recipe_path(&self, path: &Path) -> anyhow::Result<()> {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspect story recipe {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(anyhow::Error::new(StoryInternal(format!(
                "Story recipe must be a regular file: {}",
                path.display()
            ))));
        }
        let stories = self
            .root
            .canonicalize()
            .with_context(|| format!("canonicalize story directory {}", self.root.display()))?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("canonicalize story recipe {}", path.display()))?;
        if canonical.parent() != Some(stories.as_path()) {
            return Err(anyhow::Error::new(StoryInternal(format!(
                "Story recipe escapes repository: {}",
                path.display()
            ))));
        }
        Ok(())
    }

    fn read_path(&self, path: &Path, expected_id: Option<&str>) -> anyhow::Result<StoryRecipe> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let recipe: StoryRecipe = serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::Error::new(StoryCorrupt(format!(
                "parse story recipe {}: {error}",
                path.display()
            )))
        })?;
        let recipe = normalize_recipe(recipe);
        if expected_id.is_some_and(|expected| recipe.id != expected) {
            return Err(anyhow::Error::new(StoryCorrupt(format!(
                "Story filename id {:?} does not match body id {:?}",
                expected_id.unwrap_or_default(),
                recipe.id
            ))));
        }
        validate_recipe(&recipe, recipe.status == StoryStatus::Published).map_err(|error| {
            anyhow::Error::new(StoryCorrupt(format!(
                "invalid stored Story recipe {}: {error}",
                path.display()
            )))
        })?;
        Ok(recipe)
    }
}

fn normalize_recipe(mut recipe: StoryRecipe) -> StoryRecipe {
    for beat in &mut recipe.beats {
        if beat.intent == StoryIntent::Boundary {
            beat.record_iris
                .retain(|iri| iri != &recipe.subject_component_iri);
        }
    }
    recipe
}

fn next_revision(previous: Option<&str>) -> String {
    let now = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .max(0) as u64;
    let previous = previous.and_then(revision_value).unwrap_or(0);
    let mut observed = LAST_STORY_REVISION.load(Ordering::Relaxed);
    loop {
        let candidate = now
            .max(previous.saturating_add(1))
            .max(observed.saturating_add(1));
        match LAST_STORY_REVISION.compare_exchange_weak(
            observed,
            candidate,
            Ordering::SeqCst,
            Ordering::Relaxed,
        ) {
            Ok(_) => return format!("{candidate:020}#{}", uuid::Uuid::new_v4()),
            Err(actual) => observed = actual,
        }
    }
}

fn revision_value(value: &str) -> Option<u64> {
    value
        .parse()
        .ok()
        .or_else(|| value.split_once('#')?.0.parse().ok())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()?
                .timestamp_nanos_opt()
                .map(|value| value.max(0) as u64)
        })
}

fn require_current_version(
    id: &str,
    current: Option<&StoryRecipe>,
    expected: Option<&str>,
) -> anyhow::Result<()> {
    let actual = current.and_then(|recipe| recipe.updated_at.as_deref());
    if actual != expected {
        return Err(anyhow::Error::new(StoryConflict(format!(
            "Story {id:?} changed since it was read; reload it before saving"
        ))));
    }
    Ok(())
}

pub fn validate_recipe(recipe: &StoryRecipe, publishing: bool) -> anyhow::Result<()> {
    validate_id(&recipe.id)?;
    require_text("title", &recipe.title)?;
    require_text("subject_component_iri", &recipe.subject_component_iri)?;
    NamedNode::new(&recipe.subject_component_iri)
        .map_err(|error| anyhow::anyhow!("subject_component_iri must be an IRI: {error}"))?;
    require_text("goal", &recipe.goal)?;
    require_text("curator", &recipe.curator)?;
    if recipe.audience != "reboarding" {
        anyhow::bail!("story audience must be \"reboarding\"");
    }
    if recipe.beats.len() > MAX_BEATS {
        anyhow::bail!("a Story may contain at most {MAX_BEATS} beats");
    }
    if publishing && !(MIN_PUBLISHED_BEATS..=MAX_BEATS).contains(&recipe.beats.len()) {
        anyhow::bail!("a published Story must contain 3 to 5 beats");
    }
    let mut ids = BTreeSet::new();
    let mut intents = BTreeSet::new();
    let mut previous_intent = None;
    for beat in &recipe.beats {
        validate_id(&beat.id)?;
        require_text("beat title", &beat.title)?;
        if !ids.insert(&beat.id) {
            anyhow::bail!("duplicate Story beat id {:?}", beat.id);
        }
        if publishing && !intents.insert(beat.intent.id()) {
            anyhow::bail!("a published Story may contain each beat intent only once");
        }
        let rank = story_intent_rank(&beat.intent);
        if publishing && previous_intent.is_some_and(|previous| previous >= rank) {
            anyhow::bail!(
                "published Story beats must follow purpose, boundary, core-code, governance, risk order"
            );
        }
        previous_intent = Some(rank);
        if publishing
            && beat.intent != StoryIntent::Boundary
            && beat.record_iris.is_empty()
            && beat.code_symbols.is_empty()
        {
            anyhow::bail!(
                "published Story beat {:?} must reference current authoritative evidence or code",
                beat.id
            );
        }
        validate_refs("record IRI", &beat.record_iris)?;
        validate_refs("code symbol", &beat.code_symbols)?;
    }
    Ok(())
}

fn story_intent_rank(intent: &StoryIntent) -> usize {
    match intent {
        StoryIntent::Purpose => 0,
        StoryIntent::Boundary => 1,
        StoryIntent::CoreCode => 2,
        StoryIntent::Governance => 3,
        StoryIntent::Risk => 4,
    }
}

/// One request-local snapshot reused across a Story library render. This avoids
/// rescanning every code entity and component for every recipe.
pub struct StoryResolutionIndex {
    components: Vec<ComponentEntry>,
    components_by_iri: BTreeMap<String, ComponentEntry>,
    known_components_by_iri: BTreeMap<String, ComponentEntry>,
    code_by_symbol: BTreeMap<String, StoryCodeAnchor>,
    code_entities: Vec<StoryCodeAnchor>,
}

impl StoryResolutionIndex {
    pub fn build(state: &AppState) -> anyhow::Result<Self> {
        let all_components = load_components(state)?;
        let known_components_by_iri = components_by_iri(&all_components);
        let components = all_components
            .into_iter()
            .filter(|component| component_is_current(state, component))
            .collect::<Vec<_>>();
        let components_by_iri = components.iter().filter_map(component_key_value).collect();
        let (code_by_symbol, code_entities) = all_code(state)?;
        Ok(Self {
            components,
            components_by_iri,
            known_components_by_iri,
            code_by_symbol,
            code_entities,
        })
    }

    pub fn resolve_component(&self, selector: &str) -> anyhow::Result<ResolveOutcome> {
        resolve_component_from(&self.components, selector)
    }

    pub fn recipe_subject(&self, iri: &str) -> StoryCandidate {
        self.known_components_by_iri
            .get(iri)
            .map(candidate)
            .unwrap_or_else(|| StoryCandidate {
                iri: iri.to_string(),
                label: iri.to_string(),
                description: None,
            })
    }
}

fn component_key_value(component: &ComponentEntry) -> Option<(String, ComponentEntry)> {
    component
        .iri
        .as_ref()
        .map(|iri| (iri.clone(), component.clone()))
}

fn components_by_iri(components: &[ComponentEntry]) -> BTreeMap<String, ComponentEntry> {
    components.iter().filter_map(component_key_value).collect()
}

fn component_is_current(state: &AppState, component: &ComponentEntry) -> bool {
    component.iri.as_deref().is_some_and(|iri| {
        first_literal(&state.store, iri, &state.capture.status)
            .as_deref()
            .is_none_or(in_working_set)
    })
}

/// Resolve display metadata and drift from one request-local graph snapshot.
pub fn enrich_summary(
    state: &AppState,
    index: &StoryResolutionIndex,
    recipe: &StoryRecipe,
    summary: &mut StorySummary,
) -> anyhow::Result<()> {
    summary.subject_label = index
        .known_components_by_iri
        .get(&summary.subject_component_iri)
        .map(|component| component.name.clone())
        .unwrap_or_else(|| summary.subject_component_iri.clone());
    summary.drifted = recipe_has_drift_with_index(state, index, recipe)?;
    Ok(())
}

pub fn recipe_has_drift(state: &AppState, recipe: &StoryRecipe) -> anyhow::Result<bool> {
    let index = StoryResolutionIndex::build(state)?;
    recipe_has_drift_with_index(state, &index, recipe)
}

fn recipe_has_drift_with_index(
    state: &AppState,
    index: &StoryResolutionIndex,
    recipe: &StoryRecipe,
) -> anyhow::Result<bool> {
    let component_exists = index
        .components_by_iri
        .contains_key(&recipe.subject_component_iri);
    if !component_exists {
        return Ok(true);
    }
    for beat in &recipe.beats {
        for symbol in &beat.code_symbols {
            let Some(anchor) = index.code_by_symbol.get(symbol) else {
                return Ok(true);
            };
            let Some(entity) = anchor.entity_iri.as_deref() else {
                return Ok(true);
            };
            if !code_realizes_component(state, entity, &recipe.subject_component_iri)? {
                return Ok(true);
            }
        }
        for iri in &beat.record_iris {
            let AnchorResolution::Current(record) = resolve_recipe_record(state, iri)? else {
                return Ok(true);
            };
            if !record_concerns_component(state, &record.iri, &recipe.subject_component_iri)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn validate_story_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        anyhow::bail!("invalid Story id {id:?}; use letters, digits, '-' or '_'");
    }
    Ok(())
}

fn validate_id(id: &str) -> anyhow::Result<()> {
    validate_story_id(id)
}

fn require_text(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    Ok(())
}

fn validate_refs(kind: &str, refs: &[String]) -> anyhow::Result<()> {
    if refs.len() > MAX_ANCHORS_PER_BEAT {
        anyhow::bail!("a Story beat may contain at most {MAX_ANCHORS_PER_BEAT} {kind}s");
    }
    let mut seen = BTreeSet::new();
    for value in refs {
        require_text(kind, value)?;
        if !seen.insert(value) {
            anyhow::bail!("duplicate {kind} {value:?}");
        }
    }
    Ok(())
}

fn resolve_component_from(
    components: &[ComponentEntry],
    selector: &str,
) -> anyhow::Result<ResolveOutcome> {
    let selector = selector.trim();
    if selector.is_empty() {
        anyhow::bail!("a component IRI or prompt is required");
    }
    let matches = resolve_component_query(components, selector);
    if matches.is_empty() {
        anyhow::bail!("no SystemComponent matches {selector:?}");
    }
    if matches.len() == 1 {
        Ok(ResolveOutcome::Resolved(candidate(matches[0])))
    } else {
        Ok(ResolveOutcome::Ambiguous(
            matches.into_iter().map(candidate).collect(),
        ))
    }
}

fn candidate(component: &ComponentEntry) -> StoryCandidate {
    StoryCandidate {
        iri: component.iri.clone().unwrap_or_default(),
        label: component.name.clone(),
        description: (!component.covers_paths.is_empty()).then(|| {
            format!(
                "Owns {}",
                component
                    .covers_paths
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
    }
}

pub fn generate_symbolic_with_index(
    state: &AppState,
    index: &StoryResolutionIndex,
    component: &StoryCandidate,
    recipe: Option<&StoryRecipe>,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    let records = component_records(state, &component.iri)?;
    let code = component_code(state, &component.iri)?;
    let (beats, mut gaps) = match recipe {
        Some(recipe) => recipe_beats(state, recipe, component, &index.code_by_symbol)?,
        None => {
            let component_status =
                first_literal(&state.store, &component.iri, &state.capture.status)
                    .unwrap_or_else(|| "unknown".to_string());
            generated_beats(component, &component_status, &records, &code)
        }
    };
    if recipe.is_none() {
        let pending = pending_component_record_count(state, &component.iri)?;
        if pending > 0 {
            gaps.push(StoryGap {
                id: "pending-knowledge".to_string(),
                title: "Pending knowledge is not authoritative".to_string(),
                detail: format!(
                    "{pending} proposed record(s) concern this component and await ratification; they are not used as Story evidence."
                ),
                beat_intent: None,
            });
        }
    }
    let subject_is_current = index.components_by_iri.contains_key(&component.iri);
    if !subject_is_current {
        gaps.push(StoryGap {
            id: "subject-drift".to_string(),
            title: "Story subject is unresolved".to_string(),
            detail: format!(
                "Component {} is no longer a current authoritative SystemComponent.",
                component.iri
            ),
            beat_intent: None,
        });
    }
    for beat in &beats {
        if let Some(detail) = &beat.gap {
            gaps.push(StoryGap {
                id: format!("gap-{}", beat.id),
                title: format!("Missing evidence for {}", beat.title),
                detail: detail.clone(),
                beat_intent: Some(beat.intent.clone()),
            });
        }
    }
    dedupe_gaps(&mut gaps);
    let (checks, viable_check_count) = if subject_is_current {
        build_checks(
            state,
            component,
            &beats,
            index,
            &records,
            &code,
            include_checks,
        )?
    } else {
        // A drifted recipe remains readable for recovery, but it cannot issue
        // an authoritative quiz about a subject outside the working set.
        (Vec::new(), 0)
    };
    if viable_check_count == 0 {
        gaps.push(StoryGap {
            id: "checks-unavailable".to_string(),
            title: "Comprehension check unavailable".to_string(),
            detail: "Current authoritative knowledge does not provide two distinct, unambiguous options for a symbolic relationship check.".to_string(),
            beat_intent: None,
        });
        dedupe_gaps(&mut gaps);
    }
    let (title, goal, trust_state, recipe_id) = match recipe {
        Some(recipe) => (
            recipe.title.clone(),
            recipe.goal.clone(),
            match recipe.status {
                StoryStatus::Draft => StoryTrustState::Draft,
                StoryStatus::Published => StoryTrustState::Published,
            },
            Some(recipe.id.clone()),
        ),
        None => (
            format!("The story of {}", component.label),
            format!("Understand {} before changing it", component.label),
            StoryTrustState::Generated,
            None,
        ),
    };
    Ok(StoryRun {
        recipe_id,
        trust_state,
        narration_mode: NarrationMode::Symbolic,
        title,
        subject: StorySubject {
            iri: component.iri.clone(),
            label: component.label.clone(),
        },
        goal,
        overview: format!(
            "A five-part, evidence-backed route through {}. Gaps are shown rather than filled by inference.",
            component.label
        ),
        beats,
        gaps,
        checks,
    })
}

fn generated_beats(
    component: &StoryCandidate,
    component_status: &str,
    records: &[RecordData],
    code: &[StoryCodeAnchor],
) -> (Vec<StoryBeat>, Vec<StoryGap>) {
    let purpose_records = record_data_of_kinds(records, &["Requirement"]);
    let governance_records =
        record_data_of_kinds(records, &["ArchitecturalDecision", "Constraint", "Pattern"]);
    let risk_records = record_data_of_kinds(
        records,
        &["Lesson", "AntiPattern", "Consequence", "Rationale"],
    );
    let boundary_evidence = vec![StoryEvidence {
        iri: component.iri.clone(),
        title: component.label.clone(),
        kind: "SystemComponent".to_string(),
        status: component_status.to_string(),
    }];
    let mut beats = vec![
        make_beat(
            "purpose",
            "Why it exists",
            StoryIntent::Purpose,
            purpose_records
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
        make_beat(
            "boundary",
            "Where its boundary lies",
            StoryIntent::Boundary,
            boundary_evidence,
            vec![],
            component.description.clone(),
            None,
        ),
        make_beat(
            "core-code",
            "Code to understand first",
            StoryIntent::CoreCode,
            vec![],
            code.iter().take(MAX_ANCHORS_PER_BEAT).cloned().collect(),
            None,
            None,
        ),
        make_beat(
            "governance",
            "Decisions and constraints",
            StoryIntent::Governance,
            governance_records
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
        make_beat(
            "risk",
            "Risks and lessons",
            StoryIntent::Risk,
            risk_records
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
    ];
    apply_extractive_narrative_to(&mut beats, "purpose", &purpose_records);
    apply_extractive_narrative_to(&mut beats, "governance", &governance_records);
    apply_extractive_narrative_to(&mut beats, "risk", &risk_records);
    (beats, Vec::new())
}

fn apply_extractive_narrative_to(beats: &mut [StoryBeat], beat_id: &str, records: &[&RecordData]) {
    if let Some(beat) = beats.iter_mut().find(|beat| beat.id == beat_id) {
        apply_extractive_narrative(beat, records);
    }
}

fn make_beat(
    id: &str,
    title: &str,
    intent: StoryIntent,
    evidence: Vec<StoryEvidence>,
    code_anchors: Vec<StoryCodeAnchor>,
    detail: Option<String>,
    curator_note: Option<String>,
) -> StoryBeat {
    let gap = if evidence.is_empty() && code_anchors.is_empty() {
        Some(format!(
            "No current authoritative {} evidence is linked to this component.",
            intent.id()
        ))
    } else {
        None
    };
    let narrative = symbolic_narrative(&evidence, &code_anchors, detail.as_deref());
    StoryBeat {
        id: id.to_string(),
        title: title.to_string(),
        intent,
        narrative,
        evidence,
        code_anchors,
        curator_note,
        gap,
    }
}

fn symbolic_narrative(
    evidence: &[StoryEvidence],
    code: &[StoryCodeAnchor],
    detail: Option<&str>,
) -> String {
    let base = if !code.is_empty() {
        format!(
            "Start with {}.",
            code.iter()
                .map(|anchor| anchor.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else if evidence.is_empty() {
        "The project graph does not currently contain authoritative evidence for this beat."
            .to_string()
    } else {
        evidence
            .iter()
            .map(|item| format!("{}: {}", item.kind, item.title))
            .collect::<Vec<_>>()
            .join(". ")
            + "."
    };
    match detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{detail}\n\n{base}"),
        None => base,
    }
}

fn record_data_of_kinds<'a>(records: &'a [RecordData], kinds: &[&str]) -> Vec<&'a RecordData> {
    records
        .iter()
        .filter(|record| kinds.contains(&record.evidence.kind.as_str()))
        .take(MAX_ANCHORS_PER_BEAT)
        .collect()
}

fn apply_extractive_narrative(beat: &mut StoryBeat, records: &[&RecordData]) {
    let claims = records
        .iter()
        .filter_map(|record| {
            record
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(|description| {
                    let clipped = truncate_utf8(description, 320);
                    format!("{}: {}", record.evidence.title, clipped)
                })
        })
        .collect::<Vec<_>>();
    if !claims.is_empty() {
        beat.narrative = claims.join("\n\n");
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn recipe_beats(
    state: &AppState,
    recipe: &StoryRecipe,
    component: &StoryCandidate,
    code_by_symbol: &BTreeMap<String, StoryCodeAnchor>,
) -> anyhow::Result<(Vec<StoryBeat>, Vec<StoryGap>)> {
    let subject_iri = component.iri.as_str();
    let mut beats = Vec::new();
    let mut gaps = Vec::new();
    for spec in &recipe.beats {
        let mut evidence = Vec::new();
        for iri in &spec.record_iris {
            match resolve_recipe_record(state, iri)? {
                AnchorResolution::Current(item) => {
                    if record_concerns_component(state, &item.iri, subject_iri)? {
                        evidence.push(item);
                    } else {
                        gaps.push(StoryGap {
                            id: format!("subject-record-{}-{}", spec.id, gaps.len()),
                            title: "Story record does not concern this subject".to_string(),
                            detail: format!(
                                "Record {iri} is not linked to effective Story subject {subject_iri}."
                            ),
                            beat_intent: Some(spec.intent.clone()),
                        });
                    }
                }
                AnchorResolution::Superseded { replacements } => {
                    let detail = match replacements.as_slice() {
                        [successor] => format!(
                            "{iri} is retired; current successor {} is shown.",
                            successor.iri
                        ),
                        [] => {
                            format!("{iri} is retired and no current successor can be resolved.")
                        }
                        successors => format!(
                            "{iri} is retired and has multiple current successors ({}); curator selection is required.",
                            successors
                                .iter()
                                .map(|successor| successor.iri.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    gaps.push(StoryGap {
                        id: format!("drift-{}-{}", spec.id, gaps.len()),
                        title: "Story anchor was superseded".to_string(),
                        detail,
                        beat_intent: Some(spec.intent.clone()),
                    });
                    if let [item] = replacements.as_slice() {
                        if record_concerns_component(state, &item.iri, subject_iri)? {
                            evidence.push(item.clone());
                        } else {
                            gaps.push(StoryGap {
                                id: format!("subject-record-{}-{}", spec.id, gaps.len()),
                                title: "Story successor does not concern this subject".to_string(),
                                detail: format!(
                                    "Successor {} is not linked to effective Story subject {subject_iri}.",
                                    item.iri
                                ),
                                beat_intent: Some(spec.intent.clone()),
                            });
                        }
                    }
                }
                AnchorResolution::Missing => gaps.push(StoryGap {
                    id: format!("missing-{}-{}", spec.id, gaps.len()),
                    title: "Story anchor is missing".to_string(),
                    detail: format!("Record {iri} cannot be resolved."),
                    beat_intent: Some(spec.intent.clone()),
                }),
                AnchorResolution::Ineligible(status) => gaps.push(StoryGap {
                    id: format!("inactive-{}-{}", spec.id, gaps.len()),
                    title: "Story anchor is not current".to_string(),
                    detail: format!("Record {iri} has lifecycle status {status}."),
                    beat_intent: Some(spec.intent.clone()),
                }),
            }
        }
        let mut anchors = Vec::new();
        for symbol in &spec.code_symbols {
            if let Some(anchor) = code_by_symbol.get(symbol) {
                let grounded = match anchor.entity_iri.as_deref() {
                    Some(entity) => code_realizes_component(state, entity, subject_iri)?,
                    None => false,
                };
                if grounded {
                    anchors.push(anchor.clone());
                } else {
                    gaps.push(StoryGap {
                        id: format!("subject-code-{}-{}", spec.id, gaps.len()),
                        title: "Story code does not realize this subject".to_string(),
                        detail: format!(
                            "Symbol {symbol} is not linked to effective Story subject {subject_iri}."
                        ),
                        beat_intent: Some(spec.intent.clone()),
                    });
                }
            } else {
                gaps.push(StoryGap {
                    id: format!("code-{}-{}", spec.id, gaps.len()),
                    title: "Code anchor is unresolved".to_string(),
                    detail: format!("Symbol {symbol} is not present in the current code graph."),
                    beat_intent: Some(spec.intent.clone()),
                });
            }
        }
        if spec.intent == StoryIntent::Boundary && is_system_component(state, subject_iri)? {
            evidence.insert(
                0,
                StoryEvidence {
                    iri: subject_iri.to_string(),
                    title: component.label.clone(),
                    kind: "SystemComponent".to_string(),
                    status: first_literal(&state.store, subject_iri, &state.capture.status)
                        .unwrap_or_else(|| "unknown".to_string()),
                },
            );
        }
        let beat = make_beat(
            &spec.id,
            &spec.title,
            spec.intent.clone(),
            evidence,
            anchors,
            (spec.intent == StoryIntent::Boundary)
                .then(|| component.description.clone())
                .flatten(),
            spec.curator_note.clone(),
        );
        beats.push(beat);
    }
    Ok((beats, gaps))
}

#[derive(Debug)]
enum AnchorResolution {
    Current(StoryEvidence),
    Superseded { replacements: Vec<StoryEvidence> },
    Missing,
    Ineligible(String),
}

fn resolve_recipe_record(state: &AppState, iri: &str) -> anyhow::Result<AnchorResolution> {
    let Ok(node) = NamedNode::new(iri) else {
        return Ok(AnchorResolution::Missing);
    };
    if crate::graph::capture::require_information_record(state, &node).is_err() {
        return Ok(AnchorResolution::Missing);
    }
    let Some(record) = record_data(state, iri)? else {
        return Ok(AnchorResolution::Missing);
    };
    if in_working_set(&record.evidence.status) {
        return Ok(AnchorResolution::Current(record.evidence));
    }
    if record.evidence.status.eq_ignore_ascii_case("superseded") {
        let successors = accepted_successors(state, iri)?;
        return Ok(AnchorResolution::Superseded {
            replacements: successors,
        });
    }
    Ok(AnchorResolution::Ineligible(record.evidence.status))
}

fn component_records(state: &AppState, component_iri: &str) -> anyhow::Result<Vec<RecordData>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let object = NamedNodeRef::new(component_iri)?;
    let concerns = state.resolve_object_property("concerns")?;
    let predicate = NamedNodeRef::new(&concerns)?;
    let mut out = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(object.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        if let Some(record) = record_data(state, subject.as_str())? {
            if in_working_set(&record.evidence.status) {
                out.push(record);
            }
        }
    }
    let inverse_iri = state.resolve_object_property("isConcernedBy")?;
    let inverse = NamedNodeRef::new(&inverse_iri)?;
    let component = NamedNodeRef::new(component_iri)?;
    for quad in state.store.quads_for_pattern(
        Some(component.into()),
        Some(inverse),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(record_node) = quad?.object {
            if let Some(record) = record_data(state, record_node.as_str())? {
                if in_working_set(&record.evidence.status) {
                    out.push(record);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.evidence
            .kind
            .cmp(&b.evidence.kind)
            .then(a.evidence.title.cmp(&b.evidence.title))
            .then(a.evidence.iri.cmp(&b.evidence.iri))
    });
    out.dedup_by(|a, b| a.evidence.iri == b.evidence.iri);
    Ok(out)
}

fn pending_component_record_count(state: &AppState, component_iri: &str) -> anyhow::Result<usize> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let component = NamedNodeRef::new(component_iri)?;
    let concerns_iri = state.resolve_object_property("concerns")?;
    let concerns = NamedNodeRef::new(&concerns_iri)?;
    let inverse_iri = state.resolve_object_property("isConcernedBy")?;
    let inverse = NamedNodeRef::new(&inverse_iri)?;
    let mut records = BTreeSet::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(concerns),
        Some(component.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let NamedOrBlankNode::NamedNode(record) = quad?.subject {
            records.insert(record.as_str().to_string());
        }
    }
    for quad in state.store.quads_for_pattern(
        Some(component.into()),
        Some(inverse),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(record) = quad?.object {
            records.insert(record.as_str().to_string());
        }
    }
    Ok(records
        .into_iter()
        .filter(|iri| {
            let Ok(node) = NamedNode::new(iri) else {
                return false;
            };
            crate::graph::capture::require_information_record(state, &node).is_ok()
                && first_literal(&state.store, iri, &state.capture.status)
                    .as_deref()
                    .is_some_and(|status| status.eq_ignore_ascii_case("proposed"))
        })
        .count())
}

#[derive(Debug, Clone)]
struct RecordData {
    evidence: StoryEvidence,
    description: Option<String>,
}

fn record_data(state: &AppState, iri: &str) -> anyhow::Result<Option<RecordData>> {
    let node = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    if crate::graph::capture::require_information_record(state, &node).is_err() {
        return Ok(None);
    }
    let kinds = asserted_project_types(state, &node)
        .into_iter()
        .map(|kind| local_name(&kind).to_string())
        .filter(|kind| kind != "NamedIndividual")
        .collect::<Vec<_>>();
    let kind = choose_record_kind(kinds);
    let Some(kind) = kind else {
        return Ok(None);
    };
    let title = first_literal(&state.store, iri, moose::RDFS_LABEL)
        .or_else(|| first_literal(&state.store, iri, &state.capture.title))
        .unwrap_or_else(|| local_name(iri).to_string());
    let status = first_literal(&state.store, iri, &state.capture.status)
        .unwrap_or_else(|| "unknown".to_string());
    let description = first_literal(&state.store, iri, &state.capture.description);
    Ok(Some(RecordData {
        evidence: StoryEvidence {
            iri: iri.to_string(),
            title,
            kind,
            status,
        },
        description,
    }))
}

fn choose_record_kind(mut kinds: Vec<String>) -> Option<String> {
    kinds.sort_by(|left, right| {
        record_kind_rank(left)
            .cmp(&record_kind_rank(right))
            .then(left.cmp(right))
    });
    kinds.dedup();
    kinds.into_iter().next()
}

fn record_kind_rank(kind: &str) -> usize {
    [
        "Requirement",
        "ArchitecturalDecision",
        "Constraint",
        "Pattern",
        "AntiPattern",
        "Lesson",
        "Consequence",
        "Rationale",
    ]
    .iter()
    .position(|candidate| *candidate == kind)
    .unwrap_or(match kind {
        "InformationRecord" => usize::MAX - 1,
        "ProjectEntity" => usize::MAX,
        _ => usize::MAX - 2,
    })
}

fn successor_iris(state: &AppState, iri: &str) -> anyhow::Result<Vec<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(iri)?;
    let predicate_iri = state.resolve_object_property("isSupersededBy")?;
    let predicate = NamedNodeRef::new(&predicate_iri)?;
    let mut out = Vec::new();
    for quad in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(node) = quad?.object {
            out.push(node.as_str().to_string());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn accepted_successors(state: &AppState, retired_iri: &str) -> anyhow::Result<Vec<StoryEvidence>> {
    let mut visited = BTreeSet::from([retired_iri.to_string()]);
    let mut frontier = VecDeque::new();
    for successor in successor_iris(state, retired_iri)? {
        if visited.insert(successor.clone()) {
            frontier.push_back(successor);
        }
    }
    let mut accepted = BTreeMap::new();
    for _ in 0..256 {
        let Some(next) = frontier.pop_front() else {
            break;
        };
        if let Some(record) = record_data(state, &next)? {
            if in_working_set(&record.evidence.status) {
                accepted.insert(record.evidence.iri.clone(), record.evidence);
                continue;
            }
        }
        for successor in successor_iris(state, &next)? {
            if visited.insert(successor.clone()) {
                frontier.push_back(successor);
            }
        }
    }
    Ok(accepted.into_values().collect())
}

fn component_code(state: &AppState, component_iri: &str) -> anyhow::Result<Vec<StoryCodeAnchor>> {
    let terms = CodeTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.realizes)?;
    let object = NamedNodeRef::new(component_iri)?;
    let mut anchors = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(object.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        if let Some(anchor) = code_anchor(state, &terms, subject.as_str())? {
            anchors.push(anchor);
        }
    }
    Ok(dedupe_code_anchors(anchors))
}

#[cfg(test)]
fn all_code_by_symbol(state: &AppState) -> anyhow::Result<BTreeMap<String, StoryCodeAnchor>> {
    Ok(all_code(state)?.0)
}

fn all_code(
    state: &AppState,
) -> anyhow::Result<(BTreeMap<String, StoryCodeAnchor>, Vec<StoryCodeAnchor>)> {
    let terms = CodeTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.has_substrate_symbol)?;
    let mut by_symbol = BTreeMap::new();
    let mut entities = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        if let Some(anchor) = code_anchor(state, &terms, subject.as_str())? {
            entities.push(anchor.clone());
            insert_code_anchor(&mut by_symbol, anchor);
        }
    }
    entities.sort_by(code_anchor_order);
    entities.dedup_by(|left, right| left.entity_iri == right.entity_iri);
    Ok((by_symbol, entities))
}

fn insert_code_anchor(anchors: &mut BTreeMap<String, StoryCodeAnchor>, candidate: StoryCodeAnchor) {
    anchors
        .entry(candidate.symbol.clone())
        .and_modify(|current| {
            if code_anchor_order(&candidate, current).is_lt() {
                *current = candidate.clone();
            }
        })
        .or_insert(candidate);
}

fn code_anchor_order(left: &StoryCodeAnchor, right: &StoryCodeAnchor) -> std::cmp::Ordering {
    left.symbol
        .cmp(&right.symbol)
        .then(left.entity_iri.cmp(&right.entity_iri))
        .then(left.label.cmp(&right.label))
        .then(left.path.cmp(&right.path))
}

fn dedupe_code_anchors(mut anchors: Vec<StoryCodeAnchor>) -> Vec<StoryCodeAnchor> {
    anchors.sort_by(code_anchor_order);
    anchors.dedup_by(|left, right| left.symbol == right.symbol);
    anchors
}

fn code_anchor(
    state: &AppState,
    terms: &CodeTerms,
    iri: &str,
) -> anyhow::Result<Option<StoryCodeAnchor>> {
    if !is_instance_of(state, iri, &terms.code_entity_class)?
        || first_literal(&state.store, iri, &state.capture.status)
            .as_deref()
            .is_some_and(|status| !in_working_set(status))
    {
        return Ok(None);
    }
    let Some(symbol) = first_literal(&state.store, iri, &terms.has_substrate_symbol) else {
        return Ok(None);
    };
    Ok(Some(StoryCodeAnchor {
        label: first_literal(&state.store, iri, &terms.has_code_name)
            .or_else(|| first_literal(&state.store, iri, moose::RDFS_LABEL))
            .unwrap_or_else(|| symbol.clone()),
        symbol,
        entity_iri: Some(iri.to_string()),
        path: first_literal(&state.store, iri, &terms.defined_in_path),
        line: None,
    }))
}

fn is_instance_of(state: &AppState, iri: &str, class_iri: &str) -> anyhow::Result<bool> {
    let node = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(false),
    };
    Ok(asserted_project_types(state, &node)
        .into_iter()
        .any(|kind| crate::graph::util::is_subclass_of(&state.store, &kind, class_iri)))
}

fn is_system_component(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    let class = state.resolve_class("SystemComponent")?;
    is_instance_of(state, iri, &class)
}

fn component_iri_is_current(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    Ok(is_system_component(state, iri)?
        && first_literal(&state.store, iri, &state.capture.status)
            .as_deref()
            .is_none_or(in_working_set))
}

fn code_entity_is_current(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    let terms = CodeTerms::resolve(state)?;
    Ok(code_anchor(state, &terms, iri)?.is_some())
}

fn dedupe_gaps(gaps: &mut Vec<StoryGap>) {
    let mut seen = BTreeSet::new();
    gaps.retain(|gap| seen.insert((gap.title.clone(), gap.detail.clone())));
}

fn build_checks(
    state: &AppState,
    component: &StoryCandidate,
    beats: &[StoryBeat],
    index: &StoryResolutionIndex,
    component_records: &[RecordData],
    component_code: &[StoryCodeAnchor],
    include_checks: bool,
) -> anyhow::Result<(Vec<StoryCheck>, usize)> {
    let mut checks = Vec::new();
    let mut viable_check_count = 0;
    let target_record_ids = component_records
        .iter()
        .map(|record| record.evidence.iri.clone())
        .collect::<BTreeSet<_>>();
    let displayed_record_ids = beats
        .iter()
        .flat_map(|beat| beat.evidence.iter())
        .filter(|evidence| {
            evidence.kind != "SystemComponent" && target_record_ids.contains(&evidence.iri)
        })
        .map(|evidence| evidence.iri.clone())
        .collect::<Vec<_>>();
    let record_facts = records_for_components(state, &index.components)?
        .into_iter()
        .chain(component_records.iter().cloned())
        .map(|record| CheckOptionFact {
            matches_target: target_record_ids.contains(&record.evidence.iri),
            id: record.evidence.iri,
            label: record.evidence.title,
        });
    if let Some((correct_id, options)) =
        unambiguous_check_options(&displayed_record_ids, record_facts)
    {
        let beat_id = beats
            .iter()
            .find(|beat| {
                beat.evidence
                    .iter()
                    .any(|evidence| evidence.iri == correct_id)
            })
            .map(|beat| beat.id.as_str())
            .unwrap_or_default();
        if push_check(
            state,
            &mut checks,
            CheckSpec {
                kind: CheckKind::Concerns,
                component_iri: &component.iri,
                beat_id,
                correct_option_id: &correct_id,
                question: format!("Which accepted record is linked to {}?", component.label),
                options,
            },
            include_checks,
        ) {
            viable_check_count += 1;
        }
    }
    if viable_check_count < 2 {
        let target_code_ids = component_code
            .iter()
            .filter_map(|anchor| anchor.entity_iri.clone())
            .collect::<BTreeSet<_>>();
        let displayed_code_ids = beats
            .iter()
            .flat_map(|beat| beat.code_anchors.iter())
            .filter_map(|anchor| anchor.entity_iri.as_ref())
            .filter(|entity| target_code_ids.contains(*entity))
            .cloned()
            .collect::<Vec<_>>();
        let code_facts = index
            .code_entities
            .iter()
            .chain(component_code.iter())
            .filter_map(|anchor| {
                anchor.entity_iri.as_ref().map(|entity| CheckOptionFact {
                    matches_target: target_code_ids.contains(entity),
                    id: entity.clone(),
                    label: anchor.label.clone(),
                })
            });
        if let Some((correct_id, options)) =
            unambiguous_check_options(&displayed_code_ids, code_facts)
        {
            if let Some(beat_id) = beats.iter().find_map(|beat| {
                beat.code_anchors
                    .iter()
                    .any(|anchor| anchor.entity_iri.as_deref() == Some(correct_id.as_str()))
                    .then_some(beat.id.as_str())
            }) {
                if push_check(
                    state,
                    &mut checks,
                    CheckSpec {
                        kind: CheckKind::Realizes,
                        component_iri: &component.iri,
                        beat_id,
                        correct_option_id: &correct_id,
                        question: format!("Which code entity realizes {}?", component.label),
                        options,
                    },
                    include_checks,
                ) {
                    viable_check_count += 1;
                }
            }
        }
    }
    // Keep the API bounded even if future generators add more check kinds.
    checks.truncate(2);
    Ok((checks, viable_check_count.min(2)))
}

#[derive(Debug, Clone)]
struct CheckOptionFact {
    id: String,
    label: String,
    matches_target: bool,
}

fn unambiguous_check_options(
    displayed_correct_ids: &[String],
    facts: impl IntoIterator<Item = CheckOptionFact>,
) -> Option<(String, Vec<StoryCheckOption>)> {
    let mut facts_by_id = BTreeMap::<String, CheckOptionFact>::new();
    for fact in facts {
        if fact.id.is_empty() || normalize_visible_label(&fact.label).is_empty() {
            continue;
        }
        facts_by_id
            .entry(fact.id.clone())
            .and_modify(|current| current.matches_target |= fact.matches_target)
            .or_insert(fact);
    }

    let mut groups = BTreeMap::<String, Vec<CheckOptionFact>>::new();
    for fact in facts_by_id.into_values() {
        groups
            .entry(normalize_visible_label(&fact.label))
            .or_default()
            .push(fact);
    }
    for group in groups.values_mut() {
        group.sort_by(|left, right| left.id.cmp(&right.id));
    }

    let correct = displayed_correct_ids.iter().find_map(|displayed_id| {
        groups.values().find_map(|group| {
            let candidate = group.iter().find(|fact| &fact.id == displayed_id)?;
            (candidate.matches_target && group.iter().all(|fact| fact.matches_target))
                .then(|| candidate.clone())
        })
    })?;
    let mut options = vec![StoryCheckOption {
        id: correct.id.clone(),
        label: correct.label,
    }];
    for group in groups.values() {
        if group.iter().all(|fact| !fact.matches_target) {
            let candidate = &group[0];
            options.push(StoryCheckOption {
                id: candidate.id.clone(),
                label: candidate.label.clone(),
            });
            if options.len() == MAX_CHECK_OPTIONS {
                break;
            }
        }
    }
    (options.len() >= 2).then_some((correct.id, options))
}

struct CheckSpec<'a> {
    kind: CheckKind,
    component_iri: &'a str,
    beat_id: &'a str,
    correct_option_id: &'a str,
    question: String,
    options: Vec<StoryCheckOption>,
}

fn push_check(
    state: &AppState,
    checks: &mut Vec<StoryCheck>,
    mut spec: CheckSpec<'_>,
    include_check: bool,
) -> bool {
    if !nontrivial_options(&mut spec.options) {
        return false;
    }
    if include_check {
        let (correct_option_token, options, option_entities) =
            opaque_options(spec.options, spec.correct_option_id, uuid::Uuid::new_v4);
        let handle = register_check(
            state,
            CheckGrant {
                kind: spec.kind,
                component_iri: spec.component_iri.to_string(),
                beat_id: spec.beat_id.to_string(),
                correct_option_token,
                option_entities,
                correct_entity_iri: spec.correct_option_id.to_string(),
                expires_at: std::time::Instant::now() + CHECK_TTL,
            },
        );
        checks.push(StoryCheck {
            id: handle,
            question: spec.question,
            options,
        });
    }
    true
}

fn opaque_options(
    options: Vec<StoryCheckOption>,
    correct_option_id: &str,
    mut random_uuid: impl FnMut() -> uuid::Uuid,
) -> (String, Vec<StoryCheckOption>, BTreeMap<String, String>) {
    let mut correct_option_token = String::new();
    let mut option_entities = BTreeMap::new();
    let mut randomized = options
        .into_iter()
        .map(|option| {
            let token = random_uuid().to_string();
            if option.id == correct_option_id {
                correct_option_token = token.clone();
            }
            option_entities.insert(token.clone(), option.id);
            (
                random_uuid(),
                StoryCheckOption {
                    id: token,
                    label: option.label,
                },
            )
        })
        .collect::<Vec<_>>();
    randomized.sort_by_key(|(order, _)| *order);
    (
        correct_option_token,
        randomized.into_iter().map(|(_, option)| option).collect(),
        option_entities,
    )
}

fn register_check(state: &AppState, grant: CheckGrant) -> String {
    let handle = uuid::Uuid::new_v4().to_string();
    let now = std::time::Instant::now();
    let mut registry = state
        .story_checks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let expired = registry
        .grants
        .iter()
        .filter(|(_, grant)| grant.expires_at <= now)
        .map(|(handle, _)| handle.clone())
        .collect::<Vec<_>>();
    for handle in expired {
        registry.grants.remove(&handle);
        remember_retired(&mut registry, handle, RetiredCheckKind::Expired);
    }
    if registry.grants.len() >= MAX_CHECK_GRANTS {
        if let Some(oldest) = registry
            .grants
            .iter()
            .min_by_key(|(_, grant)| grant.expires_at)
            .map(|(handle, _)| handle.clone())
        {
            registry.grants.remove(&oldest);
            remember_retired(&mut registry, oldest, RetiredCheckKind::Evicted);
        }
    }
    registry.grants.insert(handle.clone(), grant);
    handle
}

fn remember_retired(registry: &mut StoryCheckRegistry, handle: String, kind: RetiredCheckKind) {
    if registry.retired.len() >= MAX_RETIRED_CHECK_HANDLES {
        registry.retired.pop_front();
    }
    registry.retired.push_back((handle, kind));
}

fn nontrivial_options(options: &mut Vec<StoryCheckOption>) -> bool {
    // Callers put the correct answer first. Stable retention therefore keeps
    // it while removing duplicate IDs/visible labels, then admits at most two
    // distinct distractors before opaque randomization.
    let mut seen_ids = BTreeSet::new();
    let mut seen_labels = BTreeSet::new();
    options.retain(|option| {
        let label = normalize_visible_label(&option.label);
        !option.id.is_empty()
            && !label.is_empty()
            && seen_ids.insert(option.id.clone())
            && seen_labels.insert(label)
    });
    options.truncate(MAX_CHECK_OPTIONS);
    options.len() >= 2
}

fn normalize_visible_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn records_for_components(
    state: &AppState,
    components: &[ComponentEntry],
) -> anyhow::Result<Vec<RecordData>> {
    let mut out = Vec::new();
    for component in components {
        let Some(iri) = component.iri.as_deref() else {
            continue;
        };
        out.extend(component_records(state, iri)?);
    }
    out.sort_by(|left, right| left.evidence.iri.cmp(&right.evidence.iri));
    out.dedup_by(|left, right| left.evidence.iri == right.evidence.iri);
    Ok(out)
}

pub fn grade_check(
    state: &AppState,
    check_id: &str,
    selected: &[String],
) -> anyhow::Result<GradeResult> {
    if uuid::Uuid::parse_str(check_id).is_err() {
        return Err(StoryCheckError::Malformed.into());
    }
    let grant = {
        let mut registry = state
            .story_checks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(grant) = registry.grants.get(check_id).cloned() else {
            let error = registry
                .retired
                .iter()
                .rev()
                .find(|(handle, _)| handle == check_id)
                .map(|(_, kind)| match kind {
                    RetiredCheckKind::Expired => StoryCheckError::Expired,
                    RetiredCheckKind::Evicted => StoryCheckError::Evicted,
                })
                .unwrap_or(StoryCheckError::Unknown);
            return Err(error.into());
        };
        if grant.expires_at <= std::time::Instant::now() {
            registry.grants.remove(check_id);
            remember_retired(
                &mut registry,
                check_id.to_string(),
                RetiredCheckKind::Expired,
            );
            return Err(StoryCheckError::Expired.into());
        }
        grant
    };
    if selected.len() != 1
        || selected
            .iter()
            .any(|token| !grant.option_entities.contains_key(token))
    {
        return Err(StoryCheckError::ForeignOption.into());
    }
    if !component_iri_is_current(state, &grant.component_iri)? {
        return Err(StoryCheckError::Stale.into());
    }
    let endpoint_is_current = |entity: &str| -> anyhow::Result<bool> {
        match grant.kind {
            CheckKind::Concerns => Ok(record_data(state, entity)?
                .is_some_and(|record| in_working_set(&record.evidence.status))),
            CheckKind::Realizes => code_entity_is_current(state, entity),
        }
    };
    for entity in grant.option_entities.values() {
        if !endpoint_is_current(entity)? {
            return Err(StoryCheckError::Stale.into());
        }
    }
    let relationship_is_current = |entity: &str| match grant.kind {
        CheckKind::Concerns => record_concerns_component(state, entity, &grant.component_iri),
        CheckKind::Realizes => code_realizes_component(state, entity, &grant.component_iri),
    };
    let mut distractor_became_valid = false;
    for (token, entity) in &grant.option_entities {
        if token != &grant.correct_option_token && relationship_is_current(entity)? {
            distractor_became_valid = true;
            break;
        }
    }
    if !relationship_is_current(&grant.correct_entity_iri)? || distractor_became_valid {
        return Err(StoryCheckError::Stale.into());
    }
    let correct = selected == [grant.correct_option_token.as_str()];
    let evidence_iris = if correct {
        vec![grant.correct_entity_iri.clone()]
    } else {
        vec![grant.correct_entity_iri]
    };
    Ok(GradeResult {
        correct,
        feedback: if correct {
            "Correct. The selected relationship is present in current authoritative project knowledge."
                .to_string()
        } else {
            "Not quite. Revisit the evidence linked to this Story beat.".to_string()
        },
        revisit_beat_id: (!correct).then_some(grant.beat_id),
        evidence_iris,
    })
}

fn record_concerns_component(
    state: &AppState,
    record: &str,
    component: &str,
) -> anyhow::Result<bool> {
    if !component_iri_is_current(state, component)? {
        return Ok(false);
    }
    let Some(record) = record_data(state, record)? else {
        return Ok(false);
    };
    if !in_working_set(&record.evidence.status) {
        return Ok(false);
    }
    Ok(
        edge_exists(state, &record.evidence.iri, "concerns", component)?
            || edge_exists(state, component, "isConcernedBy", &record.evidence.iri)?,
    )
}

fn code_realizes_component(
    state: &AppState,
    entity: &str,
    component: &str,
) -> anyhow::Result<bool> {
    if !component_iri_is_current(state, component)? || !code_entity_is_current(state, entity)? {
        return Ok(false);
    }
    edge_exists(state, entity, "realizes", component)
}

fn edge_exists(
    state: &AppState,
    subject: &str,
    predicate: &str,
    object: &str,
) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject)?;
    let predicate_iri = state.resolve_object_property(predicate)?;
    let predicate = NamedNodeRef::new(&predicate_iri)?;
    let object = NamedNodeRef::new(object)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            Some(object.into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}

/// Optionally rewrite only the already-grounded beat prose. Any sensor error,
/// timeout, malformed JSON, citation mismatch, or incomplete response preserves
/// the complete symbolic run.
pub async fn narrate_with_llm(state: &AppState, run: StoryRun, assist_level: u8) -> StoryRun {
    if assist_level == 0
        || !state.llm_configured
        || state.engine_config.llm_assist_level == LlmAssistLevel::PureSymbolic
    {
        return run;
    }
    if !narration_evidence_is_current(&run) {
        return run;
    }
    let eligible = run
        .beats
        .iter()
        .filter(|beat| !beat.evidence.is_empty() || !beat.code_anchors.is_empty())
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return run;
    }
    let Some(prompt) = build_narration_prompt(state, &eligible) else {
        return run;
    };
    let llm = state.llm.with_fresh_usage();
    let params = LlmParams {
        temperature: Some(0.0),
        ..LlmParams::default()
    };
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        llm.chat_completion(&state.model, &prompt, Some(&params)),
    )
    .await;
    let Ok(Ok(raw)) = response else {
        return run;
    };
    apply_narration_response(run.clone(), &raw).unwrap_or(run)
}

fn narration_evidence_is_current(run: &StoryRun) -> bool {
    !run.gaps.iter().any(|gap| gap.id == "subject-drift")
        && run
            .beats
            .iter()
            .flat_map(|beat| &beat.evidence)
            .all(|evidence| in_working_set(&evidence.status))
}

fn build_narration_prompt(state: &AppState, eligible: &[&StoryBeat]) -> Option<String> {
    let prompt_beats = eligible
        .iter()
        .map(|beat| {
            let evidence = beat
                .evidence
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let description = first_literal(
                        &state.store,
                        &item.iri,
                        &state.capture.description,
                    )
                    .map(|value| truncate_utf8(&value, MAX_LLM_FIELD_BYTES).to_string());
                    serde_json::json!({
                        "id": format!("e{index}"),
                        "title": truncate_utf8(&item.title, MAX_LLM_FIELD_BYTES),
                        "kind": truncate_utf8(&item.kind, MAX_LLM_FIELD_BYTES),
                        "status": truncate_utf8(&item.status, MAX_LLM_FIELD_BYTES),
                        "description": description,
                    })
                })
                .chain(beat.code_anchors.iter().enumerate().map(|(index, item)| {
                    serde_json::json!({
                        "id": format!("c{index}"),
                        "code": truncate_utf8(&item.label, MAX_LLM_FIELD_BYTES),
                        "path": item.path.as_deref().map(|path| truncate_utf8(path, MAX_LLM_FIELD_BYTES)),
                    })
                }))
                .collect::<Vec<_>>();
            serde_json::json!({
                "beat_id": beat.id,
                "intent": beat.intent,
                "symbolic_extract": truncate_utf8(&beat.narrative, MAX_LLM_FIELD_BYTES),
                "evidence": evidence,
            })
        })
        .collect::<Vec<_>>();
    let prompt = format!(
        "Rewrite each Story beat as concise explanatory prose using ONLY its supplied evidence. \
         Evidence lifecycle status is authoritative; never describe non-current evidence as current. \
         Return strict JSON {{\"beats\":[{{\"beat_id\":string,\"text\":string,\"citation_ids\":[string]}}]}}. \
         Include every beat once, in order; cite at least one supplied evidence id per beat. Evidence: {}",
        serde_json::to_string(&prompt_beats).unwrap_or_default()
    );
    (prompt.len() <= MAX_LLM_PROMPT_BYTES).then_some(prompt)
}

fn apply_narration_response(mut run: StoryRun, raw: &str) -> Option<StoryRun> {
    let parsed = serde_json::from_str::<NarrationResponse>(raw).ok()?;
    let eligible = run
        .beats
        .iter()
        .filter(|beat| !beat.evidence.is_empty() || !beat.code_anchors.is_empty())
        .collect::<Vec<_>>();
    if parsed.beats.len() != eligible.len() {
        return None;
    }
    let mut replacements = BTreeMap::new();
    for (expected, narrated) in eligible.iter().zip(parsed.beats) {
        if narrated.beat_id != expected.id
            || narrated.text.trim().is_empty()
            || narrated.text.len() > 1_200
            || narrated.citation_ids.is_empty()
        {
            return None;
        }
        let allowed = (0..expected.evidence.len())
            .map(|index| format!("e{index}"))
            .chain((0..expected.code_anchors.len()).map(|index| format!("c{index}")))
            .collect::<BTreeSet<_>>();
        let citations = narrated.citation_ids.iter().collect::<BTreeSet<_>>();
        if citations.len() != narrated.citation_ids.len()
            || narrated
                .citation_ids
                .iter()
                .any(|citation| !allowed.contains(citation))
        {
            return None;
        }
        replacements.insert(narrated.beat_id, narrated.text.trim().to_string());
    }
    for beat in &mut run.beats {
        if let Some(text) = replacements.remove(&beat.id) {
            beat.narrative = text;
        }
    }
    run.narration_mode = NarrationMode::Llm;
    Some(run)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarrationResponse {
    beats: Vec<NarratedBeat>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarratedBeat {
    beat_id: String,
    text: String,
    citation_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use oxigraph::model::{GraphName, NamedNode, Quad};

    struct StoryTestState {
        state: AppState,
        dir: PathBuf,
    }

    impl std::ops::Deref for StoryTestState {
        type Target = AppState;

        fn deref(&self) -> &Self::Target {
            &self.state
        }
    }

    impl Drop for StoryTestState {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn story_state(name: &str) -> StoryTestState {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-story-state-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let state = AppState::bootstrap(
            &dir,
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
        )
        .unwrap();
        StoryTestState { state, dir }
    }

    fn record_with_status(state: &AppState, title: &str, status: &str) -> String {
        let kind = "ArchitecturalDecision";
        crate::graph::record_instance(
            state,
            &crate::graph::RecordInput {
                class_iri: state.resolve_class(kind).unwrap(),
                class_local: kind.to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), title.to_string()),
                    (state.capture.title.clone(), title.to_string()),
                    (state.capture.status.clone(), status.to_string()),
                ],
            },
            "story-test",
            Utc::now(),
        )
        .unwrap()
    }

    fn link_successor(state: &AppState, old: &str, new: &str) {
        link_edge(state, old, "isSupersededBy", new);
    }

    fn link_edge(state: &AppState, subject: &str, predicate: &str, object: &str) {
        let quad = Quad::new(
            NamedNode::new(subject).unwrap(),
            NamedNode::new(state.resolve_object_property(predicate).unwrap()).unwrap(),
            NamedNode::new(object).unwrap(),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        );
        state.store.insert(&quad).unwrap();
    }

    fn type_entity(state: &AppState, iri: &str, class_iri: &str) {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(iri).unwrap(),
                NamedNode::new(moose::RDF_TYPE).unwrap(),
                NamedNode::new(class_iri).unwrap(),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
    }

    fn type_component(state: &AppState, iri: &str) {
        type_entity(state, iri, &state.resolve_class("SystemComponent").unwrap());
    }

    fn type_code_entity(state: &AppState, iri: &str, symbol: &str, label: &str) {
        let terms = CodeTerms::resolve(state).unwrap();
        type_entity(state, iri, &terms.code_entity_class);
        for (predicate, value) in [
            (terms.has_substrate_symbol, symbol),
            (terms.has_code_name, label),
        ] {
            state
                .store
                .insert(&Quad::new(
                    NamedNode::new(iri).unwrap(),
                    NamedNode::new(predicate).unwrap(),
                    oxigraph::model::Literal::new_simple_literal(value),
                    GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
                ))
                .unwrap();
        }
    }

    fn recipe(id: &str, status: StoryStatus, beats: usize) -> StoryRecipe {
        StoryRecipe {
            id: id.to_string(),
            title: "Graph store overview".to_string(),
            subject_component_iri: "https://example.test/components/graph".to_string(),
            goal: "Understand the graph store".to_string(),
            audience: "reboarding".to_string(),
            beats: (0..beats)
                .map(|index| StoryBeatRecipe {
                    id: format!("beat-{index}"),
                    title: format!("Beat {index}"),
                    intent: [
                        StoryIntent::Purpose,
                        StoryIntent::Boundary,
                        StoryIntent::CoreCode,
                        StoryIntent::Governance,
                        StoryIntent::Risk,
                    ][index]
                        .clone(),
                    record_iris: vec![],
                    code_symbols: vec![],
                    curator_note: None,
                })
                .collect(),
            status,
            curator: "maintainer".to_string(),
            updated_at: None,
        }
    }

    #[test]
    fn repository_round_trips_and_lists_recipe() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-repository-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        let saved = repository
            .save("graph-store", recipe("graph-store", StoryStatus::Draft, 1))
            .unwrap();
        assert!(saved.updated_at.is_some());
        assert_eq!(repository.get("graph-store").unwrap(), Some(saved));
        let recipes = repository.list_recipes().unwrap();
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].beats.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_quarantines_invalid_files_and_filename_id_mismatches() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-quarantine-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        repository
            .save("valid", recipe("valid", StoryStatus::Draft, 1))
            .unwrap();
        let stories = root.join("stories");
        std::fs::write(stories.join("broken.json"), b"{not-json").unwrap();
        let mismatched = serde_json::to_vec(&recipe("body-id", StoryStatus::Draft, 1)).unwrap();
        std::fs::write(stories.join("filename-id.json"), mismatched).unwrap();

        let loaded = repository.list_recipes().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "valid");
        assert!(repository.get("filename-id").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn repository_rejects_symlinked_directories_and_recipe_files() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "moosedev-story-symlink-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let project = base.join("project");
        let outside = base.join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, project.join("stories")).unwrap();
        let error = StoryRepository::new(&project).list_recipes().unwrap_err();
        assert!(error.downcast_ref::<StoryInternal>().is_some());

        let project_two = base.join("project-two");
        std::fs::create_dir_all(project_two.join("stories")).unwrap();
        let external_recipe = outside.join("linked.json");
        std::fs::write(
            &external_recipe,
            serde_json::to_vec(&recipe("linked", StoryStatus::Draft, 1)).unwrap(),
        )
        .unwrap();
        symlink(&external_recipe, project_two.join("stories/linked.json")).unwrap();
        let error = StoryRepository::new(&project_two)
            .get("linked")
            .unwrap_err();
        assert!(error.downcast_ref::<StoryInternal>().is_some());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stale_publish_conflicts_and_current_snapshot_publishes() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-publish-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        let mut validated = recipe("publish", StoryStatus::Draft, 3);
        for beat in &mut validated.beats {
            beat.record_iris
                .push("https://example.test/record".to_string());
        }
        let saved = repository.save("publish", validated).unwrap();

        let mut concurrent_disk_version = saved.clone();
        concurrent_disk_version.title = "Unvalidated concurrent title".to_string();
        let concurrent = repository.save("publish", concurrent_disk_version).unwrap();

        let conflict = repository
            .publish_checked("publish", saved.updated_at.as_deref().unwrap(), |_| Ok(()))
            .unwrap_err();
        assert!(conflict.downcast_ref::<StoryConflict>().is_some());

        let published = repository
            .publish_checked("publish", concurrent.updated_at.as_deref().unwrap(), |_| {
                Ok(())
            })
            .unwrap();
        assert_eq!(published.title, "Unvalidated concurrent title");
        assert_eq!(published.status, StoryStatus::Published);
        assert_eq!(repository.get("publish").unwrap(), Some(published));
        assert!(std::fs::read_dir(root.join("stories"))
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_writers_use_cas_and_leave_no_shared_temp_artifacts() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-writers-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for title in ["Writer A", "Writer B"] {
            let repository = StoryRepository::new(&root);
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut candidate = recipe("race", StoryStatus::Draft, 1);
                candidate.title = title.to_string();
                barrier.wait();
                repository.save("race", candidate)
            }));
        }
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome
                    .as_ref()
                    .is_err_and(|error| error.downcast_ref::<StoryConflict>().is_some()))
                .count(),
            1
        );
        assert!(std::fs::read_dir(root.join("stories"))
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_revisions_are_unique_and_monotonic() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-revisions-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        let first = repository
            .save("revision", recipe("revision", StoryStatus::Draft, 1))
            .unwrap();
        let mut update = first.clone();
        update.title = "Second revision".to_string();
        let second = repository.save("revision", update).unwrap();
        let first_revision = revision_value(first.updated_at.as_deref().unwrap()).unwrap();
        let second_revision = revision_value(second.updated_at.as_deref().unwrap()).unwrap();
        assert!(second_revision > first_revision);
        assert_ne!(first.updated_at, second.updated_at);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_allows_only_one_published_story_per_subject() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-unique-subject-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        let mut first = recipe("first", StoryStatus::Published, 3);
        let mut second = recipe("second", StoryStatus::Published, 3);
        for candidate in [&mut first, &mut second] {
            for beat in &mut candidate.beats {
                beat.record_iris
                    .push("https://example.test/record".to_string());
            }
        }
        repository.save("first", first).unwrap();
        let error = repository.save("second", second).unwrap_err();
        assert!(error.downcast_ref::<StoryConflict>().is_some());
        assert_eq!(repository.list_recipes().unwrap().len(), 1);

        // Legacy/manual duplicate files are detected at selection time rather
        // than whichever filename happens to be visited first winning.
        let mut duplicate = recipe("second", StoryStatus::Published, 3);
        for beat in &mut duplicate.beats {
            beat.record_iris
                .push("https://example.test/record".to_string());
        }
        duplicate.updated_at = Some(next_revision(None));
        std::fs::write(
            root.join("stories/second.json"),
            serde_json::to_vec_pretty(&duplicate).unwrap(),
        )
        .unwrap();
        assert!(repository
            .published_for_subject(&duplicate.subject_component_iri)
            .unwrap_err()
            .to_string()
            .contains("multiple published Stories"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_publish_allows_exactly_one_story_per_subject() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-concurrent-subject-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        let mut saved = Vec::new();
        for id in ["first", "second"] {
            let mut draft = recipe(id, StoryStatus::Draft, 3);
            for beat in &mut draft.beats {
                beat.record_iris
                    .push("https://example.test/record".to_string());
            }
            saved.push(repository.save(id, draft).unwrap());
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let outcomes = saved
            .into_iter()
            .map(|saved| {
                let repository = StoryRepository::new(&root);
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    repository.publish_checked(
                        &saved.id,
                        saved.updated_at.as_deref().unwrap(),
                        |_| Ok(()),
                    )
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome
                    .as_ref()
                    .is_err_and(|error| error.downcast_ref::<StoryConflict>().is_some()))
                .count(),
            1
        );
        assert_eq!(
            repository
                .list_recipes()
                .unwrap()
                .iter()
                .filter(|recipe| recipe.status == StoryStatus::Published)
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_save_conflicts_before_graph_dependent_validation() {
        let root = std::env::temp_dir().join(format!(
            "moosedev-story-save-check-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let repository = StoryRepository::new(&root);
        let stale = repository
            .save("checked", recipe("checked", StoryStatus::Draft, 1))
            .unwrap();
        let mut current = stale.clone();
        current.title = "Current".to_string();
        repository.save("checked", current).unwrap();

        let validation_calls = std::sync::atomic::AtomicUsize::new(0);
        let error = repository
            .save_checked("checked", stale, |_| {
                validation_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap_err();
        assert!(error.downcast_ref::<StoryConflict>().is_some());
        assert_eq!(validation_calls.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn superseded_recipe_anchor_follows_chain_to_current_successor() {
        let state = story_state("supersession-chain");
        let old = record_with_status(&state, "Old", "superseded");
        let middle = record_with_status(&state, "Middle", "superseded");
        let current = record_with_status(&state, "Current", "accepted");
        link_successor(&state, &old, &middle);
        link_successor(&state, &old, "https://example.test/000-dead-branch");
        link_successor(&state, &middle, &current);

        match resolve_recipe_record(&state, &old).unwrap() {
            AnchorResolution::Superseded { replacements } => {
                assert_eq!(replacements[0].iri, current)
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        let competing = record_with_status(&state, "Competing current", "accepted");
        link_successor(&state, &old, &competing);
        match resolve_recipe_record(&state, &old).unwrap() {
            AnchorResolution::Superseded { replacements } => {
                assert_eq!(replacements.len(), 2);
                assert_eq!(
                    replacements
                        .iter()
                        .map(|replacement| replacement.iri.as_str())
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from([current.as_str(), competing.as_str()])
                );
            }
            other => panic!("unexpected resolution: {other:?}"),
        }

        let legacy_old = record_with_status(&state, "Legacy old", "superseded");
        let legacy_current = record_with_status(&state, "Legacy current", "accepted");
        let status = NamedNode::new(&state.capture.status).unwrap();
        let legacy_node = NamedNode::new(&legacy_current).unwrap();
        let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap();
        let status_quads = state
            .store
            .quads_for_pattern(
                Some(legacy_node.as_ref().into()),
                Some(status.as_ref()),
                None,
                Some(GraphNameRef::NamedNode(graph.as_ref())),
            )
            .flatten()
            .collect::<Vec<_>>();
        for quad in &status_quads {
            state.store.remove(quad).unwrap();
        }
        link_successor(&state, &legacy_old, &legacy_current);
        match resolve_recipe_record(&state, &legacy_old).unwrap() {
            AnchorResolution::Superseded { replacements } => {
                let replacement = &replacements[0];
                assert_eq!(replacement.iri, legacy_current);
                assert_eq!(replacement.status, "unknown");
            }
            other => panic!("unexpected legacy resolution: {other:?}"),
        }

        let orphan = record_with_status(&state, "Orphan", "superseded");
        assert!(matches!(
            resolve_recipe_record(&state, &orphan).unwrap(),
            AnchorResolution::Superseded { replacements } if replacements.is_empty()
        ));
        let recipe = StoryRecipe {
            id: "orphan".to_string(),
            title: "Orphan".to_string(),
            subject_component_iri: "https://example.test/component".to_string(),
            goal: "See the gap".to_string(),
            audience: "reboarding".to_string(),
            beats: vec![StoryBeatRecipe {
                id: "governance".to_string(),
                title: "Governance".to_string(),
                intent: StoryIntent::Governance,
                record_iris: vec![orphan],
                code_symbols: vec![],
                curator_note: None,
            }],
            status: StoryStatus::Draft,
            curator: "tester".to_string(),
            updated_at: None,
        };
        let code = all_code_by_symbol(&state).unwrap();
        let (_, gaps) = recipe_beats(
            &state,
            &recipe,
            &StoryCandidate {
                iri: recipe.subject_component_iri.clone(),
                label: "Component".to_string(),
                description: None,
            },
            &code,
        )
        .unwrap();
        assert!(gaps[0]
            .detail
            .contains("no current successor can be resolved"));
    }

    #[test]
    fn inverse_concerns_is_evidence_while_proposed_knowledge_is_only_a_gap_signal() {
        let state = story_state("inverse-concerns");
        let component = "https://example.test/component";
        let accepted = record_with_status(&state, "Accepted evidence", "accepted");
        let proposed = record_with_status(&state, "Proposed evidence", "proposed");
        let proposed_component = crate::graph::record_instance(
            &state,
            &crate::graph::RecordInput {
                class_iri: state.resolve_class("SystemComponent").unwrap(),
                class_local: "SystemComponent".to_string(),
                properties: vec![
                    (
                        moose::RDFS_LABEL.to_string(),
                        "Proposed component".to_string(),
                    ),
                    (state.capture.status.clone(), "proposed".to_string()),
                ],
            },
            "story-test",
            Utc::now(),
        )
        .unwrap();
        let predicate =
            NamedNode::new(state.resolve_object_property("isConcernedBy").unwrap()).unwrap();
        for record in [&accepted, &proposed, &proposed_component] {
            state
                .store
                .insert(&Quad::new(
                    NamedNode::new(component).unwrap(),
                    predicate.clone(),
                    NamedNode::new(record).unwrap(),
                    GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
                ))
                .unwrap();
        }

        let records = component_records(&state, component).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].evidence.iri, accepted);
        assert_eq!(
            pending_component_record_count(&state, component).unwrap(),
            1
        );
        let index = StoryResolutionIndex::build(&state).unwrap();
        assert!(!index.components_by_iri.contains_key(&proposed_component));
        assert_eq!(
            index.recipe_subject(&proposed_component).label,
            "Proposed component"
        );
    }

    #[test]
    fn generated_boundary_round_trips_as_intrinsic_subject_evidence() {
        let state = story_state("boundary-round-trip");
        let component_iri = "https://example.test/component/boundary";
        type_component(&state, component_iri);
        let component = StoryCandidate {
            iri: component_iri.to_string(),
            label: "Boundary component".to_string(),
            description: Some("Owns src/boundary/".to_string()),
        };
        let (generated, _) = generated_beats(&component, "unknown", &[], &[]);
        let generated_boundary = generated
            .iter()
            .find(|beat| beat.intent == StoryIntent::Boundary)
            .unwrap();
        let draft = StoryRecipe {
            id: "boundary-round-trip".to_string(),
            title: "Boundary round trip".to_string(),
            subject_component_iri: component_iri.to_string(),
            goal: "Preserve the boundary".to_string(),
            audience: "reboarding".to_string(),
            beats: generated
                .iter()
                .map(|beat| StoryBeatRecipe {
                    id: beat.id.clone(),
                    title: beat.title.clone(),
                    intent: beat.intent.clone(),
                    record_iris: beat
                        .evidence
                        .iter()
                        .map(|evidence| evidence.iri.clone())
                        .collect(),
                    code_symbols: beat
                        .code_anchors
                        .iter()
                        .map(|anchor| anchor.symbol.clone())
                        .collect(),
                    curator_note: None,
                })
                .collect(),
            status: StoryStatus::Draft,
            curator: "tester".to_string(),
            updated_at: None,
        };
        let repository = StoryRepository::new(&state.dir);
        let saved = repository.save("boundary-round-trip", draft).unwrap();
        let saved_boundary = saved
            .beats
            .iter()
            .find(|beat| beat.intent == StoryIntent::Boundary)
            .unwrap();
        assert!(saved_boundary.record_iris.is_empty());

        let code = all_code_by_symbol(&state).unwrap();
        let (reloaded, gaps) = recipe_beats(&state, &saved, &component, &code).unwrap();
        let reloaded_boundary = reloaded
            .iter()
            .find(|beat| beat.intent == StoryIntent::Boundary)
            .unwrap();
        assert_eq!(reloaded_boundary.evidence, generated_boundary.evidence);
        assert_eq!(reloaded_boundary.narrative, generated_boundary.narrative);
        assert!(!gaps
            .iter()
            .any(|gap| gap.beat_intent == Some(StoryIntent::Boundary)));
    }

    #[test]
    fn generated_evidence_requires_canonical_record_and_code_types() {
        let state = story_state("canonical-evidence-types");
        let component = "https://example.test/component/typed";
        let impostor_record = "https://example.test/component/impostor";
        let untyped_code = "https://example.test/code/untyped";
        let typed_code = "https://example.test/code/typed";
        type_component(&state, component);
        type_component(&state, impostor_record);
        link_edge(&state, impostor_record, "concerns", component);

        let accepted_record = record_with_status(&state, "Accepted record", "accepted");
        link_edge(&state, &accepted_record, "concerns", component);

        let terms = CodeTerms::resolve(&state).unwrap();
        for (entity, symbol) in [
            (untyped_code, "untyped-symbol"),
            (typed_code, "typed-symbol"),
        ] {
            state
                .store
                .insert(&Quad::new(
                    NamedNode::new(entity).unwrap(),
                    NamedNode::new(&terms.has_substrate_symbol).unwrap(),
                    oxigraph::model::Literal::new_simple_literal(symbol),
                    GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
                ))
                .unwrap();
            link_edge(&state, entity, "realizes", component);
        }
        type_code_entity(&state, typed_code, "typed-symbol", "Typed code");

        let records = component_records(&state, component).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.evidence.iri.as_str())
                .collect::<Vec<_>>(),
            vec![accepted_record.as_str()]
        );
        let code = component_code(&state, component).unwrap();
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].entity_iri.as_deref(), Some(typed_code));
        assert!(!all_code_by_symbol(&state)
            .unwrap()
            .contains_key("untyped-symbol"));
    }

    #[test]
    fn presentation_only_generation_preserves_structure_without_issuing_grants() {
        let state = story_state("presentation-only-checks");
        let target = "https://example.test/component/target";
        let other = "https://example.test/component/other";
        type_component(&state, target);
        type_component(&state, other);
        let correct = record_with_status(&state, "Correct evidence", "accepted");
        let distractor = record_with_status(&state, "Other evidence", "accepted");
        link_edge(&state, &correct, "concerns", target);
        link_edge(&state, &distractor, "concerns", other);
        let component = StoryCandidate {
            iri: target.to_string(),
            label: "Target".to_string(),
            description: None,
        };
        let index = StoryResolutionIndex::build(&state).unwrap();
        let symbolic =
            generate_symbolic_with_index(&state, &index, &component, None, true).unwrap();
        assert_eq!(symbolic.checks.len(), 1);
        let grants_after_symbolic = state.story_checks.lock().unwrap().grants.len();

        let presentation =
            generate_symbolic_with_index(&state, &index, &component, None, false).unwrap();
        assert!(presentation.checks.is_empty());
        assert_eq!(presentation.gaps, symbolic.gaps);
        assert_eq!(
            state.story_checks.lock().unwrap().grants.len(),
            grants_after_symbolic
        );

        let empty_component = StoryCandidate {
            iri: other.to_string(),
            label: "Other".to_string(),
            description: None,
        };
        let empty_index = StoryResolutionIndex::build(&state).unwrap();
        let sparse = generate_symbolic_with_index(
            &state,
            &empty_index,
            &empty_component,
            Some(&StoryRecipe {
                id: "sparse".to_string(),
                title: "Sparse".to_string(),
                subject_component_iri: other.to_string(),
                goal: "Show gaps".to_string(),
                audience: "reboarding".to_string(),
                beats: vec![StoryBeatRecipe {
                    id: "boundary".to_string(),
                    title: "Boundary".to_string(),
                    intent: StoryIntent::Boundary,
                    record_iris: vec![],
                    code_symbols: vec![],
                    curator_note: None,
                }],
                status: StoryStatus::Draft,
                curator: "tester".to_string(),
                updated_at: None,
            }),
            true,
        )
        .unwrap();
        assert!(sparse.checks.is_empty());
        assert!(sparse.gaps.iter().any(|gap| gap.id == "checks-unavailable"));
    }

    #[test]
    fn drifted_subjects_remain_readable_but_never_issue_checks() {
        let state = story_state("drifted-subject-no-checks");
        let target = "https://example.test/component/retired";
        let other = "https://example.test/component/current";
        type_component(&state, target);
        type_component(&state, other);
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(target).unwrap(),
                NamedNode::new(&state.capture.status).unwrap(),
                oxigraph::model::Literal::new_simple_literal("deprecated"),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
        let correct = record_with_status(&state, "Retired subject evidence", "accepted");
        let distractor = record_with_status(&state, "Current subject evidence", "accepted");
        link_edge(&state, &correct, "concerns", target);
        link_edge(&state, &distractor, "concerns", other);

        let index = StoryResolutionIndex::build(&state).unwrap();
        let run = generate_symbolic_with_index(
            &state,
            &index,
            &StoryCandidate {
                iri: target.to_string(),
                label: "Retired component".to_string(),
                description: None,
            },
            None,
            true,
        )
        .unwrap();

        assert!(run.gaps.iter().any(|gap| gap.id == "subject-drift"));
        assert!(run.gaps.iter().any(|gap| gap.id == "checks-unavailable"));
        assert!(run.checks.is_empty());
        assert!(state.story_checks.lock().unwrap().grants.is_empty());
    }

    #[test]
    fn curated_code_is_subject_grounded_and_check_uses_displayed_anchor() {
        let state = story_state("curated-code-grounding");
        let component_a = "https://example.test/component/a";
        let component_b = "https://example.test/component/b";
        let code_a = "https://example.test/code/a";
        let code_b = "https://example.test/code/b";
        type_component(&state, component_a);
        type_component(&state, component_b);
        type_code_entity(&state, code_a, "symbol-a", "Code A");
        type_code_entity(&state, code_b, "symbol-b", "Code B");
        link_edge(&state, code_a, "realizes", component_a);
        link_edge(&state, code_b, "realizes", component_b);
        let anchor = |symbol: &str, label: &str, iri: &str| StoryCodeAnchor {
            symbol: symbol.to_string(),
            label: label.to_string(),
            entity_iri: Some(iri.to_string()),
            path: None,
            line: None,
        };
        let anchor_a = anchor("symbol-a", "Code A", code_a);
        let anchor_b = anchor("symbol-b", "Code B", code_b);
        let code_by_symbol = BTreeMap::from([
            (anchor_a.symbol.clone(), anchor_a.clone()),
            (anchor_b.symbol.clone(), anchor_b.clone()),
        ]);
        let curated = StoryRecipe {
            id: "curated-code".to_string(),
            title: "Curated code".to_string(),
            subject_component_iri: component_a.to_string(),
            goal: "Ground code".to_string(),
            audience: "reboarding".to_string(),
            beats: vec![StoryBeatRecipe {
                id: "core-code".to_string(),
                title: "Core code".to_string(),
                intent: StoryIntent::CoreCode,
                record_iris: vec![],
                code_symbols: vec![anchor_b.symbol.clone()],
                curator_note: None,
            }],
            status: StoryStatus::Draft,
            curator: "tester".to_string(),
            updated_at: None,
        };
        let (beats, gaps) = recipe_beats(
            &state,
            &curated,
            &StoryCandidate {
                iri: component_a.to_string(),
                label: "Component A".to_string(),
                description: None,
            },
            &code_by_symbol,
        )
        .unwrap();
        assert!(beats[0].code_anchors.is_empty());
        assert!(gaps
            .iter()
            .any(|gap| gap.title == "Story code does not realize this subject"));

        let displayed = vec![make_beat(
            "core-code",
            "Core code",
            StoryIntent::CoreCode,
            vec![],
            vec![anchor_b],
            None,
            None,
        )];
        let index = StoryResolutionIndex {
            components: vec![],
            components_by_iri: BTreeMap::new(),
            known_components_by_iri: BTreeMap::new(),
            code_entities: code_by_symbol.values().cloned().collect(),
            code_by_symbol,
        };
        let (checks, viable) = build_checks(
            &state,
            &StoryCandidate {
                iri: component_b.to_string(),
                label: "Component B".to_string(),
                description: None,
            },
            &displayed,
            &index,
            &[],
            &[anchor("symbol-b", "Code B", code_b)],
            true,
        )
        .unwrap();
        assert_eq!(viable, 1);
        assert_eq!(checks.len(), 1);
        let labels = checks[0]
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(labels, BTreeSet::from(["Code A", "Code B"]));
    }

    #[test]
    fn multiply_typed_kind_and_duplicate_symbol_selection_are_deterministic() {
        assert_eq!(
            choose_record_kind(vec![
                "InformationRecord".to_string(),
                "Constraint".to_string(),
                "ArchitecturalDecision".to_string(),
            ]),
            Some("ArchitecturalDecision".to_string())
        );
        let anchor = |iri: &str, label: &str| StoryCodeAnchor {
            symbol: "same-symbol".to_string(),
            label: label.to_string(),
            entity_iri: Some(iri.to_string()),
            path: None,
            line: None,
        };
        let first =
            dedupe_code_anchors(vec![anchor("urn:z", "A label"), anchor("urn:a", "Z label")]);
        let reversed =
            dedupe_code_anchors(vec![anchor("urn:a", "Z label"), anchor("urn:z", "A label")]);
        assert_eq!(first, reversed);
        assert_eq!(first[0].entity_iri.as_deref(), Some("urn:a"));

        let mut forward_map = BTreeMap::new();
        insert_code_anchor(&mut forward_map, anchor("urn:z", "A label"));
        insert_code_anchor(&mut forward_map, anchor("urn:a", "Z label"));
        let mut reverse_map = BTreeMap::new();
        insert_code_anchor(&mut reverse_map, anchor("urn:a", "Z label"));
        insert_code_anchor(&mut reverse_map, anchor("urn:z", "A label"));
        assert_eq!(forward_map, reverse_map);
        assert_eq!(
            forward_map["same-symbol"].entity_iri.as_deref(),
            Some("urn:a")
        );
    }

    #[test]
    fn published_recipes_require_three_to_five_beats() {
        let error = validate_recipe(&recipe("short", StoryStatus::Published, 2), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("3 to 5"));
        let mut valid = recipe("valid", StoryStatus::Published, 5);
        for beat in &mut valid.beats {
            beat.record_iris
                .push("https://example.test/record".to_string());
        }
        assert!(validate_recipe(&valid, true).is_ok());

        let mut duplicated = valid.clone();
        duplicated.beats[1].intent = StoryIntent::Purpose;
        assert!(validate_recipe(&duplicated, true)
            .unwrap_err()
            .to_string()
            .contains("intent only once"));

        let mut out_of_order = valid;
        out_of_order.beats.swap(0, 1);
        assert!(validate_recipe(&out_of_order, true)
            .unwrap_err()
            .to_string()
            .contains("must follow"));
    }

    #[test]
    fn beat_reference_limit_accepts_six_and_rejects_seven_or_duplicates() {
        let six = (0..6)
            .map(|index| format!("ref-{index}"))
            .collect::<Vec<_>>();
        assert!(validate_refs("record IRI", &six).is_ok());

        let seven = (0..7)
            .map(|index| format!("ref-{index}"))
            .collect::<Vec<_>>();
        assert!(validate_refs("record IRI", &seven)
            .unwrap_err()
            .to_string()
            .contains("at most 6"));

        assert!(
            validate_refs("record IRI", &["same".to_string(), "same".to_string()])
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn recipe_ids_cannot_escape_story_directory() {
        let error = validate_recipe(&recipe("../escape", StoryStatus::Draft, 1), false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid Story id"));
    }

    #[test]
    fn curator_note_stays_separate_from_narration_and_utf8_truncation_is_bounded() {
        let note = "A curator's bridge";
        let beat = make_beat(
            "boundary",
            "Boundary",
            StoryIntent::Boundary,
            vec![StoryEvidence {
                iri: "record".to_string(),
                title: "Component boundary".to_string(),
                kind: "Constraint".to_string(),
                status: "accepted".to_string(),
            }],
            vec![],
            None,
            Some(note.to_string()),
        );
        assert_eq!(beat.curator_note.as_deref(), Some(note));
        assert!(!beat.narrative.contains(note));

        let unicode = "🫎".repeat(100);
        let clipped = truncate_utf8(&unicode, 320);
        assert!(clipped.len() <= 320);
        assert!(clipped.is_char_boundary(clipped.len()));
        assert_eq!(clipped.chars().count(), 80);
    }

    #[test]
    fn comprehension_checks_require_two_unique_options() {
        let mut one = vec![StoryCheckOption {
            id: "same".to_string(),
            label: "First".to_string(),
        }];
        assert!(!nontrivial_options(&mut one));

        let mut duplicated = vec![
            StoryCheckOption {
                id: "same".to_string(),
                label: "First".to_string(),
            },
            StoryCheckOption {
                id: "same".to_string(),
                label: "Duplicate".to_string(),
            },
        ];
        assert!(!nontrivial_options(&mut duplicated));
        assert_eq!(duplicated.len(), 1);

        let mut same_visible_label = vec![
            StoryCheckOption {
                id: "one".to_string(),
                label: "  Same   Answer ".to_string(),
            },
            StoryCheckOption {
                id: "two".to_string(),
                label: "same answer".to_string(),
            },
        ];
        assert!(!nontrivial_options(&mut same_visible_label));
        assert_eq!(same_visible_label.len(), 1);

        let mut duplicate_then_unique = vec![
            StoryCheckOption {
                id: "correct".to_string(),
                label: "Correct answer".to_string(),
            },
            StoryCheckOption {
                id: "duplicate".to_string(),
                label: " correct   ANSWER ".to_string(),
            },
            StoryCheckOption {
                id: "first-unique".to_string(),
                label: "First distractor".to_string(),
            },
            StoryCheckOption {
                id: "second-unique".to_string(),
                label: "Second distractor".to_string(),
            },
            StoryCheckOption {
                id: "third-unique".to_string(),
                label: "Third distractor".to_string(),
            },
        ];
        assert!(nontrivial_options(&mut duplicate_then_unique));
        assert_eq!(duplicate_then_unique.len(), MAX_CHECK_OPTIONS);
        assert_eq!(duplicate_then_unique[0].id, "correct");
        assert_eq!(duplicate_then_unique[1].id, "first-unique");
        assert_eq!(duplicate_then_unique[2].id, "second-unique");

        let mut two = vec![
            StoryCheckOption {
                id: "a".to_string(),
                label: "A".to_string(),
            },
            StoryCheckOption {
                id: "b".to_string(),
                label: "B".to_string(),
            },
        ];
        assert!(nontrivial_options(&mut two));
    }

    #[test]
    fn check_options_reject_mixed_truth_labels_and_cap_after_deduplication() {
        let fact = |id: &str, label: &str, matches_target| CheckOptionFact {
            id: id.to_string(),
            label: label.to_string(),
            matches_target,
        };
        let displayed = vec!["correct".to_string()];
        let (_, options) = unambiguous_check_options(
            &displayed,
            vec![
                fact("correct", "Correct", true),
                fact("also-valid", "Shared answer", true),
                fact("looks-valid", " shared   ANSWER ", false),
                fact("duplicate-a", "Duplicate", false),
                fact("duplicate-b", " duplicate ", false),
                fact("first", "First distractor", false),
                fact("second", "Second distractor", false),
                fact("third", "Third distractor", false),
            ],
        )
        .expect("two unambiguous distractor labels remain");
        assert_eq!(
            options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            vec!["correct", "duplicate-a", "first"]
        );
        assert!(!options.iter().any(|option| option.label == "Shared answer"));

        let ambiguous_only = unambiguous_check_options(
            &displayed,
            vec![
                fact("correct", "Correct", true),
                fact("true-shared", "Shared", true),
                fact("false-shared", " shared ", false),
            ],
        );
        assert!(ambiguous_only.is_none());

        let mixed_correct_label = unambiguous_check_options(
            &displayed,
            vec![
                fact("correct", "Shared", true),
                fact("looks-correct", " shared ", false),
                fact("other", "Other", false),
            ],
        );
        assert!(mixed_correct_label.is_none());
    }

    #[test]
    fn opaque_option_order_has_no_fixed_answer_position() {
        let source = || {
            vec![
                StoryCheckOption {
                    id: "correct-entity".to_string(),
                    label: "Correct".to_string(),
                },
                StoryCheckOption {
                    id: "other-entity".to_string(),
                    label: "Other".to_string(),
                },
            ]
        };
        let mut first_keys = [1_u128, 40, 2, 10].into_iter().map(uuid::Uuid::from_u128);
        let (correct_token, other_first, option_entities) =
            opaque_options(source(), "correct-entity", || first_keys.next().unwrap());
        assert_eq!(other_first[1].id, correct_token);
        assert_eq!(option_entities[&correct_token], "correct-entity");
        assert!(other_first
            .iter()
            .all(|option| { option.id != "correct-entity" && option.id != "other-entity" }));

        let mut second_keys = [1_u128, 10, 2, 40].into_iter().map(uuid::Uuid::from_u128);
        let (correct_token, correct_first, _) =
            opaque_options(source(), "correct-entity", || second_keys.next().unwrap());
        assert_eq!(correct_first[0].id, correct_token);
    }

    #[test]
    fn check_grants_are_project_scoped_and_distinguish_error_states() {
        let state = story_state("check-registry-a");
        let other_state = story_state("check-registry-b");
        let allowed = "allowed-token".to_string();
        let handle = register_check(
            &state,
            CheckGrant {
                kind: CheckKind::Concerns,
                component_iri: "https://example.test/component".to_string(),
                beat_id: "purpose".to_string(),
                correct_option_token: allowed.clone(),
                option_entities: BTreeMap::from([(
                    allowed.clone(),
                    "https://example.test/record".to_string(),
                )]),
                correct_entity_iri: "https://example.test/record".to_string(),
                expires_at: std::time::Instant::now() + CHECK_TTL,
            },
        );
        assert_eq!(
            grade_check(&other_state, &handle, std::slice::from_ref(&allowed))
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::Unknown)
        );
        assert_eq!(
            grade_check(&state, &handle, &["foreign".to_string()])
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::ForeignOption)
        );
        assert_eq!(
            grade_check(&state, &handle, &[allowed])
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::Stale)
        );

        let expired = register_check(
            &state,
            CheckGrant {
                kind: CheckKind::Concerns,
                component_iri: "https://example.test/component".to_string(),
                beat_id: "purpose".to_string(),
                correct_option_token: "token".to_string(),
                option_entities: BTreeMap::from([(
                    "token".to_string(),
                    "https://example.test/record".to_string(),
                )]),
                correct_entity_iri: "https://example.test/record".to_string(),
                expires_at: std::time::Instant::now(),
            },
        );
        assert_eq!(
            grade_check(&state, &expired, &[])
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::Expired)
        );
    }

    #[test]
    fn grading_rejects_retired_subjects_and_newly_valid_distractors() {
        let state = story_state("check-current-graph");
        let component = "https://example.test/component/check";
        type_component(&state, component);
        let correct = record_with_status(&state, "Correct", "accepted");
        let distractor = record_with_status(&state, "Distractor", "accepted");
        link_edge(&state, &correct, "concerns", component);

        let grant = || CheckGrant {
            kind: CheckKind::Concerns,
            component_iri: component.to_string(),
            beat_id: "purpose".to_string(),
            correct_option_token: "correct-token".to_string(),
            option_entities: BTreeMap::from([
                ("correct-token".to_string(), correct.clone()),
                ("distractor-token".to_string(), distractor.clone()),
            ]),
            correct_entity_iri: correct.clone(),
            expires_at: std::time::Instant::now() + CHECK_TTL,
        };
        let distractor_handle = register_check(&state, grant());
        assert!(
            grade_check(&state, &distractor_handle, &["correct-token".to_string()])
                .unwrap()
                .correct
        );
        link_edge(&state, &distractor, "concerns", component);
        assert_eq!(
            grade_check(&state, &distractor_handle, &["correct-token".to_string()])
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::Stale)
        );

        let other_component = "https://example.test/component/retired";
        type_component(&state, other_component);
        let other_record = record_with_status(&state, "Other correct", "accepted");
        link_edge(&state, &other_record, "concerns", other_component);
        let retired_handle = register_check(
            &state,
            CheckGrant {
                kind: CheckKind::Concerns,
                component_iri: other_component.to_string(),
                beat_id: "purpose".to_string(),
                correct_option_token: "only-token".to_string(),
                option_entities: BTreeMap::from([("only-token".to_string(), other_record.clone())]),
                correct_entity_iri: other_record,
                expires_at: std::time::Instant::now() + CHECK_TTL,
            },
        );
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(other_component).unwrap(),
                NamedNode::new(&state.capture.status).unwrap(),
                oxigraph::model::Literal::new_simple_literal("deprecated"),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
        assert_eq!(
            grade_check(&state, &retired_handle, &["only-token".to_string()])
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::Stale)
        );
    }

    #[test]
    fn capacity_evicted_check_reports_unavailable() {
        let state = story_state("check-capacity");
        let grant = || CheckGrant {
            kind: CheckKind::Concerns,
            component_iri: "https://example.test/component".to_string(),
            beat_id: "purpose".to_string(),
            correct_option_token: "token".to_string(),
            option_entities: BTreeMap::from([(
                "token".to_string(),
                "https://example.test/record".to_string(),
            )]),
            correct_entity_iri: "https://example.test/record".to_string(),
            expires_at: std::time::Instant::now() + CHECK_TTL,
        };
        let evicted = register_check(&state, grant());
        for _ in 0..MAX_CHECK_GRANTS {
            register_check(&state, grant());
        }
        assert_eq!(
            grade_check(&state, &evicted, &["token".to_string()])
                .unwrap_err()
                .downcast_ref::<StoryCheckError>(),
            Some(&StoryCheckError::Evicted)
        );
    }

    fn symbolic_run_for_narration() -> StoryRun {
        StoryRun {
            recipe_id: None,
            trust_state: StoryTrustState::Generated,
            narration_mode: NarrationMode::Symbolic,
            title: "A Story".to_string(),
            subject: StorySubject {
                iri: "component".to_string(),
                label: "Component".to_string(),
            },
            goal: "Understand it".to_string(),
            overview: "Overview".to_string(),
            beats: vec![
                StoryBeat {
                    id: "purpose".to_string(),
                    title: "Purpose".to_string(),
                    intent: StoryIntent::Purpose,
                    narrative: "Symbolic purpose".to_string(),
                    evidence: vec![StoryEvidence {
                        iri: "record".to_string(),
                        title: "Requirement".to_string(),
                        kind: "Requirement".to_string(),
                        status: "accepted".to_string(),
                    }],
                    code_anchors: vec![],
                    curator_note: Some("Keep this note".to_string()),
                    gap: None,
                },
                StoryBeat {
                    id: "code".to_string(),
                    title: "Code".to_string(),
                    intent: StoryIntent::CoreCode,
                    narrative: "Symbolic code".to_string(),
                    evidence: vec![],
                    code_anchors: vec![StoryCodeAnchor {
                        symbol: "symbol".to_string(),
                        label: "function".to_string(),
                        entity_iri: Some("entity".to_string()),
                        path: Some("src/lib.rs".to_string()),
                        line: None,
                    }],
                    curator_note: None,
                    gap: None,
                },
            ],
            gaps: vec![],
            checks: vec![],
        }
    }

    #[test]
    fn valid_narration_changes_only_grounded_prose_and_mode() {
        let symbolic = symbolic_run_for_narration();
        let raw = r#"{"beats":[{"beat_id":"purpose","text":"Narrated purpose","citation_ids":["e0"]},{"beat_id":"code","text":"Narrated code","citation_ids":["c0"]}]}"#;
        let narrated = apply_narration_response(symbolic.clone(), raw).unwrap();
        let mut expected = symbolic;
        expected.narration_mode = NarrationMode::Llm;
        expected.beats[0].narrative = "Narrated purpose".to_string();
        expected.beats[1].narrative = "Narrated code".to_string();
        assert_eq!(narrated, expected);
    }

    #[test]
    fn narration_prompt_bounds_individual_fields_and_total_input() {
        let state = story_state("narration-prompt-bounds");
        let mut run = symbolic_run_for_narration();
        run.beats[0].evidence[0].title = "x".repeat(MAX_LLM_FIELD_BYTES * 4);
        run.beats[0].narrative = "Owns src/boundary/ through coversPath".to_string();
        run.beats[0].curator_note = Some("private curator guidance".to_string());
        let prompt = build_narration_prompt(&state, &[&run.beats[0]]).unwrap();
        assert!(prompt.len() <= MAX_LLM_PROMPT_BYTES);
        assert!(!prompt.contains(&"x".repeat(MAX_LLM_FIELD_BYTES + 1)));
        assert!(prompt.contains("Owns src/boundary/ through coversPath"));
        assert!(prompt.contains(r#""status":"accepted""#));
        assert!(!prompt.contains("private curator guidance"));

        let huge = "y".repeat(MAX_LLM_FIELD_BYTES * 4);
        let template = StoryBeat {
            id: "beat".to_string(),
            title: "Beat".to_string(),
            intent: StoryIntent::Governance,
            narrative: "Symbolic".to_string(),
            evidence: (0..MAX_ANCHORS_PER_BEAT)
                .map(|index| StoryEvidence {
                    iri: format!("https://example.test/record/{index}"),
                    title: huge.clone(),
                    kind: huge.clone(),
                    status: "accepted".to_string(),
                })
                .collect(),
            code_anchors: (0..MAX_ANCHORS_PER_BEAT)
                .map(|index| StoryCodeAnchor {
                    symbol: format!("symbol-{index}"),
                    label: huge.clone(),
                    entity_iri: Some(format!("https://example.test/code/{index}")),
                    path: Some(huge.clone()),
                    line: None,
                })
                .collect(),
            curator_note: Some("not sent to the sensor".to_string()),
            gap: None,
        };
        let beats = (0..MAX_BEATS)
            .map(|index| {
                let mut beat = template.clone();
                beat.id = format!("beat-{index}");
                beat
            })
            .collect::<Vec<_>>();
        let eligible = beats.iter().collect::<Vec<_>>();
        assert!(build_narration_prompt(&state, &eligible).is_none());
    }

    #[test]
    fn narration_rejects_drifted_or_noncurrent_evidence() {
        let mut run = symbolic_run_for_narration();
        assert!(narration_evidence_is_current(&run));

        run.beats[0].evidence[0].status = "superseded".to_string();
        assert!(!narration_evidence_is_current(&run));

        run.beats[0].evidence[0].status = "accepted".to_string();
        run.gaps.push(StoryGap {
            id: "subject-drift".to_string(),
            title: "Story subject is unresolved".to_string(),
            detail: "The subject is no longer current.".to_string(),
            beat_intent: None,
        });
        assert!(!narration_evidence_is_current(&run));
    }

    #[test]
    fn invalid_narration_preserves_the_identical_symbolic_run() {
        let symbolic = symbolic_run_for_narration();
        let invalid = [
            "not json",
            r#"{"unknown":true,"beats":[]}"#,
            r#"{"beats":[{"beat_id":"purpose","text":"x","citation_ids":["e0","e0"]},{"beat_id":"code","text":"y","citation_ids":["c0"]}]}"#,
            r#"{"beats":[{"beat_id":"purpose","text":"x","citation_ids":["c0"]},{"beat_id":"code","text":"y","citation_ids":["c0"]}]}"#,
            r#"{"beats":[{"beat_id":"code","text":"y","citation_ids":["c0"]},{"beat_id":"purpose","text":"x","citation_ids":["e0"]}]}"#,
        ];
        for raw in invalid {
            let actual =
                apply_narration_response(symbolic.clone(), raw).unwrap_or_else(|| symbolic.clone());
            assert_eq!(actual, symbolic, "response should fall back: {raw}");
        }
    }
}
