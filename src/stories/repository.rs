//! Versioned, file-backed Story recipe persistence and validation.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use oxigraph::model::NamedNode;

use super::model::{validate_topic, StoryFocus, StoryRecipe, StoryRecipeSubject, StoryStatus};

pub(super) const MAX_FOCUS_REFS: usize = 128;
pub(super) const STORY_SCHEMA_VERSION: u8 = 3;

/// File-backed recipe repository. Claims stay in the KG; recipes contain only
/// presentation metadata and stable references.
#[derive(Debug, Clone)]
pub struct StoryRepository {
    project_root: PathBuf,
    root: PathBuf,
}

// Repository values are cheap and recreated by handlers. A process-wide lock
// keeps CAS writes and the list-based single-published-subject check atomic.
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

#[derive(Debug)]
pub struct StorySubjectInvalid(pub String);

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

impl_story_error!(StoryConflict);
impl_story_error!(StoryNotFound);
impl_story_error!(StoryCorrupt);
impl_story_error!(StoryInternal);
impl_story_error!(StorySubjectInvalid);

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
        validate_story_id(id)?;
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
        validate_story_id(route_id)?;
        if recipe.schema_version == 0 || recipe.schema_version > STORY_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported Story schema version {}; this server supports v1 through v{STORY_SCHEMA_VERSION}",
                recipe.schema_version
            );
        }
        if recipe.schema_version == STORY_SCHEMA_VERSION && !recipe.beats.is_empty() {
            anyhow::bail!("Story v3 recipes use focus metadata and must not contain legacy beats");
        }
        if recipe.schema_version == STORY_SCHEMA_VERSION
            && recipe.subject_component_iri.is_none()
            && recipe.beats.is_empty()
        {
            validate_recipe(&recipe, recipe.status == StoryStatus::Published)?;
        }
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
            self.ensure_unique_published_subject(route_id, &recipe.subject_identity_key()?)?;
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
        validate_story_id(id)?;
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
        self.ensure_unique_published_subject(id, &current.subject_identity_key()?)?;
        self.write_recipe(id, current, Some(expected_updated_at))
    }

    pub fn published_for_subject(
        &self,
        subject: &StoryRecipeSubject,
    ) -> anyhow::Result<Option<StoryRecipe>> {
        let subject_key = subject.identity_key();
        let mut matches = self.list_recipes()?.into_iter().filter(|recipe| {
            recipe.status == StoryStatus::Published
                && recipe.subject_identity_key().ok().as_deref() == Some(subject_key.as_str())
        });
        let first = matches.next();
        if matches.next().is_some() {
            anyhow::bail!(
                "multiple published Stories target subject {subject_key}; repair the Story library"
            );
        }
        Ok(first)
    }

    fn ensure_unique_published_subject(&self, id: &str, subject_key: &str) -> anyhow::Result<()> {
        if let Some(existing) = self.list_recipes()?.into_iter().find(|recipe| {
            recipe.id != id
                && recipe.status == StoryStatus::Published
                && recipe.subject_identity_key().ok().as_deref() == Some(subject_key)
        }) {
            return Err(anyhow::Error::new(StoryConflict(format!(
                "Story {:?} is already published for subject {subject_key}",
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
        if recipe.schema_version == 0 || recipe.schema_version > STORY_SCHEMA_VERSION {
            return Err(anyhow::Error::new(StoryCorrupt(format!(
                "invalid stored Story recipe {}: unsupported schema version {}",
                path.display(),
                recipe.schema_version
            ))));
        }
        if recipe.schema_version == STORY_SCHEMA_VERSION && !recipe.beats.is_empty() {
            return Err(anyhow::Error::new(StoryCorrupt(format!(
                "invalid stored Story recipe {}: Story v3 must not contain legacy beats",
                path.display()
            ))));
        }
        let legacy = recipe.schema_version < STORY_SCHEMA_VERSION
            || recipe.subject_component_iri.is_some()
            || !recipe.beats.is_empty();
        if !legacy {
            validate_recipe(&recipe, recipe.status == StoryStatus::Published).map_err(|error| {
                anyhow::Error::new(StoryCorrupt(format!(
                    "invalid stored Story recipe {}: {error}",
                    path.display()
                )))
            })?;
        }
        let recipe = normalize_recipe(recipe);
        if expected_id.is_some_and(|expected| recipe.id != expected) {
            return Err(anyhow::Error::new(StoryCorrupt(format!(
                "Story filename id {:?} does not match body id {:?}",
                expected_id.unwrap_or_default(),
                recipe.id
            ))));
        }
        let mut validation_view = recipe.clone();
        if legacy
            && validation_view
                .curator_context
                .as_ref()
                .is_some_and(|context| context.chars().count() > 2_000)
        {
            // Legacy files remain readable without truncation. A subsequent
            // save still fails the v3 limit with an actionable message.
            validation_view.curator_context = None;
        }
        validate_recipe(
            &validation_view,
            validation_view.status == StoryStatus::Published,
        )
        .map_err(|error| {
            anyhow::Error::new(StoryCorrupt(format!(
                "invalid stored Story recipe {}: {error}",
                path.display()
            )))
        })?;
        Ok(recipe)
    }
}

fn normalize_recipe(mut recipe: StoryRecipe) -> StoryRecipe {
    if recipe.subject.is_none() {
        if let Some(iri) = recipe.subject_component_iri.take() {
            recipe.subject = Some(StoryRecipeSubject::Entity { iri });
        }
    }
    if !recipe.beats.is_empty() {
        let subject_iri = match &recipe.subject {
            Some(StoryRecipeSubject::Entity { iri }) => Some(iri.as_str()),
            _ => None,
        };
        let mut curator_notes = Vec::new();
        for beat in &recipe.beats {
            recipe.focus.emphasis.push((&beat.intent).into());
            recipe.focus.include_record_iris.extend(
                beat.record_iris
                    .iter()
                    .filter(|iri| Some(iri.as_str()) != subject_iri)
                    .cloned(),
            );
            recipe
                .focus
                .include_code_symbols
                .extend(beat.code_symbols.iter().cloned());
            if let Some(note) = beat
                .curator_note
                .as_deref()
                .map(str::trim)
                .filter(|note| !note.is_empty())
            {
                curator_notes.push(format!("{}: {note}", beat.title));
            }
        }
        if recipe.curator_context.is_none() && !curator_notes.is_empty() {
            recipe.curator_context = Some(curator_notes.join("\n\n"));
        }
    }
    canonicalize_focus(&mut recipe.focus);
    recipe.schema_version = STORY_SCHEMA_VERSION;
    recipe.beats.clear();
    recipe
}

fn canonicalize_focus(focus: &mut StoryFocus) {
    dedupe_stable(&mut focus.include_record_iris);
    dedupe_stable(&mut focus.exclude_record_iris);
    dedupe_stable(&mut focus.include_code_symbols);
    dedupe_stable(&mut focus.exclude_code_symbols);
    let mut emphasis = BTreeSet::new();
    focus.emphasis.retain(|kind| emphasis.insert(kind.clone()));
}

fn dedupe_stable(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

pub(super) fn next_revision(previous: Option<&str>) -> String {
    // The clock supplies ordering across restarts, while the prior token and
    // process counter preserve monotonicity across clock stalls/regressions.
    // The UUID keeps independently running writers from minting the same token.
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

pub(super) fn revision_value(value: &str) -> Option<u64> {
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

fn require_text(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    Ok(())
}

pub(super) fn validate_refs(kind: &str, refs: &[String]) -> anyhow::Result<()> {
    if refs.len() > MAX_FOCUS_REFS {
        anyhow::bail!("Story focus may contain at most {MAX_FOCUS_REFS} {kind}s");
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

pub(super) fn validate_recipe(recipe: &StoryRecipe, publishing: bool) -> anyhow::Result<()> {
    validate_story_id(&recipe.id)?;
    require_text("title", &recipe.title)?;
    if recipe.schema_version != STORY_SCHEMA_VERSION {
        anyhow::bail!("unsupported Story schema version {}", recipe.schema_version);
    }
    match recipe.resolved_subject()? {
        StoryRecipeSubject::Entity { iri } => {
            require_text("subject entity IRI", iri)?;
            NamedNode::new(iri)
                .map_err(|error| anyhow::anyhow!("subject entity must be an IRI: {error}"))?;
        }
        StoryRecipeSubject::Topic { query } => validate_topic(query)?,
    }
    require_text("goal", &recipe.goal)?;
    require_text("curator", &recipe.curator)?;
    if recipe.audience != "reboarding" {
        anyhow::bail!("story audience must be \"reboarding\"");
    }
    if recipe
        .curator_context
        .as_ref()
        .is_some_and(|context| context.chars().count() > 2_000)
    {
        anyhow::bail!("curator_context may contain at most 2000 characters; shorten the migrated curator notes before saving");
    }
    validate_refs("included record IRI", &recipe.focus.include_record_iris)?;
    validate_refs("excluded record IRI", &recipe.focus.exclude_record_iris)?;
    validate_refs("included code symbol", &recipe.focus.include_code_symbols)?;
    validate_refs("excluded code symbol", &recipe.focus.exclude_code_symbols)?;
    for iri in recipe
        .focus
        .include_record_iris
        .iter()
        .chain(&recipe.focus.exclude_record_iris)
    {
        NamedNode::new(iri)
            .map_err(|error| anyhow::anyhow!("Story focus record must be an IRI: {error}"))?;
    }
    let included_records = recipe
        .focus
        .include_record_iris
        .iter()
        .collect::<BTreeSet<_>>();
    if let Some(iri) = recipe
        .focus
        .exclude_record_iris
        .iter()
        .find(|iri| included_records.contains(iri))
    {
        anyhow::bail!("record IRI {iri:?} cannot be both included and excluded");
    }
    let included_code = recipe
        .focus
        .include_code_symbols
        .iter()
        .collect::<BTreeSet<_>>();
    if let Some(symbol) = recipe
        .focus
        .exclude_code_symbols
        .iter()
        .find(|symbol| included_code.contains(symbol))
    {
        anyhow::bail!("code symbol {symbol:?} cannot be both included and excluded");
    }
    if publishing
        && recipe.focus.include_record_iris.is_empty()
        && recipe.focus.include_code_symbols.is_empty()
        && matches!(recipe.resolved_subject()?, StoryRecipeSubject::Topic { .. })
    {
        anyhow::bail!(
            "a published topic Story must include at least one grounded record or code anchor"
        );
    }
    Ok(())
}
