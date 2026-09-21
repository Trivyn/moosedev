//! `moosedev.toml`: the local, human-authored model configuration for the
//! harness. It names the model for each role and that model's settings; it holds
//! no project knowledge and never a credential.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

pub const FILE_NAME: &str = super::CONFIG_FILE_NAME;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const HEADER: &str = "# Local MOOSEDev harness configuration. Keep this file out of version control:\n# model IDs and endpoints belong to this machine. A variable set in the real\n# environment overrides this file; this file overrides the project .env.\n# API keys are never stored here; api_key_env names the variable that holds one.\n";

/// Which configured model answers a request. The role follows the task mode:
/// planning work uses `Plan`; approved work, and the capture note written by
/// the model that did it, use `Implement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Plan,
    Implement,
}

impl ModelRole {
    pub const ALL: [Self; 2] = [Self::Plan, Self::Implement];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Implement => "implement",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == value)
    }
}

/// The keys one `[harness.model]` table or role sub-table may set. A role
/// inherits every key it leaves unset.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelKeys {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub context_window_tokens: Option<u64>,
    pub structured_output: Option<String>,
    pub response_policy: Option<String>,
    pub action_contract: Option<String>,
    pub connect_timeout_secs: Option<u64>,
    pub first_chunk_timeout_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub present: bool,
    pub default: ModelKeys,
    pub plan: Option<ModelKeys>,
    pub implement: Option<ModelKeys>,
}

impl ModelFile {
    pub fn role(&self, role: ModelRole) -> Option<&ModelKeys> {
        match role {
            ModelRole::Plan => self.plan.as_ref(),
            ModelRole::Implement => self.implement.as_ref(),
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        match read_regular(&root.join(FILE_NAME))? {
            Some(text) => Self::parse(&text).with_context(|| format!("invalid {FILE_NAME}")),
            None => Ok(Self::default()),
        }
    }

    fn parse(text: &str) -> Result<Self> {
        let mut file = Self {
            present: true,
            ..Self::default()
        };
        let document: toml::Table = text.parse()?;
        // Other top-level tables may come to belong to other MOOSEDev surfaces;
        // inside [harness] every key is ours, so a typo there is an error.
        let Some(harness) = document.get("harness") else {
            return Ok(file);
        };
        let harness = harness.as_table().context("[harness] must be a table")?;
        if let Some(unknown) = harness.keys().find(|key| *key != "model") {
            bail!("unknown key harness.{unknown}");
        }
        let Some(model) = harness.get("model") else {
            return Ok(file);
        };
        let mut model = model
            .as_table()
            .context("[harness.model] must be a table")?
            .clone();
        for role in ModelRole::ALL {
            let Some(keys) = model.remove(role.as_str()) else {
                continue;
            };
            let keys = keys
                .try_into::<ModelKeys>()
                .with_context(|| format!("[harness.model.{}]", role.as_str()))?;
            match role {
                ModelRole::Plan => file.plan = Some(keys),
                ModelRole::Implement => file.implement = Some(keys),
            }
        }
        file.default = toml::Value::Table(model)
            .try_into()
            .context("[harness.model]")?;
        Ok(file)
    }
}

/// Distinguishes a variable set in the real process environment from one that
/// only the project `.env` supplied. The first is a per-invocation override and
/// outranks `moosedev.toml`; the second is the shared project default (the
/// daemon reads the same file) and ranks below it.
///
/// It is a snapshot taken once, after the binary has loaded `.env` into the
/// process, so every key of one load is judged against the same state.
#[derive(Debug, Default)]
pub struct Environment {
    process: HashMap<String, String>,
    dotenv: HashMap<String, String>,
}

impl Environment {
    pub fn load(root: &Path) -> Result<Self> {
        let process: HashMap<String, String> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        let path = root.join(".env");
        let entries = match dotenvy::from_path_iter(&path) {
            Ok(entries) => entries,
            Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    process,
                    ..Self::default()
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("load dotenv {}", path.display()))
            }
        };
        let mut dotenv = HashMap::new();
        for entry in entries {
            let (key, value) = entry.with_context(|| format!("load dotenv {}", path.display()))?;
            dotenv.insert(key, value);
        }
        Ok(Self { process, dotenv })
    }

    /// A process and `.env` state for tests; `.env` entries are also loaded
    /// into the process, as the binary does before anything reads them.
    #[cfg(test)]
    pub fn of(process: &[(&str, &str)], dotenv: &[(&str, &str)]) -> Self {
        let owned = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        let mut all = owned(dotenv);
        all.extend(owned(process));
        Self {
            process: all,
            dotenv: owned(dotenv),
        }
    }

    /// The variable's value, from either source; blank counts as unset.
    pub fn value(&self, name: &str) -> Option<String> {
        self.process
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }

    /// Set outside the project `.env`. A value identical to the `.env` entry is
    /// indistinguishable from one it supplied and is treated as such.
    pub fn explicit(&self, name: &str) -> Option<String> {
        self.value(name)
            .filter(|value| self.dotenv.get(name) != Some(value))
    }

    pub fn project(&self, name: &str) -> Option<String> {
        self.value(name)
            .filter(|value| self.dotenv.get(name) == Some(value))
    }
}

/// A regular, bounded UTF-8 file, or `None` when absent. A symlink is refused
/// so the harness never reads or rewrites a file outside the project.
fn read_regular(path: &Path) -> Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    ensure!(
        metadata.is_file(),
        "{} must be a regular file, not a symlink or directory",
        path.display()
    );
    ensure!(
        metadata.len() <= MAX_FILE_BYTES,
        "{} exceeds {MAX_FILE_BYTES} bytes",
        path.display()
    );
    std::fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("read {}", path.display()))
}

fn child<'a>(
    parent: &'a mut dyn toml_edit::TableLike,
    key: &str,
    implicit: bool,
    path: &str,
) -> Result<&'a mut dyn toml_edit::TableLike> {
    if parent.get(key).is_none() {
        let mut table = toml_edit::Table::new();
        table.set_implicit(implicit);
        parent.insert(key, toml_edit::Item::Table(table));
    }
    parent
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_like_mut)
        .with_context(|| format!("[{path}] must be a table"))
}

/// Replace a string value where it stands, so the comment above the key and the
/// one trailing the value both survive; `insert` would replace the key as well.
fn set_string(table: &mut dyn toml_edit::TableLike, key: &str, text: &str) {
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        Some(value) => {
            let decor = value.decor().clone();
            *value = text.into();
            *value.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::value(text));
        }
    }
}

/// Persist a model choice by editing `moosedev.toml` in place, preserving the
/// user's comments and layout. `role` of `None` sets the default table.
pub fn set_model(
    root: &Path,
    role: Option<ModelRole>,
    endpoint: Option<&str>,
    model: &str,
) -> Result<()> {
    let path = root.join(FILE_NAME);
    let existing = read_regular(&path)?;
    let mut document: toml_edit::DocumentMut = existing
        .as_deref()
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("invalid {FILE_NAME}"))?;
    let harness = child(document.as_table_mut(), "harness", true, "harness")?;
    let mut table = child(harness, "model", false, "harness.model")?;
    if let Some(role) = role {
        table = child(
            table,
            role.as_str(),
            false,
            &format!("harness.model.{}", role.as_str()),
        )?;
    }
    if let Some(endpoint) = endpoint {
        set_string(table, "endpoint", endpoint);
    }
    set_string(table, "model", model);
    let mut text = document.to_string();
    if existing.is_none() {
        // A comment-only document keeps its comments after the tables.
        text.insert_str(0, &format!("{HEADER}\n"));
    }
    // Never leave a file the loader would reject.
    ModelFile::parse(&text).with_context(|| format!("refusing to write an invalid {FILE_NAME}"))?;

    let temporary = root.join(format!(".{FILE_NAME}-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    use std::io::Write;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("moosedev-config-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn roles_parse_beside_the_default_and_a_missing_file_is_empty() {
        let fixture = Fixture::new();
        assert_eq!(ModelFile::load(&fixture.0).unwrap(), ModelFile::default());
        std::fs::write(
            fixture.0.join(FILE_NAME),
            "[other]\nkept = true\n\n[harness.model]\nmodel = \"base\"\ncontext_window_tokens = 32768\n\n[harness.model.implement]\nmodel = \"small\"\naction_contract = \"json_schema\"\n",
        )
        .unwrap();
        let file = ModelFile::load(&fixture.0).unwrap();
        assert!(file.present);
        assert_eq!(file.default.model.as_deref(), Some("base"));
        assert_eq!(file.default.context_window_tokens, Some(32768));
        assert!(file.plan.is_none());
        let implement = file.role(ModelRole::Implement).unwrap();
        assert_eq!(implement.model.as_deref(), Some("small"));
        assert_eq!(implement.action_contract.as_deref(), Some("json_schema"));
        assert_eq!(implement.context_window_tokens, None);
    }

    #[test]
    fn typos_inside_the_harness_section_are_errors() {
        for (text, expected) in [
            ("[harness.model]\nmodle = \"x\"\n", "modle"),
            ("[harness.model.plan]\ncontext = 1\n", "harness.model.plan"),
            ("[harness.models]\nmodel = \"x\"\n", "harness.models"),
            ("[harness.model.review]\nmodel = \"x\"\n", "review"),
            (
                "[harness.model]\ncontext_window_tokens = -1\n",
                "harness.model",
            ),
            ("[harness.model]\napi_key = \"secret\"\n", "api_key"),
        ] {
            let error = format!("{:#}", ModelFile::parse(text).unwrap_err());
            assert!(error.contains(expected), "{text:?}: {error}");
        }
    }

    #[test]
    fn a_real_dotenv_separates_project_defaults_from_explicit_variables() {
        let fixture = Fixture::new();
        // Unique names: the process environment is shared by parallel tests.
        let tag = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
        let [shared, overridden, absent] =
            ["SHARED", "OVERRIDDEN", "ABSENT"].map(|name| format!("MD_TEST_{tag}_{name}"));
        std::fs::write(
            fixture.0.join(".env"),
            format!("{shared}=\"daemon model\"\n{overridden}=from-dotenv\n"),
        )
        .unwrap();
        // The binary loads .env into the process before anything reads it.
        dotenvy::from_path(fixture.0.join(".env")).unwrap();
        std::env::set_var(&overridden, "from-the-shell");
        let environment = Environment::load(&fixture.0).unwrap();
        assert_eq!(
            environment.project(&shared).as_deref(),
            Some("daemon model")
        );
        assert_eq!(environment.explicit(&shared), None);
        assert_eq!(
            environment.explicit(&overridden).as_deref(),
            Some("from-the-shell")
        );
        assert_eq!(environment.project(&overridden), None);
        assert_eq!(environment.value(&absent), None);
        for name in [&shared, &overridden] {
            std::env::remove_var(name);
        }
        // No .env at all: everything set is explicit.
        std::fs::remove_file(fixture.0.join(".env")).unwrap();
        std::env::set_var(&absent, "x");
        assert_eq!(
            Environment::load(&fixture.0)
                .unwrap()
                .explicit(&absent)
                .as_deref(),
            Some("x")
        );
        std::env::remove_var(&absent);
    }

    #[test]
    fn a_symlinked_config_is_refused() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("real.toml"), "").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fixture.0.join("real.toml"), fixture.0.join(FILE_NAME))
                .unwrap();
            assert!(ModelFile::load(&fixture.0).is_err());
            assert!(set_model(&fixture.0, None, None, "x").is_err());
        }
    }

    #[test]
    fn set_model_creates_the_file_then_edits_only_its_key_in_place() {
        let fixture = Fixture::new();
        set_model(&fixture.0, None, Some("http://127.0.0.1:1234/v1"), "base").unwrap();
        let created = std::fs::read_to_string(fixture.0.join(FILE_NAME)).unwrap();
        assert!(created.starts_with("# Local MOOSEDev harness configuration"));
        assert!(created.contains("[harness.model]\n"), "{created}");
        assert!(!created.contains("[harness]\n"), "{created}");

        std::fs::write(
            fixture.0.join(FILE_NAME),
            "# my notes\n[harness.model]\nmodel = \"base\" # the big one\ncontext_window_tokens = 65536\n\n[harness.model.plan]\n# planner\nmodel = \"old-planner\"\n",
        )
        .unwrap();
        set_model(&fixture.0, Some(ModelRole::Plan), None, "new-planner").unwrap();
        set_model(&fixture.0, Some(ModelRole::Implement), None, "small").unwrap();
        let edited = std::fs::read_to_string(fixture.0.join(FILE_NAME)).unwrap();
        assert!(edited.contains("# my notes\n"), "{edited}");
        assert!(
            edited.contains("model = \"base\" # the big one\n"),
            "{edited}"
        );
        assert!(
            edited.contains("# planner\nmodel = \"new-planner\"\n"),
            "{edited}"
        );
        assert!(!edited.contains("old-planner"), "{edited}");
        assert!(
            edited.contains("[harness.model.implement]\nmodel = \"small\"\n"),
            "{edited}"
        );
        let file = ModelFile::load(&fixture.0).unwrap();
        assert_eq!(file.default.context_window_tokens, Some(65536));
        assert_eq!(file.plan.unwrap().model.as_deref(), Some("new-planner"));
        assert!(std::fs::read_dir(&fixture.0).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[test]
    fn set_model_refuses_to_rewrite_a_file_it_could_not_load() {
        let fixture = Fixture::new();
        let text = "[harness.model]\nmodle = \"typo\"\n";
        std::fs::write(fixture.0.join(FILE_NAME), text).unwrap();
        assert!(set_model(&fixture.0, None, None, "x").is_err());
        assert_eq!(
            std::fs::read_to_string(fixture.0.join(FILE_NAME)).unwrap(),
            text
        );
    }
}
