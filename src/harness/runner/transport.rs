//! The loopback daemon client: HTTP failures, context refresh, checkpoints
//! and source snapshots.
use super::*;

#[derive(Debug)]
pub(super) struct HttpFailure {
    pub(super) status: u16,
    pub(super) message: String,
}
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "daemon HTTP {}: {}", self.status, self.message)
    }
}
impl std::error::Error for HttpFailure {}

/// Typed class of a failed step, derived by error-type downcast only. Order
/// matters: a daemon 400 re-wrapped as `InvalidModelOutput` spends repair
/// budget and stays `model_output`.
pub(super) fn error_kind(error: &anyhow::Error) -> &'static str {
    if error.is::<model::InvalidModelOutput>() {
        "model_output"
    } else if let Some(failure) = error.downcast_ref::<HttpFailure>() {
        if (400..500).contains(&failure.status) {
            "daemon_rejection"
        } else {
            "service"
        }
    } else if error.is::<reqwest::Error>() || error.is::<std::io::Error>() {
        "service"
    } else {
        "other"
    }
}

impl Runner {
    pub(super) fn client(daemon: &str) -> Result<reqwest::Client> {
        let url = reqwest::Url::parse(daemon)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "invalid daemon URL scheme"
        );
        anyhow::ensure!(
            matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            ),
            "harness requires a loopback project daemon"
        );
        Ok(reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?)
    }

    pub(super) async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let response = self
            .http
            .post(format!("{}/api/v1/harness/{path}", self.daemon))
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(HttpFailure {
                status: status.as_u16(),
                message: format!("{path}: {}", bounded(&text, 4000)),
            }
            .into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    pub(super) async fn checkpoint(&self, operation: Option<&str>) -> Result<CheckpointResponse> {
        let mut request = self
            .http
            .post(format!("{}/api/v1/harness/checkpoint", self.daemon));
        if let Some(id) = operation {
            request = request.query(&[("operation_id", id)]);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "checkpoint failed: {status}: {}",
            bounded(&text, 4000)
        );
        Ok(serde_json::from_str(&text)?)
    }

    pub(super) async fn refresh(&mut self, files: &[String]) -> Result<ContextResponse> {
        let response: ContextResponse = self
            .post(
                "context",
                &ContextRequest {
                    topic: format!("{} {}", self.task.objective, self.task.guidance)
                        .trim()
                        .to_owned(),
                    files: files.to_vec(),
                },
            )
            .await?;
        anyhow::ensure!(
            Path::new(&response.project_root).canonicalize()? == self.workspace.root(),
            "daemon belongs to a different project"
        );
        anyhow::ensure!(
            files
                .iter()
                .all(|file| response.files.iter().any(|entry| &entry.file == file)),
            "daemon omitted requested file context"
        );
        self.update_knowledge_revision(response.revision.clone());
        self.context = Some(response.clone());
        Ok(response)
    }

    pub(super) fn snapshot(&self, files: &[String]) -> Result<BTreeMap<String, Option<String>>> {
        files
            .iter()
            .map(|file| Ok((file.clone(), fingerprint(&self.workspace.read(file)?))))
            .collect()
    }
}
