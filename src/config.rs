//! `moosedev.toml`: the local, human-authored configuration in the project
//! root, read by the daemon (`[model]`, `[daemon]`) and the harness (`[model]`,
//! `[harness]`). It names endpoints, model ids and their settings; it holds no
//! project knowledge and never a credential.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, path::Path};

use crate::llm::{LlmConfig, LlmTimeouts};

pub const FILE_NAME: &str = "moosedev.toml";
const MAX_FILE_BYTES: u64 = 64 * 1024;

const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:0";
pub const DEFAULT_API_KEY_ENV: &str = "MOOSEDEV_LLM_API_KEY";

/// The keys a `[model]` or `[daemon.model]` table may set; a more specific
/// table inherits every key it leaves unset.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderKeys {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub context_window_tokens: Option<u64>,
    pub structured_output: Option<String>,
    pub connect_timeout_secs: Option<u64>,
    pub first_chunk_timeout_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
    pub tool_arguments_timeout_secs: Option<u64>,
}

/// One table of provider settings, read by the environment variable it stands
/// in for, so both processes resolve a key through the same `pick`.
pub trait ProviderLayer {
    fn get(&self, variable: &str) -> Option<String>;
    /// The variable holding the API key; never a value from the environment.
    fn api_key_env(&self) -> Option<String>;
}

impl ProviderLayer for ProviderKeys {
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

/// The `[daemon]` table: where the web UI binds, which browser origins it
/// trusts, and the daemon's own model.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonKeys {
    pub http_addr: Option<String>,
    pub allowed_origins: Option<Vec<String>>,
    pub model: Option<ProviderKeys>,
}

/// The parsed file. `[model]`, `[daemon]` and `[harness]` are ours, so a typo
/// inside them is an error; other top-level tables may come to belong to other
/// MOOSEDev surfaces and are left alone.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProjectFile {
    pub present: bool,
    pub model: ProviderKeys,
    pub daemon: DaemonKeys,
    /// The raw `[harness]` table, parsed by the harness's own loader.
    pub harness: Option<toml::Table>,
}

impl ProjectFile {
    pub fn load(root: &Path) -> Result<Self> {
        match read_regular(&root.join(FILE_NAME))? {
            Some(text) => Self::parse(&text).with_context(|| format!("invalid {FILE_NAME}")),
            None => Ok(Self::default()),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut file = Self {
            present: true,
            ..Self::default()
        };
        let document: toml::Table = text.parse()?;
        if let Some(model) = document.get("model") {
            file.model = model.clone().try_into().context("[model]")?;
        }
        if let Some(daemon) = document.get("daemon") {
            file.daemon = daemon.clone().try_into().context("[daemon]")?;
        }
        if let Some(harness) = document.get("harness") {
            file.harness = Some(
                harness
                    .as_table()
                    .context("[harness] must be a table")?
                    .clone(),
            );
        }
        Ok(file)
    }
}

/// Distinguishes a variable set in the real process environment from one that
/// only the project `.env` supplied. The first is a per-invocation override and
/// outranks `moosedev.toml`; the second is the shared project default and ranks
/// below it.
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

/// One key's value: a variable set in the real environment, then `layers` in
/// order (the most specific table first), then a value only the project `.env`
/// supplied. The caller adds its own fallbacks and default.
pub fn pick(
    environment: &Environment,
    layers: &[&dyn ProviderLayer],
    variable: &str,
) -> Option<String> {
    environment
        .explicit(variable)
        .or_else(|| layers.iter().find_map(|layer| layer.get(variable)))
        .or_else(|| environment.project(variable))
}

/// The variable `api_key_env` names (from the layers only, never the
/// environment) and its value, if set. A named variable other than the default
/// must be set: a file that names a key it cannot find is a misconfiguration,
/// not a keyless provider.
pub fn api_key(
    environment: &Environment,
    layers: &[&dyn ProviderLayer],
) -> Result<(String, Option<String>)> {
    let variable = layers
        .iter()
        .find_map(|layer| layer.api_key_env())
        .unwrap_or_else(|| DEFAULT_API_KEY_ENV.into());
    ensure!(
        !variable.is_empty()
            && variable
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "api_key_env must name an environment variable, not hold a key"
    );
    let key = environment.value(&variable);
    ensure!(
        key.is_some() || variable == DEFAULT_API_KEY_ENV,
        "api_key_env names {variable}, which is not set"
    );
    Ok((variable, key))
}

/// The daemon's resolved settings: the web UI bind address, the browser origins
/// it trusts, and its model. Each key resolves separately: a variable set in the
/// real environment, then `[daemon]` / `[daemon.model]`, then `[model]`, then a
/// value only the project `.env` supplies, then the built-in default.
#[derive(Debug, Clone)]
pub struct DaemonSettings {
    pub http_addr: SocketAddr,
    pub allowed_origins: Vec<String>,
    pub llm: LlmConfig,
}

impl DaemonSettings {
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_with(&Environment::load(root)?, &ProjectFile::load(root)?)
    }

    pub fn load_with(environment: &Environment, file: &ProjectFile) -> Result<Self> {
        let http_addr = match environment.explicit("MOOSEDEV_HTTP_ADDR") {
            Some(raw) => raw
                .parse()
                .with_context(|| format!("parse MOOSEDEV_HTTP_ADDR={raw:?}"))?,
            None => match &file.daemon.http_addr {
                Some(raw) => raw
                    .parse()
                    .with_context(|| format!("daemon.http_addr {raw:?} in {FILE_NAME}"))?,
                None => match environment.project("MOOSEDEV_HTTP_ADDR") {
                    Some(raw) => raw
                        .parse()
                        .with_context(|| format!("parse MOOSEDEV_HTTP_ADDR={raw:?}"))?,
                    None => DEFAULT_HTTP_ADDR
                        .parse()
                        .expect("default http address parses"),
                },
            },
        };

        let allowed_origins = match environment.explicit("MOOSEDEV_ALLOWED_ORIGINS") {
            Some(raw) => origins_from_env(&raw),
            None => match &file.daemon.allowed_origins {
                Some(origins) => {
                    for origin in origins {
                        ensure!(
                            crate::api::security::origin_authority(origin).is_some(),
                            "daemon.allowed_origins entry {origin:?} in {FILE_NAME} must be an http(s) origin without a path"
                        );
                    }
                    origins.clone()
                }
                None => environment
                    .project("MOOSEDEV_ALLOWED_ORIGINS")
                    .map(|raw| origins_from_env(&raw))
                    .unwrap_or_default(),
            },
        };

        let mut layers: Vec<&dyn ProviderLayer> = Vec::new();
        if let Some(model) = &file.daemon.model {
            layers.push(model);
        }
        layers.push(&file.model);
        let endpoint = pick(environment, &layers, "MOOSEDEV_LLM_BASE_URL");
        let (_, key) = api_key(environment, &layers)?;
        let mut llm = LlmConfig::from_values(
            endpoint,
            key,
            pick(environment, &layers, "MOOSEDEV_LLM_MODEL"),
            pick(environment, &layers, "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS"),
            pick(environment, &layers, "MOOSEDEV_LLM_STRUCTURED_OUTPUT"),
        )?;
        llm.timeouts = LlmTimeouts::from_values(
            pick(environment, &layers, "MOOSEDEV_LLM_CONNECT_TIMEOUT_SECS"),
            pick(
                environment,
                &layers,
                "MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS",
            ),
            pick(environment, &layers, "MOOSEDEV_LLM_IDLE_TIMEOUT_SECS"),
            pick(
                environment,
                &layers,
                "MOOSEDEV_LLM_TOOL_ARGUMENTS_TIMEOUT_SECS",
            ),
        )?;
        Ok(Self {
            http_addr,
            allowed_origins,
            llm,
        })
    }
}

/// The environment form is a comma-separated list; malformed entries are
/// dropped, as they always were.
fn origins_from_env(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|origin| crate::api::security::origin_authority(origin).is_some())
        .map(str::to_owned)
        .collect()
}

/// A regular, bounded UTF-8 file, or `None` when absent. A symlink is refused
/// so nothing reads or rewrites a file outside the project.
pub fn read_regular(path: &Path) -> Result<Option<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
[model]
endpoint = \"http://127.0.0.1:1234/v1\"
model = \"shared-model\"
context_window_tokens = 65536

[daemon]
http_addr = \"0.0.0.0:7480\"
allowed_origins = [\"http://mbp.local:7480\"]

[daemon.model]
endpoint = \"http://yavin:1234/v1\"

[harness.model]
model = \"harness-model\"

[other]
kept = true
";

    #[test]
    fn the_file_parses_its_three_tables_and_tolerates_others() {
        let file = ProjectFile::parse(FILE).unwrap();
        assert!(file.present);
        assert_eq!(file.model.model.as_deref(), Some("shared-model"));
        assert_eq!(file.daemon.http_addr.as_deref(), Some("0.0.0.0:7480"));
        assert_eq!(
            file.daemon.model.as_ref().unwrap().endpoint.as_deref(),
            Some("http://yavin:1234/v1")
        );
        assert!(file.harness.unwrap().contains_key("model"));

        let empty = ProjectFile::parse("").unwrap();
        assert!(empty.present && empty.harness.is_none());
        assert_eq!(empty.model, ProviderKeys::default());

        for (text, table) in [
            ("[daemon]\nhtpp_addr = \"x\"\n", "[daemon]"),
            ("[model]\napi_key = \"sk-1\"\n", "[model]"),
            ("[daemon.model]\nresponse_policy = \"auto\"\n", "[daemon]"),
            ("harness = 1\n", "[harness] must be a table"),
        ] {
            let error = format!("{:#}", ProjectFile::parse(text).unwrap_err());
            assert!(error.contains(table), "{text}: {error}");
        }
    }

    #[test]
    fn daemon_settings_resolve_each_key_by_precedence() {
        let file = ProjectFile::parse(FILE).unwrap();
        let dotenv = [
            ("MOOSEDEV_HTTP_ADDR", "127.0.0.1:7474"),
            ("MOOSEDEV_LLM_MODEL", "dotenv-model"),
            ("MOOSEDEV_ALLOWED_ORIGINS", "http://localhost:5173"),
        ];

        let settings = DaemonSettings::load_with(&Environment::of(&[], &dotenv), &file).unwrap();
        assert_eq!(settings.http_addr.to_string(), "0.0.0.0:7480");
        assert_eq!(settings.allowed_origins, ["http://mbp.local:7480"]);
        assert_eq!(settings.llm.base_url, "http://yavin:1234/v1");
        assert_eq!(settings.llm.model, "shared-model");
        assert_eq!(settings.llm.context_window_tokens, 65536);
        assert_eq!(settings.llm.api_key, "lm-studio");
        assert!(settings.llm.configured);

        let explicit = [
            ("MOOSEDEV_HTTP_ADDR", "[::1]:9000"),
            ("MOOSEDEV_LLM_MODEL", "shell-model"),
            ("MOOSEDEV_ALLOWED_ORIGINS", "http://a:1, junk, http://b:2"),
            ("MOOSEDEV_LLM_API_KEY", "secret"),
        ];
        let settings =
            DaemonSettings::load_with(&Environment::of(&explicit, &dotenv), &file).unwrap();
        assert_eq!(settings.http_addr.to_string(), "[::1]:9000");
        assert_eq!(settings.allowed_origins, ["http://a:1", "http://b:2"]);
        assert_eq!(settings.llm.model, "shell-model");
        assert_eq!(settings.llm.base_url, "http://yavin:1234/v1");
        assert_eq!(settings.llm.api_key, "secret");

        let settings =
            DaemonSettings::load_with(&Environment::of(&[], &dotenv), &ProjectFile::default())
                .unwrap();
        assert_eq!(settings.http_addr.to_string(), "127.0.0.1:7474");
        assert_eq!(settings.allowed_origins, ["http://localhost:5173"]);
        assert_eq!(settings.llm.model, "dotenv-model");
        assert!(!settings.llm.configured, "no endpoint from any source");

        let settings =
            DaemonSettings::load_with(&Environment::of(&[], &[]), &ProjectFile::default()).unwrap();
        assert_eq!(settings.http_addr.to_string(), DEFAULT_HTTP_ADDR);
        assert!(settings.http_addr.ip().is_loopback() && settings.http_addr.port() == 0);
        assert!(settings.allowed_origins.is_empty());
    }

    #[test]
    fn the_shipped_example_loads_for_both_processes() {
        let text = include_str!("../moosedev.toml.example");
        let file = ProjectFile::parse(text).expect("moosedev.toml.example parses");
        let settings = DaemonSettings::load_with(&Environment::of(&[], &[]), &file).unwrap();
        assert_eq!(settings.llm.model, "your-model-id");
        assert!(settings.llm.configured);
        assert!(
            file.harness.is_some(),
            "the [harness] table is part of the example"
        );
        // Every commented key must be a real one: uncomment them all and reparse.
        let is_setting = |rest: &str| {
            rest.starts_with('[')
                || rest.split_once(" = ").is_some_and(|(key, _)| {
                    !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                })
        };
        let uncommented: String = text
            .lines()
            .map(|line| {
                line.strip_prefix("# ")
                    .filter(|rest| is_setting(rest))
                    .unwrap_or(line)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let file = ProjectFile::parse(&uncommented).expect("every commented key is valid");
        let environment = Environment::of(&[("MOOSEDEV_LLM_API_KEY", "k")], &[]);
        let settings = DaemonSettings::load_with(&environment, &file).unwrap();
        assert_eq!(settings.http_addr.to_string(), "127.0.0.1:7474");
        assert_eq!(settings.allowed_origins.len(), 2);
        assert_eq!(settings.llm.model, "google/gemma-4-26b-a4b");
    }

    #[test]
    fn invalid_file_values_name_their_key() {
        let environment = Environment::of(&[], &[]);
        for (text, expected) in [
            ("[daemon]\nhttp_addr = \"nope\"\n", "daemon.http_addr"),
            (
                "[daemon]\nallowed_origins = [\"mbp.local:7480\"]\n",
                "daemon.allowed_origins",
            ),
            (
                "[model]\napi_key_env = \"sk-live\"\n",
                "api_key_env must name",
            ),
            (
                "[model]\napi_key_env = \"UNSET_KEY_VAR\"\n",
                "which is not set",
            ),
            ("[model]\ncontext_window_tokens = 1\n", "at least"),
        ] {
            let file = ProjectFile::parse(text).unwrap();
            let error = format!(
                "{:#}",
                DaemonSettings::load_with(&environment, &file).unwrap_err()
            );
            assert!(error.contains(expected), "{text}: {error}");
        }
    }

    #[test]
    fn a_real_dotenv_separates_project_defaults_from_explicit_variables() {
        let dir = std::env::temp_dir().join(format!("moosedev-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Unique names: the process environment is shared by parallel tests.
        let tag = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
        let [shared, overridden, absent] =
            ["SHARED", "OVERRIDDEN", "ABSENT"].map(|name| format!("MD_TEST_{tag}_{name}"));
        std::fs::write(
            dir.join(".env"),
            format!("{shared}=\"daemon model\"\n{overridden}=from-dotenv\n"),
        )
        .unwrap();
        // The binary loads .env into the process before anything reads it.
        dotenvy::from_path(dir.join(".env")).unwrap();
        std::env::set_var(&overridden, "from-the-shell");
        let environment = Environment::load(&dir).unwrap();
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
        std::fs::remove_file(dir.join(".env")).unwrap();
        std::env::set_var(&absent, "x");
        assert_eq!(
            Environment::load(&dir)
                .unwrap()
                .explicit(&absent)
                .as_deref(),
            Some("x")
        );
        std::env::remove_var(&absent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlinked_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("moosedev-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real.toml"), "[model]\n").unwrap();
        std::os::unix::fs::symlink(dir.join("real.toml"), dir.join(FILE_NAME)).unwrap();
        let error = ProjectFile::load(&dir).unwrap_err().to_string();
        assert!(error.contains("regular file"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
