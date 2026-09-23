//! The `[harness]` table of `moosedev.toml` (see [`crate::config`]): the model
//! for each role and that model's settings, inheriting the project-wide
//! `[model]` table.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::config::ProviderLayer;
pub use crate::config::{read_regular, Environment, ProjectFile, ProviderKeys, FILE_NAME};

const HEADER: &str = "# Local MOOSEDev configuration. Keep this file out of version control: model IDs\n# and endpoints belong to this machine. [model] is every process's default;\n# [daemon] and [harness] override it. A variable set in the real environment\n# overrides this file; this file overrides the project .env.\n# API keys are never stored here; api_key_env names the variable that holds one.\n";

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
    pub tool_arguments_timeout_secs: Option<u64>,
}

impl ProviderLayer for ModelKeys {
    fn api_key_env(&self) -> Option<String> {
        self.api_key_env.clone()
    }

    fn get(&self, variable: &str) -> Option<String> {
        match variable {
            "MOOSEDEV_LLM_BASE_URL" => self.endpoint.clone(),
            "MOOSEDEV_LLM_MODEL" => self.model.clone(),
            "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS" => {
                self.context_window_tokens.map(|tokens| tokens.to_string())
            }
            "MOOSEDEV_LLM_STRUCTURED_OUTPUT" => self.structured_output.clone(),
            "MOOSEDEV_HARNESS_RESPONSE_POLICY" => self.response_policy.clone(),
            "MOOSEDEV_HARNESS_ACTION_CONTRACT" => self.action_contract.clone(),
            "MOOSEDEV_LLM_CONNECT_TIMEOUT_SECS" => {
                self.connect_timeout_secs.map(|secs| secs.to_string())
            }
            "MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS" => {
                self.first_chunk_timeout_secs.map(|secs| secs.to_string())
            }
            "MOOSEDEV_LLM_IDLE_TIMEOUT_SECS" => self.idle_timeout_secs.map(|secs| secs.to_string()),
            "MOOSEDEV_LLM_TOOL_ARGUMENTS_TIMEOUT_SECS" => self
                .tool_arguments_timeout_secs
                .map(|secs| secs.to_string()),
            _ => None,
        }
    }
}

/// When the harness rebuilds the code index itself, so that associations and
/// capture anchors are proven against the source the task produced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexRefresh {
    /// At finish, with every producer that detects the project.
    #[default]
    Auto,
    /// Only the study pilot's frozen Python producer, through the daemon.
    FrozenPython,
    /// Never; the project indexes through `moosedev index` or its git hooks.
    Off,
}

impl IndexRefresh {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::FrozenPython => "frozen-python",
            Self::Off => "off",
        }
    }
}

/// The `[harness.sandbox]` table: paths this project grants to every task
/// without asking, because they belong to the machine rather than to a task.
/// A cargo `[paths]` override, for instance, makes a sibling checkout part of
/// every build here, and being asked for it once per task is the wrong
/// granularity.
///
/// Read only, and never a substitute for the per-task gate: a standing path is
/// a permanent, promptless capability, so the loader refuses the ones whose
/// breadth would make the gate meaningless, and every surface that shows
/// grants shows these beside them.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxKeys {
    #[serde(default)]
    pub read_paths: Vec<String>,
}

impl SandboxKeys {
    /// Reject a standing path whose breadth would hand over more than a human
    /// can reason about, or that the per-command allowlist deliberately
    /// withholds. `~/.cargo` is the tempting one and the wrong one: a subpath
    /// grant over it exposes `credentials.toml`, which `secret_free_cargo_config`
    /// exists to withhold, and its registry alone outgrows any survey.
    fn validate(&self) -> Result<()> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        for raw in &self.read_paths {
            let path = Path::new(raw);
            anyhow::ensure!(
                path.is_absolute(),
                "[harness.sandbox] read_paths must be absolute: {raw:?}"
            );
            anyhow::ensure!(
                path.parent().is_some(),
                "[harness.sandbox] read_paths cannot grant the filesystem root"
            );
            if let Some(home) = &home {
                anyhow::ensure!(
                    path != home,
                    "[harness.sandbox] read_paths cannot grant the whole home directory: {raw:?}"
                );
                anyhow::ensure!(
                    path != home.join(".cargo"),
                    "[harness.sandbox] read_paths cannot grant ~/.cargo, which would expose credentials.toml; \
                     the registry and a secret-free config.toml are already readable"
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub present: bool,
    /// The project-wide `[model]` table, beneath every harness table.
    pub shared: ProviderKeys,
    pub default: ModelKeys,
    pub plan: Option<ModelKeys>,
    pub implement: Option<ModelKeys>,
    pub index_refresh: IndexRefresh,
    pub sandbox: SandboxKeys,
}

impl ModelFile {
    pub fn role(&self, role: ModelRole) -> Option<&ModelKeys> {
        match role {
            ModelRole::Plan => self.plan.as_ref(),
            ModelRole::Implement => self.implement.as_ref(),
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        Self::of(ProjectFile::load(root)?)
    }

    fn parse(text: &str) -> Result<Self> {
        Self::of(ProjectFile::parse(text)?)
    }

    fn of(project: ProjectFile) -> Result<Self> {
        let mut file = Self {
            present: project.present,
            shared: project.model,
            ..Self::default()
        };
        // Inside [harness] every key is ours, so a typo there is an error.
        let Some(harness) = project.harness else {
            return Ok(file);
        };
        if let Some(unknown) = harness
            .keys()
            .find(|key| !matches!(key.as_str(), "model" | "index_refresh" | "sandbox"))
        {
            bail!("unknown key harness.{unknown}");
        }
        if let Some(value) = harness.get("index_refresh") {
            file.index_refresh = value
                .clone()
                .try_into()
                .context("harness.index_refresh must be \"auto\", \"frozen-python\" or \"off\"")?;
        }
        if let Some(sandbox) = harness.get("sandbox") {
            file.sandbox = sandbox.clone().try_into().context("[harness.sandbox]")?;
            file.sandbox.validate()?;
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
        assert_eq!(file.index_refresh, IndexRefresh::Auto);
        assert_eq!(file.default.model.as_deref(), Some("base"));
        assert_eq!(file.default.context_window_tokens, Some(32768));
        assert!(file.plan.is_none());
        let implement = file.role(ModelRole::Implement).unwrap();
        assert_eq!(implement.model.as_deref(), Some("small"));
        assert_eq!(implement.action_contract.as_deref(), Some("json_schema"));
        assert_eq!(implement.context_window_tokens, None);
    }

    #[test]
    fn standing_sandbox_paths_load_but_the_broadest_ones_are_refused() {
        let file = ModelFile::parse(
            "[harness.sandbox]\nread_paths = [\"/usr/share\", \"/opt/homebrew\"]\n",
        )
        .unwrap();
        assert_eq!(file.sandbox.read_paths, ["/usr/share", "/opt/homebrew"]);
        // A standing path is permanent and promptless, so the breadth that
        // would make the per-task gate meaningless is refused at load.
        let home = std::env::var("HOME").unwrap();
        for (text, expected) in [
            ("relative/path".to_string(), "must be absolute"),
            ("/".to_string(), "filesystem root"),
            (home.clone(), "whole home directory"),
            (format!("{home}/.cargo"), "credentials.toml"),
        ] {
            let error =
                ModelFile::parse(&format!("[harness.sandbox]\nread_paths = [\"{text}\"]\n"))
                    .unwrap_err()
                    .to_string();
            assert!(error.contains(expected), "{text}: {error}");
        }
        // The table is ours, so a typo inside it is an error like any other.
        assert!(ModelFile::parse("[harness.sandbox]\nwrite_paths = [\"/tmp\"]\n").is_err());
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
            ("[harness]\nindex_refresh = \"always\"\n", "index_refresh"),
            ("[harness]\nindex_refresh = true\n", "index_refresh"),
        ] {
            let error = format!("{:#}", ModelFile::parse(text).unwrap_err());
            assert!(error.contains(expected), "{text:?}: {error}");
        }
    }

    #[test]
    fn index_refresh_parses_its_three_settings_and_defaults_to_auto() {
        assert_eq!(
            ModelFile::parse("[harness.model]\nmodel = \"x\"\n")
                .unwrap()
                .index_refresh,
            IndexRefresh::Auto
        );
        for (text, expected) in [
            ("auto", IndexRefresh::Auto),
            ("frozen-python", IndexRefresh::FrozenPython),
            ("off", IndexRefresh::Off),
        ] {
            let file = ModelFile::parse(&format!(
                "[harness]\nindex_refresh = \"{text}\"\n\n[harness.model]\nmodel = \"x\"\n"
            ))
            .unwrap();
            assert_eq!(file.index_refresh, expected, "{text}");
            assert_eq!(file.default.model.as_deref(), Some("x"));
        }
    }

    #[test]
    fn the_shared_model_table_is_read_beside_the_harness_tables() {
        let file = ModelFile::parse(
            "[model]\nendpoint = \"http://127.0.0.1:1234/v1\"\nmodel = \"shared\"\n\n[harness.model.plan]\nmodel = \"planner\"\n",
        )
        .unwrap();
        assert_eq!(file.shared.model.as_deref(), Some("shared"));
        assert_eq!(file.default, ModelKeys::default());
        assert_eq!(file.plan.unwrap().model.as_deref(), Some("planner"));
        // A typo in [model] is an error for the harness too: same file.
        assert!(ModelFile::parse("[model]\nmodle = \"x\"\n").is_err());
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
        assert!(created.starts_with("# Local MOOSEDev configuration"));
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
