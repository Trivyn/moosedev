//! Interactive client startup. Discovery never opens the graph's writer store.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::protocol::{ContextRequest, ContextResponse};
use crate::{llm::LlmConfig, runtime};

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
        format!("the daemon started but its harness HTTP API failed to start; check MOOSEDEV_HTTP_ADDR and {} for a bind failure, then restart the daemon", runtime::serve_log_path_for(data_dir).display())
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

#[derive(Clone, Debug)]
pub struct ProviderSettings {
    pub config: LlmConfig,
}

#[derive(Default, Serialize, Deserialize)]
struct RememberedProvider {
    base_url: String,
    model: String,
}

impl ProviderSettings {
    /// Keep the composer available when persisted settings or environment values
    /// are invalid. The caller must show the original error and require a model
    /// selection; this does not silently execute with a different provider.
    pub fn fallback() -> Self {
        Self {
            config: LlmConfig {
                base_url: "http://127.0.0.1:1234/v1".into(),
                api_key: std::env::var("MOOSEDEV_LLM_API_KEY")
                    .unwrap_or_else(|_| "lm-studio".into()),
                model: String::new(),
                configured: false,
                context_window_tokens: crate::llm::DEFAULT_LLM_CONTEXT_WINDOW_TOKENS,
                structured_output: crate::llm::StructuredOutputMode::Auto,
            },
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(".moosedev/harness/provider.json");
        let remembered: RememberedProvider = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("invalid remembered provider settings")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                RememberedProvider::default()
            }
            Err(error) => return Err(error.into()),
        };
        let mut config = LlmConfig::from_env()?;
        if std::env::var("MOOSEDEV_LLM_BASE_URL").map_or(true, |s| s.trim().is_empty()) {
            config.base_url = if remembered.base_url.is_empty() {
                "http://127.0.0.1:1234/v1".into()
            } else {
                remembered.base_url
            };
        }
        if std::env::var("MOOSEDEV_LLM_MODEL").map_or(true, |s| s.trim().is_empty()) {
            config.model = remembered.model;
        }
        validate_provider_url(&config.base_url)?;
        config.configured = !config.model.is_empty();
        Ok(Self { config })
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

    pub fn save(&self, root: &Path) -> Result<()> {
        let directory = root.join(".moosedev/harness");
        for dir in [root.join(".moosedev"), directory.clone()] {
            if let Ok(metadata) = std::fs::symlink_metadata(&dir) {
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "provider settings directory must not be a symlink"
                );
            } else {
                std::fs::create_dir(&dir)?;
            }
        }
        let temporary = directory.join(format!("provider-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec_pretty(&RememberedProvider {
            base_url: self.config.base_url.clone(),
            model: self.config.model.clone(),
        })?)?;
        file.sync_all()?;
        std::fs::rename(temporary, directory.join("provider.json"))?;
        std::fs::File::open(directory)?.sync_all()?;
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
        let mut settings = ProviderSettings {
            config: LlmConfig::from_env().unwrap(),
        };
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
                Json(json!({"project_root":"/project","revision":"r1","context":"","files":[]}))
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

    #[test]
    fn remembered_settings_never_include_api_key() {
        let root = std::env::temp_dir().join(format!("md-provider-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut settings = ProviderSettings {
            config: LlmConfig::from_env().unwrap(),
        };
        settings.config.api_key = "do-not-store-this-key".into();
        settings
            .select(Some("http://127.0.0.1:1234/v1"), "exact-id")
            .unwrap();
        settings.save(&root).unwrap();
        let saved = std::fs::read_to_string(root.join(".moosedev/harness/provider.json")).unwrap();
        assert!(!saved.contains("do-not-store-this-key"));
        assert!(!saved.contains("api_key"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&saved).unwrap()["model"],
            "exact-id"
        );
        std::fs::remove_dir_all(root).unwrap();
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
