//! The approved change scope derived from the plan alone: obligations are the
//! direct dossier records of the plan files' resolved definitions.
use super::super::scope::{ApprovedChangeScope, ApprovedDefinitionScope};
use super::super::{ContextResponse, Runner};
use crate::harness::daemon::intent::{IntentResolveRequest, IntentResolveResponse};
use crate::harness::digest::sha256_json;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};

impl Runner {
    /// Derive the approved change scope from the plan alone: obligations are
    /// the direct dossier records of the plan files' resolved definitions and
    /// the purpose is the plan summary. No model call, one human approval.
    pub(in crate::harness::runner) async fn derive_symbolic_scope(
        &mut self,
        context: &ContextResponse,
    ) -> Result<()> {
        let plan = self.task.plan.as_ref().context("no plan")?.clone();
        let resolved: IntentResolveResponse = self
            .post(
                "intent/resolve",
                &IntentResolveRequest {
                    files: plan.files.clone(),
                    refresh_index: false,
                },
            )
            .await?;
        anyhow::ensure!(
            resolved.revision == context.revision,
            "knowledge changed while deriving obligations; approve the plan again"
        );
        let mut obligations: BTreeMap<String, BTreeSet<String>> = plan
            .files
            .iter()
            .map(|file| (file.clone(), BTreeSet::new()))
            .collect();
        let mut definition_scopes = Vec::new();
        for entity in &resolved.entities {
            let records: BTreeSet<_> = entity.dossier_records.iter().cloned().collect();
            obligations
                .entry(entity.file.clone())
                .or_default()
                .extend(records.iter().cloned());
            definition_scopes.push(ApprovedDefinitionScope {
                file: entity.file.clone(),
                symbol: entity.symbol.clone(),
                source_digest: entity.source_digest.clone(),
            });
        }
        let obligations: BTreeMap<String, Vec<String>> = obligations
            .into_iter()
            .map(|(file, records)| (file, records.into_iter().collect()))
            .collect();
        let obligation_iris: Vec<String> = obligations
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let obligations_digest = sha256_json(&obligations)?;
        self.task.approved_change_scope = Some(ApprovedChangeScope {
            version: 2,
            knowledge_revision: context.revision.clone(),
            files: self.task.snapshots.clone(),
            obligation_iris: obligation_iris.clone(),
            definition_scopes,
            checks: plan.checks.clone(),
            approval_cycle: self.task.intent_cycle.clone().unwrap_or_default(),
        });
        let state = self.symbolic_state_mut();
        state.obligations = obligations;
        state.obligations_digest = obligations_digest.clone();
        state.knowledge_revision = context.revision.clone();
        let detail = format!(
            "{} files, {} governing records, {} definitions, revision {}, digest {obligations_digest}",
            plan.files.len(),
            obligation_iris.len(),
            resolved.entities.len(),
            context.revision,
        );
        self.intent_event("obligations_derived", &detail);
        if !resolved.unresolved.is_empty() {
            self.intent_event(
                "obligations_unresolved",
                &format!("no index evidence for {}", resolved.unresolved.join(", ")),
            );
        }
        Ok(())
    }
}
