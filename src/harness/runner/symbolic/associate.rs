//! Post-edit associations without a model: bindings derived by the daemon
//! from the diff and the obligations, journaled, then put through the
//! existing link review.
use super::super::scope::changed_files;
use super::super::Runner;
use super::SymbolicAssociation;
use crate::harness::daemon::intent::{IntentBinding, IntentLinkRequest};
use crate::harness::protocol::{
    AssociatePage, AssociateRequest, IntentIndexStatus, IntentRefreshPolicy,
};
use anyhow::{bail, Context, Result};

impl Runner {
    /// Post-edit associations without a model: derive bindings from the diff
    /// and the obligations, journal each, then reuse the existing link review.
    pub(in crate::harness::runner) async fn prepare_symbolic_associations(
        &mut self,
    ) -> Result<bool> {
        if self.task.edits.is_empty() {
            return Ok(false);
        }
        let existing = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.association.clone());
        let association = match existing {
            Some(association) => association,
            None => {
                let governing = self
                    .task
                    .symbolic
                    .as_ref()
                    .map(|state| state.obligations.clone())
                    .unwrap_or_default();
                let request = AssociateRequest {
                    files: changed_files(&self.task.edits)?,
                    governing,
                    refresh_policy: IntentRefreshPolicy::SupportedFrozen,
                    knowledge_revision: self.task.knowledge_revision.clone(),
                };
                let page: AssociatePage = self.post("intent/associate", &request).await?;
                anyhow::ensure!(
                    page.knowledge_revision == self.task.knowledge_revision,
                    "knowledge changed during association derivation; retry"
                );
                for binding in &page.bindings {
                    let detail = format!(
                        "{} {} -{}-> {} ({:?} basis, {})",
                        binding.file,
                        binding.name.as_deref().unwrap_or(&binding.symbol),
                        binding.predicate,
                        binding.record_iri,
                        binding.basis,
                        binding.candidate_digest
                    );
                    self.intent_event("association_derived", &detail);
                }
                for file in &page.ungoverned {
                    self.intent_event("association_none", file);
                }
                if !page.skipped.is_empty() {
                    let mut counts = std::collections::BTreeMap::new();
                    for item in &page.skipped {
                        *counts.entry(format!("{:?}", item.reason)).or_insert(0usize) += 1;
                    }
                    self.intent_event("association_skipped", &format!("{counts:?}"));
                }
                if !page.unresolved.is_empty() || page.index.status != IntentIndexStatus::Current {
                    // Journaled, never parked: a stale or missing index costs
                    // links, not the task.
                    self.intent_event(
                        "association_unresolved",
                        &format!(
                            "index {:?}; {}",
                            page.index.status,
                            serde_json::to_string(&page.unresolved)?
                        ),
                    );
                }
                let status = if page.bindings.is_empty() {
                    "resolved"
                } else {
                    "derived"
                };
                let association = SymbolicAssociation {
                    page,
                    status: status.into(),
                    link_operation_id: None,
                };
                self.symbolic_state_mut().association = Some(association.clone());
                self.persist()?;
                association
            }
        };
        match association.status.as_str() {
            "resolved" => Ok(false),
            "awaiting_review" => self.prepare_intent_links().await,
            "derived" => {
                let mut bindings: Vec<IntentBinding> = Vec::new();
                for derived in &association.page.bindings {
                    let binding = IntentBinding::from_derived(derived);
                    if !bindings.contains(&binding) {
                        bindings.push(binding);
                    }
                }
                let request = IntentLinkRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    revision: self.task.knowledge_revision.clone(),
                    bindings,
                };
                let operation_id = request.operation_id.clone();
                self.task.pending_intent_links = Some(request);
                let association = self
                    .symbolic_state_mut()
                    .association
                    .as_mut()
                    .context("symbolic association state missing")?;
                association.link_operation_id = Some(operation_id);
                association.status = "awaiting_review".into();
                self.persist()?;
                self.prepare_intent_links().await
            }
            other => bail!("unknown symbolic association status {other}"),
        }
    }

    pub(in crate::harness::runner) fn symbolic_associations_resolved(&self) -> bool {
        self.task
            .symbolic
            .as_ref()
            .and_then(|state| state.association.as_ref())
            .is_some_and(|association| association.status == "resolved")
    }

    pub(in crate::harness::runner) fn resolve_symbolic_association(&mut self, operation_id: &str) {
        if let Some(association) = self
            .task
            .symbolic
            .as_mut()
            .and_then(|state| state.association.as_mut())
            .filter(|association| association.link_operation_id.as_deref() == Some(operation_id))
        {
            association.link_operation_id = None;
            association.status = "resolved".into();
        }
    }
}
