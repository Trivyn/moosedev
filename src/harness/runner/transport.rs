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
    fn human_query(&self) -> String {
        if self.task.guidance.trim().is_empty() {
            self.task.objective.clone()
        } else {
            self.task.guidance.clone()
        }
    }

    fn ensure_knowledge_turn(&mut self, topic: &str, revision: &str) -> &mut KnowledgeTurn {
        let sequence = self.task.knowledge_turn_sequence;
        if self.task.knowledge_turns.last().map(|turn| turn.sequence) != Some(sequence) {
            self.task.knowledge_turns.push(KnowledgeTurn {
                sequence,
                query: self.human_query(),
                retrieval_topic: topic.to_owned(),
                revision: revision.to_owned(),
                records: Vec::new(),
                files: Vec::new(),
                searches: Vec::new(),
            });
        }
        self.task.knowledge_turns.last_mut().unwrap()
    }

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
        let topic = format!("{} {}", self.task.objective, self.task.guidance)
            .trim()
            .to_owned();
        let response: ContextResponse = self
            .post(
                "context",
                &ContextRequest {
                    topic: topic.clone(),
                    files: files.to_vec(),
                    evidence_only: false,
                    max_bytes: None,
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
        self.task.knowledge_context = Some(KnowledgeContextSnapshot {
            topic: topic.clone(),
            revision: response.revision.clone(),
            context: response.context.clone(),
            files: response
                .files
                .iter()
                .map(|file| KnowledgeFileDossier {
                    file: file.file.clone(),
                    dossier: file.dossier.clone(),
                })
                .collect(),
            governing_constraints: response.governing_constraints.clone(),
            records: response.records.clone(),
            delivery_receipt: response.delivery_receipt.clone(),
        });
        let turn = self.ensure_knowledge_turn(&topic, &response.revision);
        turn.retrieval_topic = topic;
        turn.revision = response.revision.clone();
        turn.records = response.records.clone();
        turn.files = response
            .files
            .iter()
            .map(|file| file.file.clone())
            .collect();
        self.context = Some(response.clone());
        Ok(response)
    }

    /// Where the code index defines what an edit compares, and which compared
    /// literals it does not define.
    pub(super) async fn ground(&self, request: &GroundRequest) -> Result<GroundResponse> {
        self.post("ground", request).await
    }

    /// Where the code index defines what a disputed plan's text compares, and
    /// which compared literals it does not define.
    pub(super) async fn ground_plan(
        &self,
        request: &crate::harness::protocol::PlanGroundRequest,
    ) -> Result<GroundResponse> {
        self.post("ground/plan", request).await
    }

    /// Accepted knowledge for a search query, without the inventory or file
    /// dossiers. The current working context is left as it is.
    pub(super) async fn search_knowledge(
        &mut self,
        query: &str,
        max_bytes: Option<usize>,
    ) -> Result<KnowledgeSearchResult> {
        let response: ContextResponse = self
            .post(
                "context",
                &ContextRequest {
                    topic: query.to_owned(),
                    files: vec![],
                    evidence_only: true,
                    max_bytes,
                },
            )
            .await?;
        anyhow::ensure!(
            Path::new(&response.project_root).canonicalize()? == self.workspace.root(),
            "daemon belongs to a different project"
        );
        self.update_knowledge_revision(response.revision.clone());
        let search = KnowledgeSearchResult {
            query: query.to_owned(),
            revision: response.revision.clone(),
            context: response.context.clone(),
            evidence_iris: response.evidence_iris.clone(),
            records: response.records.clone(),
            delivery_receipt: response.delivery_receipt.clone(),
        };
        self.task.knowledge_searches.push(search.clone());
        let topic = format!("{} {}", self.task.objective, self.task.guidance)
            .trim()
            .to_owned();
        self.ensure_knowledge_turn(&topic, &response.revision)
            .searches
            .push(search.clone());
        Ok(search)
    }

    pub(super) fn snapshot(&self, files: &[String]) -> Result<BTreeMap<String, Option<String>>> {
        files
            .iter()
            .map(|file| Ok((file.clone(), fingerprint(&self.workspace.read(file)?))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{context_router, serve, Project};
    use super::*;

    #[tokio::test]
    async fn refresh_replaces_current_context_while_searches_accumulate() {
        let project = Project::new("knowledge-view");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Explain graph context".into())
            .await
            .unwrap();

        let current = runner.task.knowledge_context.as_ref().unwrap();
        assert_eq!(current.topic, "Explain graph context");
        assert!(runner.task.knowledge_searches.is_empty());
        assert_eq!(runner.task.knowledge_turns.len(), 1);
        assert_eq!(
            runner.task.knowledge_turns[0].query,
            "Explain graph context"
        );

        runner.search_knowledge("constraints", None).await.unwrap();
        runner.search_knowledge("lessons", None).await.unwrap();
        assert_eq!(runner.task.knowledge_searches.len(), 2);
        assert_eq!(runner.task.knowledge_searches[0].query, "constraints");
        assert_eq!(runner.task.knowledge_searches[1].query, "lessons");
        assert_eq!(runner.task.knowledge_turns[0].searches.len(), 2);
        assert_eq!(
            runner.task.knowledge_context.as_ref().unwrap().topic,
            "Explain graph context"
        );

        runner.task.guidance = "Focus on the harness".into();
        runner.refresh(&[]).await.unwrap();
        assert_eq!(
            runner.task.knowledge_context.as_ref().unwrap().topic,
            "Explain graph context Focus on the harness"
        );
        assert_eq!(runner.task.knowledge_searches.len(), 2);
        assert_eq!(runner.task.knowledge_turns.len(), 1);
        assert_eq!(
            runner.task.knowledge_turns[0].query,
            "Explain graph context"
        );
        assert_eq!(runner.task.knowledge_turns[0].searches.len(), 2);

        runner.task.knowledge_turn_sequence += 1;
        runner.task.guidance = "Focus on the harness".into();
        runner.refresh(&[]).await.unwrap();
        assert_eq!(runner.task.knowledge_turns.len(), 2);
        assert_eq!(runner.task.knowledge_turns[1].sequence, 1);
        assert_eq!(runner.task.knowledge_turns[1].query, "Focus on the harness");
        assert!(runner.task.knowledge_turns[1].searches.is_empty());
        server.abort();
    }
}
