//! Interactive client startup. Discovery never opens the graph's writer store.
use anyhow::{bail, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::config::{self, Environment, ModelFile, ModelRole};
use super::protocol::{ContextRequest, ContextResponse};
use super::response::{ActionContract, ResponsePolicy};
use crate::config::{ProviderLayer, DEFAULT_API_KEY_ENV};
use crate::{llm::LlmConfig, runtime};

const BOUNDED_CONTEXT_PROBE_BYTES: usize = 4_096;

#[derive(Clone, Debug)]
pub struct StartupOptions {
    pub root: PathBuf,
    pub daemon: Option<String>,
    pub daemon_exe: Option<PathBuf>,
}

impl StartupOptions {
    pub fn data_dir(&self) -> PathBuf {
        match std::env::var_os("MOOSEDEV_DATA_DIR") {
            Some(value) if !value.is_empty() => self.root.join(value),
            _ => self.root.join(".moosedev"),
        }
    }

    pub fn needs_initialization(&self) -> bool {
        !self.data_dir().is_dir()
    }

    /// Call only after the human selects initialization. The regular init command
    /// owns project setup; the harness does not maintain a second implementation.
    pub async fn initialize(&self) -> Result<String> {
        let exe = resolve_daemon_executable(self.daemon_exe.as_deref())?;
        let output = tokio::process::Command::new(&exe)
            .arg("init")
            .arg(&self.root)
            .arg("--binary")
            .arg(&exe)
            .arg("--data-dir")
            .arg(self.data_dir())
            .current_dir(&self.root)
            .output()
            .await
            .context("initialize MOOSEDev project")?;
        ensure!(
            output.status.success(),
            "project initialization failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub async fn ensure_daemon(&self) -> Result<String> {
        let root = self.root.canonicalize().context("resolve project root")?;
        let data_dir = self
            .data_dir()
            .canonicalize()
            .context("project is not initialized; use /init")?;
        if let Some(url) = &self.daemon {
            let url = daemon_base_url(url)?;
            verify_daemon(&url, &root, &data_dir).await?;
            return Ok(url);
        }
        let socket = std::env::var_os("MOOSEDEV_SOCKET")
            .map(|path| root.join(path))
            .unwrap_or_else(|| runtime::socket_path_for(&data_dir));
        // An occupied HTTP address is never grounds to replace its owner, even
        // when its identity/capabilities are wrong. A refused connection is stale.
        if let Some(addr) = runtime::read_http_addr(&data_dir) {
            let url = format!("http://{addr}");
            match verify_daemon(&url, &root, &data_dir).await {
                Ok(()) => return Ok(url),
                Err(error) if connection_refused(&error) => {}
                Err(error) => return Err(error.context(format!(
                    "the address recorded in {} is occupied but cannot serve this project; check the daemon log, or remove this file if its previous daemon has stopped and the port was reclaimed, then use /connect",
                    runtime::http_addr_file_path_for(&data_dir).display()
                ))),
            }
        }
        if runtime::backend_is_live(&socket).await {
            bail!("the project daemon is running but its harness HTTP API is unavailable; enable HTTP or restart it with a compatible moosedev binary");
        }
        ensure!(!env_enabled("MOOSEDEV_NO_AUTOSPAWN"), "no project daemon is available and MOOSEDEV_NO_AUTOSPAWN is set; start moosedev --serve");
        ensure!(
            !env_enabled("MOOSEDEV_NO_HTTP"),
            "MOOSEDEV_NO_HTTP disables the API required by the harness"
        );
        let exe = resolve_daemon_executable(self.daemon_exe.as_deref())?;
        let mut child = runtime::spawn_detached_backend_with_exe(&socket, &data_dir, &exe, &root)?;
        // Dropping this startup future leaves the shared daemon alive. The UI
        // remains responsive while a cold model/index load finishes.
        tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                if let Some(addr) = runtime::read_http_addr(&data_dir) {
                    let url = format!("http://{addr}");
                    match verify_daemon(&url, &root, &data_dir).await {
                        Ok(()) => return Ok(url),
                        Err(error) if connection_refused(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                if let Some(status) = child.try_wait()? {
                    bail!(
                        "daemon exited during startup ({status}); see {}",
                        runtime::serve_log_path_for(&data_dir).display()
                    );
                }
                if let Some(url) = started_backend_http(&socket, &root, &data_dir).await? {
                    return Ok(url);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("daemon startup timed out; inspect .moosedev/moosedev-serve.log")?
    }
}

async fn started_backend_http(
    socket: &Path,
    root: &Path,
    data_dir: &Path,
) -> Result<Option<String>> {
    // Runtime publishes HTTP before binding the MCP socket. A live socket after
    // an unsuccessful HTTP probe therefore means startup finished without HTTP.
    if !runtime::backend_is_live(socket).await {
        return Ok(None);
    }
    let hint = || {
        format!("the daemon started but its harness HTTP API failed to start; check [daemon].http_addr in {} (or MOOSEDEV_HTTP_ADDR) and {} for a bind failure, then restart the daemon", config::FILE_NAME, runtime::serve_log_path_for(data_dir).display())
    };
    // Re-read after observing the socket: publication may have occurred between
    // the outer HTTP probe and this liveness check.
    let addr = runtime::read_http_addr(data_dir).with_context(hint)?;
    let url = format!("http://{addr}");
    verify_daemon(&url, root, data_dir)
        .await
        .with_context(hint)?;
    Ok(Some(url))
}

fn env_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty() && value != "0")
}

fn connection_refused(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    })
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?)
}

fn daemon_base_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "daemon URL must be an HTTP base URL without credentials, query, or fragment"
    );
    let host = url.host_str().unwrap_or("");
    let local = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    ensure!(local, "daemon must use a loopback address");
    ensure!(
        url.path() == "/" || url.path().is_empty(),
        "daemon URL must not contain an API path"
    );
    Ok(value.trim_end_matches('/').to_string())
}

async fn verify_daemon(url: &str, root: &Path, data_dir: &Path) -> Result<()> {
    daemon_base_url(url)?;
    let client = client()?;
    let health: serde_json::Value = client
        .get(format!("{url}/api/v1/health"))
        .send()
        .await
        .context("connect to project daemon")?
        .error_for_status()
        .context("daemon health endpoint failed")?
        .json()
        .await
        .context("invalid daemon health response")?;
    ensure!(
        health["status"] == "ok" && health["project_graph"] == crate::graph::PROJECT_KG_GRAPH_IRI,
        "address is not a compatible MOOSEDev daemon"
    );
    ensure!(
        health["data_dir"].as_str().map(Path::new) == Some(data_dir)
            && health["project_root"].as_str().map(Path::new) == Some(root),
        "daemon belongs to a different project or data directory; refusing connection"
    );
    let response = client
        .post(format!("{url}/api/v1/harness/context"))
        .json(&ContextRequest {
            topic: "harness startup".into(),
            files: vec![],
            evidence_only: true,
            max_bytes: Some(BOUNDED_CONTEXT_PROBE_BYTES),
        })
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        if matches!(status.as_u16(), 404 | 405) {
            bail!("daemon lacks a working harness API; restart with an updated moosedev binary");
        }
        let body = response.text().await.unwrap_or_default();
        bail!(
            "daemon harness context failed ({status}): {}",
            body.chars().take(2000).collect::<String>()
        );
    }
    let context: ContextResponse = response
        .json()
        .await
        .context("daemon harness API is incompatible")?;
    ensure!(
        Path::new(&context.project_root) == root && !context.revision.is_empty(),
        "daemon harness context identity is incompatible"
    );
    ensure!(
        context.delivery_receipt.is_some(),
        "daemon lacks bounded harness context delivery; restart with an updated moosedev binary"
    );
    Ok(())
}

/// Explicit override, matching installed sibling, then PATH. Reject the client
/// itself even through a symlink so auto-start can never recurse into its TUI.
pub fn resolve_daemon_executable(explicit: Option<&Path>) -> Result<PathBuf> {
    let current = std::env::current_exe()?.canonicalize()?;
    if let Some(path) = explicit {
        return checked_executable(path, &current);
    }
    if let Some(parent) = current.parent() {
        let sibling = parent.join("moosedev");
        if sibling.is_file() {
            return checked_executable(&sibling, &current);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("moosedev");
            if candidate.is_file() {
                return checked_executable(&candidate, &current);
            }
        }
    }
    bail!("moosedev server executable not found; build/install both binaries or pass --daemon-exe PATH")
}

fn checked_executable(path: &Path, current: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = path
        .canonicalize()
        .context("resolve moosedev server executable")?;
    ensure!(
        path != current,
        "daemon executable points to the harness itself"
    );
    let metadata = path.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        "daemon executable is not an executable file"
    );
    Ok(path)
}

/// One role's fully resolved model and the levers that follow it.
#[derive(Clone, Debug)]
pub struct RoleSettings {
    pub config: LlmConfig,
    pub response_policy: ResponsePolicy,
    pub action_contract: ActionContract,
}

/// `config` and `response_policy` are the default every role uses; `plan` and
/// `implement` are present only when `moosedev.toml` gives that role a table.
#[derive(Clone, Debug)]
pub struct ProviderSettings {
    pub config: LlmConfig,
    pub response_policy: ResponsePolicy,
    /// `None` leaves the runner on `MOOSEDEV_HARNESS_ACTION_CONTRACT`.
    pub action_contract: Option<ActionContract>,
    pub plan: Option<RoleSettings>,
    pub implement: Option<RoleSettings>,
    pub index_refresh: config::IndexRefresh,
}

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:1234/v1";

/// Resolve one table. Per key: a variable set in the real environment, then
/// `layers` in order (a role's keys, the harness default, the project-wide
/// `[model]`), then a value only the project `.env` supplied, then the built-in
/// default. Values pass through the validators the environment uses.
fn resolve(environment: &Environment, layers: &[&dyn ProviderLayer]) -> Result<RoleSettings> {
    let pick = |name: &str| crate::config::pick(environment, layers, name);
    let endpoint = pick("MOOSEDEV_LLM_BASE_URL").unwrap_or_else(|| DEFAULT_ENDPOINT.into());
    validate_provider_url(&endpoint)?;
    let model = pick("MOOSEDEV_LLM_MODEL").unwrap_or_default();
    let (_, api_key) = crate::config::api_key(environment, layers)?;
    let mut config = LlmConfig::from_values(
        Some(endpoint.trim_end_matches('/').to_owned()),
        api_key,
        None,
        pick("MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS"),
        pick("MOOSEDEV_LLM_STRUCTURED_OUTPUT"),
    )?;
    // An unset model means "choose one", never the library's default model.
    config.configured = !model.is_empty();
    config.model = model;
    config.timeouts = crate::llm::LlmTimeouts::from_values(
        pick("MOOSEDEV_LLM_CONNECT_TIMEOUT_SECS"),
        pick("MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS"),
        pick("MOOSEDEV_LLM_IDLE_TIMEOUT_SECS"),
        pick("MOOSEDEV_LLM_TOOL_ARGUMENTS_TIMEOUT_SECS"),
    )?;
    let response_policy =
        ResponsePolicy::parse(pick("MOOSEDEV_HARNESS_RESPONSE_POLICY").as_deref())?
            .unwrap_or_default();
    let action_contract =
        ActionContract::parse(pick("MOOSEDEV_HARNESS_ACTION_CONTRACT").as_deref())?;
    Ok(RoleSettings {
        config,
        response_policy,
        action_contract,
    })
}

impl ProviderSettings {
    /// Keep the composer available when persisted settings or environment values
    /// are invalid. The caller must show the original error and require a model
    /// selection; this does not silently execute with a different provider.
    pub fn fallback() -> Self {
        Self {
            response_policy: ResponsePolicy::Auto,
            action_contract: None,
            plan: None,
            implement: None,
            index_refresh: config::IndexRefresh::default(),
            config: LlmConfig {
                base_url: DEFAULT_ENDPOINT.into(),
                api_key: std::env::var(DEFAULT_API_KEY_ENV).unwrap_or_else(|_| "lm-studio".into()),
                model: String::new(),
                configured: false,
                context_window_tokens: crate::llm::DEFAULT_LLM_CONTEXT_WINDOW_TOKENS,
                structured_output: crate::llm::StructuredOutputMode::Auto,
                timeouts: Default::default(),
            },
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        Self::load_with(root, &Environment::load(root)?)
    }

    fn load_with(root: &Path, environment: &Environment) -> Result<Self> {
        let file = ModelFile::load(root)?;
        let default = resolve(environment, &[&file.default, &file.shared])
            .with_context(|| format!("[harness.model] in {}", config::FILE_NAME))?;
        let role = |role: ModelRole| {
            file.role(role)
                .map(|keys| {
                    resolve(environment, &[keys, &file.default, &file.shared]).with_context(|| {
                        format!("[harness.model.{}] in {}", role.as_str(), config::FILE_NAME)
                    })
                })
                .transpose()
        };
        Ok(Self {
            plan: role(ModelRole::Plan)?,
            implement: role(ModelRole::Implement)?,
            config: default.config,
            response_policy: default.response_policy,
            action_contract: Some(default.action_contract),
            index_refresh: file.index_refresh,
        })
    }

    /// The settings a role runs with: its own table, else the default.
    pub fn for_role(&self, role: ModelRole) -> RoleSettings {
        let own = match role {
            ModelRole::Plan => &self.plan,
            ModelRole::Implement => &self.implement,
        };
        own.clone().unwrap_or_else(|| RoleSettings {
            config: self.config.clone(),
            response_policy: self.response_policy,
            action_contract: self.action_contract.unwrap_or_default(),
        })
    }

    /// `plan=<model> implement=<model>`, or the one model when they agree.
    pub fn describe(&self) -> String {
        let [plan, implement] = ModelRole::ALL.map(|role| self.for_role(role).config.model);
        if plan == implement {
            plan
        } else {
            format!("plan={plan} implement={implement}")
        }
    }

    /// Persist a choice in `moosedev.toml` and reload, so inheritance and
    /// precedence are resolved by the one loader. `role` of `None` sets the
    /// default. Errors when a real environment variable still overrides it.
    pub fn persist_selection(
        &mut self,
        root: &Path,
        role: Option<ModelRole>,
        base_url: Option<&str>,
        model: &str,
    ) -> Result<()> {
        ensure!(!model.trim().is_empty(), "select a model ID");
        // The default table is self-contained: it records the endpoint in use
        // beside the model, and a role inherits both.
        let current = self.config.base_url.clone();
        let endpoint = base_url
            .or_else(|| role.is_none().then_some(current.as_str()))
            .map(|url| url.trim_end_matches('/'));
        if let Some(endpoint) = endpoint {
            validate_provider_url(endpoint)?;
        }
        config::set_model(root, role, endpoint, model)?;
        *self = Self::load(root)?;
        let chosen = match role {
            Some(role) => self.for_role(role).config,
            None => self.config.clone(),
        };
        ensure!(
            chosen.model == model && endpoint.is_none_or(|endpoint| chosen.base_url == endpoint),
            "saved to {}, but MOOSEDEV_LLM_MODEL or MOOSEDEV_LLM_BASE_URL in the environment overrides it; unset it to use {model}",
            config::FILE_NAME
        );
        Ok(())
    }

    pub async fn models(&self) -> Result<Vec<String>> {
        validate_provider_url(&self.config.base_url)?;
        let response: serde_json::Value = client()?
            .get(format!(
                "{}/models",
                self.config.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.config.api_key)
            .send()
            .await
            .context("connect to local model server")?
            .error_for_status()?
            .json()
            .await?;
        let data = response["data"]
            .as_array()
            .context("model discovery response has no data list")?;
        let mut models: Vec<String> = data
            .iter()
            .filter_map(|entry| entry["id"].as_str())
            .filter(|id| !id.trim().is_empty())
            .map(String::from)
            .collect();
        models.sort();
        models.dedup();
        Ok(models)
    }

    pub fn select(&mut self, base_url: Option<&str>, model: &str) -> Result<()> {
        let endpoint = base_url.unwrap_or(&self.config.base_url);
        validate_provider_url(endpoint)?;
        ensure!(!model.trim().is_empty(), "select a model ID");
        self.config.base_url = endpoint.trim_end_matches('/').into();
        self.config.model = model.into();
        self.config.configured = true;
        Ok(())
    }
}

fn validate_provider_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("invalid model server URL")?;
    ensure!(matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none(), "model server URL must use HTTP(S) without embedded secrets; configure API keys in the environment");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use serde_json::json;

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, task)
    }

    #[tokio::test]
    async fn discovery_uses_exact_model_ids_and_ignores_unusable_entries() {
        let (url, task) = serve(Router::new().route("/v1/models", get(|| async {
            Json(json!({"data":[{"id":"qwen/local-model"},{"id":"a"},{"id":"a"},{"id":""},{"name":"missing-id"}]}))
        }))).await;
        let mut settings = ProviderSettings::fallback();
        settings
            .select(Some(&format!("{url}/v1")), "qwen/local-model")
            .unwrap();
        assert_eq!(settings.models().await.unwrap(), ["a", "qwen/local-model"]);
        task.abort();
    }

    #[tokio::test]
    async fn daemon_identity_and_harness_capabilities_are_required() {
        let root = PathBuf::from("/project");
        let data_dir = root.join(".moosedev");
        let health = json!({"status":"ok","project_graph":crate::graph::PROJECT_KG_GRAPH_IRI,"project_root":"/project","data_dir":"/project/.moosedev"});
        let make_health = move || {
            let health = health.clone();
            async move { Json(health) }
        };
        let app = Router::new().route("/api/v1/health", get(make_health.clone()));
        let (old_url, old_task) = serve(app.clone()).await;
        assert!(verify_daemon(&old_url, &root, &data_dir)
            .await
            .unwrap_err()
            .to_string()
            .contains("harness API"));
        let legacy_app = app.clone().route(
            "/api/v1/harness/context",
            post(|| async {
                Json(json!({
                    "project_root":"/project",
                    "revision":"r1",
                    "context":"",
                    "files":[]
                }))
            }),
        );
        let (legacy_url, legacy_task) = serve(legacy_app).await;
        assert!(verify_daemon(&legacy_url, &root, &data_dir)
            .await
            .unwrap_err()
            .to_string()
            .contains("bounded harness context delivery"));
        let app = app.route(
            "/api/v1/harness/context",
            post(|Json(request): Json<ContextRequest>| async move {
                assert!(
                    !request.topic.trim().is_empty(),
                    "daemon context requires a nonempty topic"
                );
                assert!(
                    request.files.is_empty(),
                    "startup must not request file work"
                );
                assert!(request.evidence_only);
                assert_eq!(request.max_bytes, Some(BOUNDED_CONTEXT_PROBE_BYTES));
                Json(json!({
                    "project_root":"/project",
                    "revision":"r1",
                    "context":"",
                    "files":[],
                    "delivery_receipt": {
                        "max_bytes": BOUNDED_CONTEXT_PROBE_BYTES,
                        "context_bytes": 0,
                        "records": []
                    }
                }))
            }),
        );
        let (url, task) = serve(app).await;
        verify_daemon(&url, &root, &data_dir).await.unwrap();
        assert!(verify_daemon(&url, Path::new("/other"), &data_dir)
            .await
            .unwrap_err()
            .to_string()
            .contains("different project"));
        assert!(verify_daemon(&url, &root, Path::new("/other-store"))
            .await
            .is_err());
        task.abort();
        legacy_task.abort();
        old_task.abort();
    }

    #[tokio::test]
    async fn connection_refusal_is_stale_but_http_failures_are_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let error = verify_daemon(
            &format!("http://{addr}"),
            Path::new("/p"),
            Path::new("/p/.moosedev"),
        )
        .await
        .unwrap_err();
        assert!(connection_refused(&error), "{error:#}");
        let (url, task) = serve(Router::new()).await;
        assert!(!connection_refused(
            &verify_daemon(&url, Path::new("/p"), Path::new("/p/.moosedev"))
                .await
                .unwrap_err()
        ));
        task.abort();
    }
    #[tokio::test]
    async fn a_live_mcp_socket_without_http_reports_bind_failure_immediately() {
        let root = std::env::temp_dir().join(format!("md-bind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let socket = root.join("mcp.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            started_backend_http(&socket, &root, &root),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("bind failure"));
        assert!(error.to_string().contains("moosedev-serve.log"));
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_reclaimed_http_address_reports_safe_recovery_instructions() {
        let root = std::env::temp_dir().join(format!("md-stale-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".moosedev")).unwrap();
        let root = root.canonicalize().unwrap();
        let (url, server) = serve(Router::new()).await;
        std::fs::write(
            root.join(".moosedev/http.addr"),
            url.trim_start_matches("http://"),
        )
        .unwrap();
        let error = StartupOptions {
            root: root.clone(),
            daemon: None,
            daemon_exe: None,
        }
        .ensure_daemon()
        .await
        .unwrap_err();
        assert!(error.to_string().contains("http.addr"));
        assert!(error.to_string().contains("port was reclaimed"));
        assert!(error.to_string().contains("/connect"));
        assert!(
            root.join(".moosedev/http.addr").exists(),
            "never delete an occupied address automatically"
        );
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn executable_and_endpoint_validation_prevent_recursion_and_secret_persistence() {
        let exe = std::env::current_exe().unwrap();
        assert!(resolve_daemon_executable(Some(&exe)).is_err());
        assert!(daemon_base_url("http://127.0.0.1:42").is_ok());
        assert!(daemon_base_url("http://[::1]:42").is_ok());
        assert!(daemon_base_url("http://example.com").is_err());
        assert!(daemon_base_url("http://127.0.0.1:42/api/v1").is_err());
        assert!(validate_provider_url("http://secret@127.0.0.1/v1").is_err());
        assert!(validate_provider_url("http://127.0.0.1/v1?api_key=secret").is_err());
    }

    fn project(files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("md-provider-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        for (name, text) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    const ROLES: &str = "[harness.model]\nendpoint = \"http://127.0.0.1:1234/v1\"\nmodel = \"base\"\ncontext_window_tokens = 65536\nidle_timeout_secs = 240\ntool_arguments_timeout_secs = 900\n\n[harness.model.implement]\nmodel = \"small\"\ncontext_window_tokens = 16384\naction_contract = \"json_schema\"\nresponse_policy = \"reasoning-off\"\n";

    #[test]
    fn roles_inherit_the_default_and_override_only_their_own_keys() {
        let root = project(&[("moosedev.toml", ROLES)]);
        let settings = ProviderSettings::load_with(&root, &Environment::of(&[], &[])).unwrap();
        assert!(
            settings.plan.is_none(),
            "no plan table, so plan is the default"
        );
        let plan = settings.for_role(ModelRole::Plan);
        assert_eq!(plan.config.model, "base");
        assert_eq!(plan.config.context_window_tokens, 65536);
        assert_eq!(plan.action_contract, ActionContract::Tools);
        let implement = settings.for_role(ModelRole::Implement);
        assert_eq!(implement.config.model, "small");
        assert_eq!(implement.config.context_window_tokens, 16384);
        assert_eq!(implement.config.base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(implement.config.timeouts.idle, Duration::from_secs(240));
        assert_eq!(
            implement.config.timeouts.tool_arguments,
            Duration::from_secs(900)
        );
        assert_eq!(implement.action_contract, ActionContract::JsonSchema);
        assert_eq!(implement.response_policy, ResponsePolicy::ReasoningOff);
        assert_eq!(settings.describe(), "plan=base implement=small");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_shared_model_table_sits_beneath_every_harness_table() {
        let text = "[model]\nendpoint = \"http://127.0.0.1:1234/v1\"\nmodel = \"shared\"\ncontext_window_tokens = 65536\napi_key_env = \"SHARED_KEY\"\n\n[harness.model.plan]\nmodel = \"planner\"\n";
        let root = project(&[("moosedev.toml", text)]);
        let environment = Environment::of(&[("SHARED_KEY", "k-1")], &[]);
        let settings = ProviderSettings::load_with(&root, &environment).unwrap();
        assert_eq!(settings.describe(), "plan=planner implement=shared");
        let plan = settings.for_role(ModelRole::Plan);
        assert_eq!(plan.config.base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(plan.config.context_window_tokens, 65536);
        assert_eq!(plan.config.api_key, "k-1");
        // [harness.model] outranks [model]; a role outranks both.
        std::fs::write(
            root.join("moosedev.toml"),
            format!(
                "{text}\n[harness.model]\nmodel = \"harness\"\ncontext_window_tokens = 16384\n"
            ),
        )
        .unwrap();
        let settings = ProviderSettings::load_with(&root, &environment).unwrap();
        assert_eq!(settings.describe(), "plan=planner implement=harness");
        assert_eq!(
            settings
                .for_role(ModelRole::Plan)
                .config
                .context_window_tokens,
            16384
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_real_environment_outranks_the_file_and_the_project_dotenv_does_not() {
        let root = project(&[("moosedev.toml", ROLES)]);
        let model = "MOOSEDEV_LLM_MODEL";
        let window = "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS";
        // The daemon's model in the project .env must not flatten the roles.
        let dotenv = [(model, "daemon-model"), (window, "131072")];
        let settings = ProviderSettings::load_with(&root, &Environment::of(&[], &dotenv)).unwrap();
        assert_eq!(settings.describe(), "plan=base implement=small");
        assert_eq!(settings.config.context_window_tokens, 65536);
        // A per-invocation override still wins, for every role.
        let settings =
            ProviderSettings::load_with(&root, &Environment::of(&[(model, "one-off")], &dotenv))
                .unwrap();
        assert_eq!(settings.describe(), "one-off");
        assert_eq!(
            settings
                .for_role(ModelRole::Implement)
                .config
                .context_window_tokens,
            16384
        );
        std::fs::remove_dir_all(root).unwrap();

        // Keys the file leaves unset fall through to the project .env.
        let root = project(&[(
            "moosedev.toml",
            "[harness.model.plan]\nmodel = \"planner\"\n",
        )]);
        let settings = ProviderSettings::load_with(&root, &Environment::of(&[], &dotenv)).unwrap();
        assert_eq!(settings.describe(), "plan=planner implement=daemon-model");
        assert_eq!(
            settings
                .for_role(ModelRole::Plan)
                .config
                .context_window_tokens,
            131072
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn without_a_file_or_environment_no_model_is_chosen() {
        let root = project(&[]);
        let settings = ProviderSettings::load_with(&root, &Environment::of(&[], &[])).unwrap();
        assert!(settings.config.model.is_empty() && !settings.config.configured);
        assert_eq!(settings.config.base_url, DEFAULT_ENDPOINT);
        assert_eq!(settings.response_policy, ResponsePolicy::default());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn api_keys_come_only_from_the_variable_the_file_names() {
        let text = "[harness.model]\nmodel = \"local\"\n\n[harness.model.plan]\nmodel = \"hosted\"\napi_key_env = \"PLANNER_KEY\"\n";
        let root = project(&[("moosedev.toml", text)]);
        let error = ProviderSettings::load_with(&root, &Environment::of(&[], &[])).unwrap_err();
        assert!(
            format!("{error:#}").contains("PLANNER_KEY, which is not set"),
            "{error:#}"
        );
        let environment = Environment::of(&[("PLANNER_KEY", "secret-1")], &[]);
        let settings = ProviderSettings::load_with(&root, &environment).unwrap();
        assert_eq!(
            settings.for_role(ModelRole::Plan).config.api_key,
            "secret-1"
        );
        assert_eq!(
            settings.for_role(ModelRole::Implement).config.api_key,
            "lm-studio"
        );
        std::fs::write(
            root.join("moosedev.toml"),
            "[harness.model]\napi_key_env = \"sk-live key\"\n",
        )
        .unwrap();
        let error = ProviderSettings::load_with(&root, &environment).unwrap_err();
        assert!(format!("{error:#}").contains("not hold a key"), "{error:#}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_file_values_fail_with_their_table() {
        for (text, expected) in [
            (
                "[harness.model]\ncontext_window_tokens = 100\n",
                "[harness.model] in moosedev.toml",
            ),
            (
                "[harness.model.plan]\naction_contract = \"xml\"\n",
                "[harness.model.plan] in moosedev.toml",
            ),
            (
                "[harness.model]\nendpoint = \"http://user:pw@host/v1\"\n",
                "without embedded secrets",
            ),
            (
                "[harness.model]\nidle_timeout_secs = 0\n",
                "MOOSEDEV_LLM_IDLE_TIMEOUT_SECS",
            ),
        ] {
            let root = project(&[("moosedev.toml", text)]);
            let error = ProviderSettings::load_with(&root, &Environment::of(&[], &[])).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "{text:?}: {error:#}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn detached_startup_uses_server_executable_and_selected_project() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("md-spawn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let exe = root.join("fake-moosedev");
        std::fs::write(
            &exe,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$MOOSEDEV_DATA_DIR\" \"$1\" \"$2\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data_dir = root.join(".moosedev");
        let socket = runtime::socket_path_for(&data_dir);
        let mut child =
            runtime::spawn_detached_backend_with_exe(&socket, &data_dir, &exe, &root).unwrap();
        assert!(child.wait().unwrap().success());
        let log = std::fs::read_to_string(runtime::serve_log_path_for(&data_dir)).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                root.to_str().unwrap(),
                data_dir.to_str().unwrap(),
                "--serve",
                socket.to_str().unwrap()
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
