//! Experimental change-plan metadata. Graph identity and link validation stay in the daemon.
use super::*;
use crate::harness::daemon::intent::{
    IntentBinding, IntentLinkRequest, IntentLinkResponse, IntentResolveRequest,
    IntentResolveResponse,
};
use serde_json::json;
use std::collections::BTreeSet;

const MAX_REFERENCES: usize = 16;
const MAX_TARGETS: usize = 32;

fn requires_entity(file: &str) -> bool {
    let path = Path::new(file);
    crate::code::substrate::lang::fallback_for_path(path).is_some()
        || path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|extension| {
                crate::code::substrate::registry()
                    .iter()
                    .any(|p| p.extensions.contains(&extension))
            })
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntentPolicy {
    #[default]
    Current,
    ChangeLevel,
    ChangeLevelV2,
    /// The coding model answers only actions and one final capture note; the
    /// daemon derives purpose, obligations, associations and capture typing.
    Symbolic,
}
impl IntentPolicy {
    /// Policies whose post-edit association facility is part of the contract
    /// rather than an optional environment setting.
    pub fn mandates_postedit_associations(self) -> bool {
        matches!(self, Self::ChangeLevelV2 | Self::Symbolic)
    }
}
pub(super) fn postedit_associations_from_env(policy: IntentPolicy) -> Result<u8> {
    let mandatory = policy.mandates_postedit_associations();
    match std::env::var("MOOSEDEV_HARNESS_POSTEDIT_ASSOCIATIONS") {
        Err(std::env::VarError::NotPresent) if mandatory => Ok(1),
        Err(std::env::VarError::NotPresent) => Ok(0),
        Ok(value) if value == "0" && !mandatory => Ok(0),
        Ok(value) if value == "1" => Ok(1),
        Ok(value) if value == "0" => {
            bail!("{} includes mandatory post-edit associations; remove the conflicting 0 setting or use 1", policy.env_name())
        }
        _ => bail!("MOOSEDEV_HARNESS_POSTEDIT_ASSOCIATIONS must be 0 or 1"),
    }
}
pub(super) fn entity_links_from_env() -> Result<bool> {
    match std::env::var("MOOSEDEV_HARNESS_ENTITY_LINKS") {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Ok(value) if value == "0" => Ok(false),
        Ok(value) if value == "1" => Ok(true),
        _ => bail!("MOOSEDEV_HARNESS_ENTITY_LINKS must be 0 or 1"),
    }
}

impl IntentPolicy {
    pub fn from_env() -> Result<Self> {
        match std::env::var("MOOSEDEV_HARNESS_INTENT_POLICY") {
            Err(std::env::VarError::NotPresent) => Ok(Self::Current),
            Ok(value) if value == "current" => Ok(Self::Current),
            Ok(value) if value == "change-level" => Ok(Self::ChangeLevel),
            Ok(value) if value == "change-level-v2" => Ok(Self::ChangeLevelV2),
            Ok(value) if value == "symbolic" => Ok(Self::Symbolic),
            _ => bail!(
                "MOOSEDEV_HARNESS_INTENT_POLICY must be current, change-level, change-level-v2, or symbolic"
            ),
        }
    }

    /// The `MOOSEDEV_HARNESS_INTENT_POLICY` spelling of this policy.
    pub fn env_name(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::ChangeLevel => "change-level",
            Self::ChangeLevelV2 => "change-level-v2",
            Self::Symbolic => "symbolic",
        }
    }
}

/// Roles are journal metadata, never new graph predicates. Model handles are
/// resolved into durable record IRIs and indexed symbols before storing a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeIntent {
    pub purpose: Vec<String>,
    pub obligations: Vec<String>,
    pub targets: Vec<ChangeTarget>,
    pub missing: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeTarget {
    pub file: String,
    /// A supplied entity handle, resolved to a symbol; or a planned entity name.
    pub entity: String,
    pub planned: bool,
    pub records: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentEvent {
    pub id: String,
    pub cycle: Option<String>,
    pub kind: String,
    pub detail: String,
}

impl Runner {
    pub fn set_intent_policy(&mut self, policy: IntentPolicy) -> Result<()> {
        anyhow::ensure!(
            self.task.plan.is_none() && self.task.steps == 0,
            "intent policy is fixed once task planning begins"
        );
        self.task.intent_policy = policy;
        if policy.mandates_postedit_associations() {
            self.task.postedit_association_contract = 1;
        }
        self.persist()
    }

    pub(super) fn update_knowledge_revision(&mut self, revision: String) {
        if !self.task.knowledge_revision.is_empty() && self.task.knowledge_revision != revision {
            self.intent_event(
                "knowledge_revision_changed",
                &format!("{} -> {}", self.task.knowledge_revision, revision),
            );
        }
        self.task.knowledge_revision = revision;
    }

    pub(super) fn intent_event(&mut self, kind: &str, detail: &str) {
        // Both arms expose gate costs; only treatment emits mapping events.
        self.task.intent_events.push(IntentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            cycle: self.task.intent_cycle.clone(),
            kind: kind.to_string(),
            detail: bounded(detail, 2000),
        });
    }
    pub(super) fn start_intent_cycle(&mut self) {
        if self.task.intent_cycle.is_none() {
            self.task.intent_cycle = Some(uuid::Uuid::new_v4().to_string());
            self.intent_event("cycle_started", "entered plan review");
        }
    }
    pub(super) fn end_intent_cycle(&mut self, reason: &str) {
        if self.task.intent_cycle.is_some() {
            self.intent_event("cycle_ended", reason);
            self.task.intent_cycle = None;
        }
    }
    pub(super) async fn refresh_intent_choices(
        &mut self,
        files: &[String],
        refresh_index: bool,
    ) -> Result<()> {
        if self.task.postedit_association_contract == 1 {
            return Ok(());
        }
        if !self.task.entity_links && self.task.intent_policy == IntentPolicy::Current {
            return Ok(());
        }
        let files: Vec<_> = files
            .iter()
            .filter(|f| requires_entity(f))
            .cloned()
            .collect();
        anyhow::ensure!(files.len() <= 32, "intent entity discovery supports at most 32 code files; use /plan to narrow the working set");
        let files = files.as_slice();
        let mut response: IntentResolveResponse = self
            .post(
                "intent/resolve",
                &IntentResolveRequest {
                    files: files.to_vec(),
                    refresh_index,
                },
            )
            .await?;
        if !refresh_index && !files.is_empty() && !response.unresolved.is_empty() {
            response = self
                .post(
                    "intent/resolve",
                    &IntentResolveRequest {
                        files: files.to_vec(),
                        refresh_index: true,
                    },
                )
                .await?;
        }
        anyhow::ensure!(
            response.revision == self.task.knowledge_revision,
            "knowledge changed while resolving intent choices; retry with current context"
        );
        self.intent_choices = Some(response);
        Ok(())
    }

    pub(super) fn resolve_change_intent(
        &mut self,
        proposal: Option<ChangeIntent>,
        files: &[String],
    ) -> Result<Option<ChangeIntent>> {
        if matches!(
            self.task.intent_policy,
            IntentPolicy::Current | IntentPolicy::ChangeLevelV2 | IntentPolicy::Symbolic
        ) {
            anyhow::ensure!(
                proposal.is_none(),
                "change_intent is unavailable under the current policy"
            );
            return Ok(None);
        }
        self.resolve_intent_metadata(proposal, files).map(Some)
    }

    fn resolve_intent_metadata(
        &mut self,
        proposal: Option<ChangeIntent>,
        files: &[String],
    ) -> Result<ChangeIntent> {
        let mut mapping =
            proposal.context("change-level policy requires change_intent plan metadata")?;
        let choices = self
            .intent_choices
            .as_ref()
            .context("intent choices unavailable")?;
        anyhow::ensure!(
            choices.revision == self.task.knowledge_revision,
            "intent choices are stale"
        );
        anyhow::ensure!(
            mapping.purpose.len() <= MAX_REFERENCES && mapping.obligations.len() <= MAX_REFERENCES,
            "purpose and obligations allow at most 16 references each"
        );
        anyhow::ensure!(
            mapping.targets.len() <= MAX_TARGETS,
            "intent allows at most 32 affected entities; helpers may share one justification"
        );
        if let Some(missing) = &mapping.missing {
            anyhow::ensure!(!missing.trim().is_empty() && missing.len() <= 2000,
                "missing intent must explain the public task's unsupported purpose in 1..2000 bytes");
        }
        anyhow::ensure!(
            !mapping.purpose.is_empty() || mapping.missing.is_some(),
            "select existing purpose records or explain genuinely missing intent"
        );
        let resolve = |handle: &str| -> Result<String> {
            let index = handle
                .strip_prefix('r')
                .context("select a supplied rN record handle")?
                .parse::<usize>()?;
            let record = choices
                .records
                .get(index)
                .context("unknown intent record handle")?;
            Ok(record.iri.clone())
        };
        mapping.purpose = mapping
            .purpose
            .iter()
            .map(|r| resolve(r))
            .collect::<Result<_>>()?;
        mapping.obligations = mapping
            .obligations
            .iter()
            .map(|r| resolve(r))
            .collect::<Result<_>>()?;
        let roles: BTreeSet<_> = mapping
            .purpose
            .iter()
            .chain(&mapping.obligations)
            .cloned()
            .collect();
        anyhow::ensure!(
            mapping
                .targets
                .iter()
                .map(|target| target.records.len())
                .sum::<usize>()
                <= 128,
            "one change-level mapping allows at most 128 explicit associations"
        );
        let mut covered = BTreeSet::new();
        for target in &mut mapping.targets {
            anyhow::ensure!(
                files.contains(&target.file),
                "intent target is outside planned file scope"
            );
            anyhow::ensure!(target.records.len() <= MAX_REFERENCES && (!target.records.is_empty() || mapping.missing.is_some()),
                "each affected entity requires record references or an explicit missing-intent explanation");
            target.records = target
                .records
                .iter()
                .map(|r| resolve(r))
                .collect::<Result<_>>()?;
            anyhow::ensure!(
                target.records.iter().all(|r| roles.contains(r)),
                "target references must have a purpose or obligation role in this plan"
            );
            if target.planned {
                anyhow::ensure!(
                    !target.entity.trim().is_empty()
                        && target.entity.len() <= 200
                        && !target.entity.contains(['\n', '\r']),
                    "planned entity needs a short name, not a symbol or IRI"
                );
                target.source_digest = None;
            } else {
                let entity = choices
                    .entities
                    .iter()
                    .find(|e| e.handle == target.entity && e.file == target.file)
                    .context("select an indexed entity handle for this file; read it first")?;
                let actual = fingerprint(&self.workspace.read(&target.file)?);
                anyhow::ensure!(
                    actual.as_deref() == Some(entity.source_digest.as_str()),
                    "indexed entity source proof is stale; refresh indexing before planning"
                );
                target.entity = entity.symbol.clone();
                target.source_digest = Some(entity.source_digest.clone());
            }
            covered.insert(target.file.clone());
        }
        anyhow::ensure!(
            files.iter().filter(|f| requires_entity(f)).all(|f| covered.contains(f)),
            "every planned code file needs at least one affected existing or planned entity; accompanying non-code files inherit the change purpose"
        );
        if self.task.intent_policy == IntentPolicy::ChangeLevel {
            self.intent_event(
                if mapping.missing.is_some() {
                    "intent_missing"
                } else {
                    "intent_reused"
                },
                &serde_json::to_string(&mapping)?,
            );
        }
        Ok(mapping)
    }

    pub(super) fn resolve_association_targets(
        &mut self,
        targets: Vec<ChangeTarget>,
    ) -> Result<Vec<ChangeTarget>> {
        anyhow::ensure!(
            self.task.entity_links || self.task.intent_policy == IntentPolicy::ChangeLevel,
            "association action is unavailable"
        );
        anyhow::ensure!(
            targets.iter().all(|t| !t.planned),
            "associate selects current indexed entities; planned targets belong in change_intent"
        );
        let files = targets
            .iter()
            .map(|t| t.file.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let purpose = targets
            .iter()
            .flat_map(|t| t.records.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(self
            .resolve_intent_metadata(
                Some(ChangeIntent {
                    purpose,
                    obligations: vec![],
                    targets,
                    missing: None,
                }),
                &files,
            )?
            .targets)
    }

    pub(super) async fn propose_associations(&mut self, targets: Vec<ChangeTarget>) -> Result<()> {
        if self.review_before_associations()? {
            return Ok(());
        }
        let mut bindings: Vec<IntentBinding> = targets
            .into_iter()
            .flat_map(|target| {
                target
                    .records
                    .into_iter()
                    .map(move |record_iri| IntentBinding {
                        record_iri,
                        file: target.file.clone(),
                        symbol: Some(target.entity.clone()),
                        planned_name: None,
                        source_digest: target.source_digest.clone(),
                    })
            })
            .collect();
        let mut unique = Vec::new();
        for binding in bindings.drain(..) {
            if !unique.contains(&binding) {
                unique.push(binding);
            }
        }
        self.task.pending_intent_links = Some(IntentLinkRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            revision: self.task.knowledge_revision.clone(),
            bindings: unique,
        });
        self.persist()?;
        self.prepare_intent_links(false).await?;
        Ok(())
    }

    pub(super) fn intent_prompt(&self, budget: usize) -> Result<String> {
        if self.task.postedit_association_contract == 1 {
            return Ok(String::new());
        }
        if !self.task.entity_links && self.task.intent_policy == IntentPolicy::Current {
            return Ok(String::new());
        }
        let choices = self
            .intent_choices
            .as_ref()
            .context("intent choices unavailable")?;
        let mut prompt = String::from("\nCode associations: associate(targets:[{file,entity,planned:false,records:[record handles]}]) proposes reviewed links to existing knowledge. Read files to get indexed entity handles. Use the applicable subset of records per entity; helpers may share justification. Reuse existing records rather than minting duplicates. A link records relevance, not proof of correct implementation.\n");
        if self.task.intent_policy == IntentPolicy::ChangeLevel {
            prompt.push_str("Experimental policy: plan requires change_intent {purpose:[record handles],obligations:[record handles],targets:[{file,entity,planned,records:[record handles]}],missing:null or explanation}. Roles are journal metadata. Every planned code file needs an affected entity; accompanying documentation inherits the change purpose without invented entities. Use supplied handles with planned=false for existing entities; use an intended short name and planned=true for a new helper. Related helpers may share the same purpose and obligations. If genuinely needed intent is absent, explain missing in the plan and its public-task rationale in summary; capture and human review precede a revised plan selecting accepted records. Empty purpose never authorizes edits. Selecting records does not exempt other constraints.\n");
        }
        // Choice labels are discovery data. Add only complete encoded entries
        // within the actual request budget; never clip a handle or proof.
        anyhow::ensure!(prompt.len() <= budget, "insufficient context space for change-plan instructions; use /plan to narrow the working set or increase the configured context window");
        let record_limit = prompt.len() + budget.saturating_sub(prompt.len()) / 2;
        let mut shown_records = 0;
        for (index, record) in choices.records.iter().enumerate() {
            let line = format!(
                "Record {}\n",
                serde_json::to_string(
                    &json!({"handle":format!("r{index}"),"kind":record.kind,"label":bounded(&record.label,300)})
                )?
            );
            if prompt.len() + line.len() > record_limit {
                break;
            }
            prompt.push_str(&line);
            shown_records += 1;
        }
        anyhow::ensure!(choices.records.is_empty() || shown_records > 0,
            "insufficient context space for any current record choice; use /plan to narrow the working set or increase the configured context window");
        for entity in &choices.entities {
            let line = format!(
                "Entity {}\n",
                serde_json::to_string(
                    &json!({"handle":entity.handle,"file":entity.file,"name":entity.name})
                )?
            );
            if prompt.len() + line.len() > budget {
                break;
            }
            prompt.push_str(&line);
        }
        Ok(prompt)
    }

    pub(super) fn action_schema(&self) -> Value {
        let mut schema = if self.task.batch_capture {
            conversational_schema(self.task.mode)
        } else {
            action_schema()
        };
        if self.task.postedit_association_contract == 1 {
            return schema;
        }
        if !self.task.entity_links && self.task.intent_policy == IntentPolicy::Current {
            return schema;
        }
        let actions = if self.task.batch_capture {
            &mut schema["properties"]["action"]
        } else {
            &mut schema
        };
        let refs = json!({"type":"array","maxItems":MAX_REFERENCES,"items":{"type":"string"}});
        let target_schema = json!({"type":"array","minItems":1,"maxItems":MAX_TARGETS,"items":{"type":"object","additionalProperties":false,"required":["file","entity","planned","records"],"properties":{"file":{"type":"string"},"entity":{"type":"string","maxLength":200},"planned":{"type":"boolean"},"records":refs}}});
        if self.task.mode == Mode::Auto {
            actions["oneOf"].as_array_mut().unwrap().push(json!({"type":"object","additionalProperties":false,"required":["action","targets"],"properties":{"action":{"const":"associate","type":"string"},"targets":target_schema}}));
        }
        if matches!(
            self.task.intent_policy,
            IntentPolicy::Current | IntentPolicy::ChangeLevelV2 | IntentPolicy::Symbolic
        ) {
            return schema;
        }
        if let Some(plan) = actions["oneOf"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|a| a["properties"]["action"]["const"] == "plan")
        {
            let refs = json!({"type":"array","maxItems":MAX_REFERENCES,"items":{"type":"string"}});
            plan["properties"]["change_intent"] = json!({"type":"object","additionalProperties":false,"required":["purpose","obligations","targets","missing"],"properties":{
                "purpose":refs,"obligations":refs,"missing":{"type":["string","null"],"maxLength":2000},
                "targets":{"type":"array","minItems":0,"maxItems":MAX_TARGETS,"items":{"type":"object","additionalProperties":false,"required":["file","entity","planned","records"],"properties":{"file":{"type":"string"},"entity":{"type":"string","maxLength":200},"planned":{"type":"boolean"},"records":refs}}}}});
            plan["required"]
                .as_array_mut()
                .unwrap()
                .push(json!("change_intent"));
        }
        schema
    }

    /// Returns false after a recoverable gate failure has moved back to planning.
    pub(super) async fn validate_intent_approval(
        &mut self,
        context: &ContextResponse,
    ) -> Result<bool> {
        if matches!(
            self.task.intent_policy,
            IntentPolicy::Current | IntentPolicy::ChangeLevelV2 | IntentPolicy::Symbolic
        ) {
            return Ok(true);
        }
        let plan = self.task.plan.as_ref().context("no plan")?.clone();
        let mapping = plan
            .change_intent
            .context("intent mapping missing from treatment plan")?;
        if mapping.missing.is_some() || mapping.purpose.is_empty() {
            self.replan_missing_intent("Capture did not yet supply approved purpose; select current records after review, or ask the human for missing intent.");
            self.persist()?;
            return Ok(false);
        }
        self.refresh_intent_choices(&plan.files, false).await?;
        let choices = self.intent_choices.as_ref().unwrap();
        let current: BTreeSet<_> = choices.records.iter().map(|r| &r.iri).collect();
        let roles: BTreeSet<_> = mapping.purpose.iter().chain(&mapping.obligations).collect();
        let mut valid = roles.iter().all(|r| current.contains(r))
            && mapping.targets.iter().all(|t| {
                plan.files.contains(&t.file)
                    && !t.records.is_empty()
                    && t.records.iter().all(|r| roles.contains(r))
            })
            && plan
                .files
                .iter()
                .filter(|f| requires_entity(f))
                .all(|f| mapping.targets.iter().any(|t| &t.file == f))
            && context.revision == choices.revision;
        for target in mapping.targets.iter().filter(|t| !t.planned) {
            let actual = fingerprint(&self.workspace.read(&target.file)?);
            valid &= choices.entities.iter().any(|e| {
                e.file == target.file
                    && e.symbol == target.entity
                    && actual.as_deref() == Some(e.source_digest.as_str())
            });
        }
        if !valid {
            self.replan_missing_intent("Intent record, indexed source proof, or file scope is no longer current; revise the plan using refreshed choices.");
            self.persist()?;
        }
        Ok(valid)
    }
    pub(super) fn plan_needs_intent(&self) -> bool {
        self.task.intent_policy == IntentPolicy::ChangeLevel
            && self
                .task
                .plan
                .as_ref()
                .and_then(|p| p.change_intent.as_ref())
                .is_some_and(|m| m.missing.is_some() || m.purpose.is_empty())
    }

    pub(super) fn replan_missing_intent(&mut self, reason: &str) {
        self.task.intent_missing_rounds = self.task.intent_missing_rounds.saturating_add(1);
        self.intent_event("intent_invalidated", reason);
        self.end_intent_cycle("replan missing or rejected intent");
        self.task.mode = Mode::Plan;
        self.task.approved_revision = None;
        self.task.phase = if self.task.intent_missing_rounds >= 3 {
            Phase::AwaitingInput
        } else {
            Phase::Planning
        };
        self.task.after_review = self.task.phase;
        self.task.review_continuation = None;
        self.task.last_response = if self.task.intent_missing_rounds >= 3 {
            format!("{reason} Three intent-resolution cycles could not produce an approvable plan. Human guidance is required.")
        } else {
            reason.to_owned()
        };
        self.event(self.task.last_response.clone());
    }

    fn review_before_associations(&mut self) -> Result<bool> {
        if self.task.reviews.is_empty() {
            return Ok(false);
        }
        // A preceding record ratification changes the revision. Resolve it
        // before freezing a link request, so review order cannot strand links.
        self.task.phase = Phase::AwaitingReview;
        self.task.review_continuation = Some(Phase::AwaitingPlan);
        self.task.after_review = Phase::AwaitingPlan;
        self.task.mode = Mode::Plan;
        self.task.approved_revision = None;
        self.start_intent_cycle();
        self.event("Review pending captured knowledge before proposing associations against its accepted revision.");
        self.persist()?;
        Ok(true)
    }

    /// Persist the proposal operation before contacting the daemon. Existing
    /// entities bind at approval; planned entities bind before verification.
    pub(super) async fn prepare_intent_links(&mut self, include_planned: bool) -> Result<bool> {
        if matches!(
            self.task.intent_policy,
            IntentPolicy::Current | IntentPolicy::ChangeLevelV2 | IntentPolicy::Symbolic
        ) && self.task.pending_intent_links.is_none()
        {
            return Ok(false);
        }
        if self.task.pending_intent_links.is_none() {
            if self.review_before_associations()? {
                return Ok(true);
            }
            let plan = self.task.plan.as_ref().context("no plan")?.clone();
            let mapping = plan.change_intent.context("missing intent mapping")?;
            self.refresh_intent_choices(&plan.files, include_planned)
                .await?;
            let choices = self
                .intent_choices
                .as_ref()
                .context("missing intent choices")?;
            let mut bindings = Vec::new();
            for target in &mapping.targets {
                if target.planned && !include_planned {
                    continue;
                }
                let entity = choices.entities.iter().find(|e| {
                    e.file == target.file
                        && if target.planned {
                            e.name == target.entity
                        } else {
                            e.symbol == target.entity
                        }
                });
                let Some(entity) = entity else {
                    self.intent_event(
                        "unresolved_binding",
                        &format!("{}:{}", target.file, target.entity),
                    );
                    self.task.last_response = format!("Intent target {} in {} does not resolve in current indexed source. Revise the plan or implement the planned entity before finishing.", target.entity, target.file);
                    let reason = self.task.last_response.clone();
                    self.replan_missing_intent(&reason);
                    self.persist()?;
                    return Ok(true);
                };
                for record in &target.records {
                    let binding = IntentBinding {
                        record_iri: record.clone(),
                        file: target.file.clone(),
                        symbol: Some(entity.symbol.clone()),
                        planned_name: None,
                        source_digest: Some(entity.source_digest.clone()),
                    };
                    let linked = entity.dossier_records.contains(record);
                    if !linked && !bindings.contains(&binding) {
                        bindings.push(binding);
                    }
                }
            }
            if bindings.is_empty() {
                return Ok(false);
            }
            self.task.pending_intent_links = Some(IntentLinkRequest {
                operation_id: uuid::Uuid::new_v4().to_string(),
                revision: self.task.knowledge_revision.clone(),
                bindings,
            });
            self.persist()?;
        }
        let request = self.task.pending_intent_links.clone().unwrap();
        let response: IntentLinkResponse = self.post("intent/link", &request).await?;
        if !response.unresolved.is_empty() {
            anyhow::ensure!(response.links.is_empty() && response.resolved.is_empty(),
                "daemon must reject unresolved intent batches atomically; use /plan to abandon this operation");
            self.task.pending_intent_links = None;
            self.intent_event("unresolved_binding", &response.unresolved.join("; "));
            self.replan_missing_intent(
                "Intent targets did not resolve; refresh source and revise the plan.",
            );
            self.persist()?;
            return Ok(true);
        }
        anyhow::ensure!(
            response.resolved == request.bindings,
            "daemon intent response did not preserve the exact requested binding order"
        );
        let unique_links: BTreeSet<_> = response.links.iter().collect();
        anyhow::ensure!(
            response.links.len() == request.bindings.len()
                && unique_links.len() == response.links.len(),
            "daemon intent response must identify one unique link per requested binding"
        );
        // Reuse the existing review UI for precise associations. Existing
        // dossier links were skipped; daemon retries deduplicate new proposals.
        self.task.reviews.push(ReviewItem {
            capture_resolution: None,
            request: CaptureRequest { operation_id:request.operation_id.clone(), proposals:vec![] },
            response: CaptureResponse { proposals:vec![CapturedProposal {
                iri:request.operation_id.clone(), title:"Proposed code-to-knowledge associations".into(),kind:"CodeLink".into(), links:response.links,unanchored:vec![],
            }] }, reason: "Review the relevance of each existing-record association; record selection is not proof of implementation.".into(), intent_links:Some(request),
        });
        self.task.pending_intent_links = None;
        self.task.phase = Phase::AwaitingReview;
        self.task.mode = Mode::Plan;
        self.task.approved_revision = None;
        self.task.review_continuation = Some(Phase::AwaitingPlan);
        self.task.after_review = Phase::AwaitingPlan;
        self.task.final_capture = false;
        self.start_intent_cycle();
        self.persist()?;
        Ok(true)
    }

    pub(super) async fn abandon_pending_intent(&mut self, reason: &str) -> Result<()> {
        let Some(request) = self.task.pending_intent_links.clone() else {
            return Ok(());
        };
        // Resolve uncertainty at the owner of the durable graph operation.
        // A rejection tombstone prevents an in-flight request resurfacing later.
        let response: CheckpointResponse = self
            .post(
                "intent/abandon",
                &ReviewRequest {
                    operation_id: request.operation_id.clone(),
                    accept: false,
                },
            )
            .await?;
        anyhow::ensure!(
            response.durable && response.conforms && response.pending.is_empty(),
            "intent operation could not be durably abandoned; retry after the daemon recovers"
        );
        self.task.pending_intent_links = None;
        self.intent_event(
            "intent_abandoned",
            &format!("{}: {}", request.operation_id, reason),
        );
        self.update_knowledge_revision(response.revision);
        self.persist()
    }

    pub(super) async fn review_intent_links(
        &mut self,
        position: usize,
        accept: bool,
        emit_interaction: bool,
    ) -> Result<()> {
        let request = self.task.reviews[position]
            .intent_links
            .clone()
            .context("no intent associations")?;
        let is_postedit = self
            .task
            .postedit_association
            .as_ref()
            .is_some_and(|state| {
                state.pending_link_operation_id.as_deref() == Some(&request.operation_id)
            })
            || self.symbolic_association_matches(&request.operation_id);
        let link_iris = self.task.reviews[position]
            .response
            .proposals
            .first()
            .context("intent review omitted its persisted response")?
            .links
            .clone();
        anyhow::ensure!(
            self.task.reviews[position].response.proposals.len() == 1
                && link_iris.len() == request.bindings.len(),
            "intent review response does not identify exactly one link per binding"
        );
        let response: CheckpointResponse = self
            .post(
                "intent/review",
                &ReviewRequest {
                    operation_id: request.operation_id.clone(),
                    accept,
                },
            )
            .await?;
        anyhow::ensure!(
            response.durable && response.conforms && response.pending.is_empty(),
            "intent review is not durably resolved"
        );
        self.task.reviews.remove(position);
        if emit_interaction {
            self.emit_review_interaction(accept, &request.operation_id);
        }
        for (binding, link_iri) in request.bindings.iter().zip(&link_iris) {
            self.intent_event(
                "link_review",
                &format!(
                    "{} {} for {} {} in {}",
                    if accept { "accepted" } else { "rejected" },
                    link_iri,
                    binding.record_iri,
                    binding.symbol.as_deref().unwrap_or_default(),
                    request.operation_id
                ),
            );
        }
        if accept {
            self.task.intent_bound.extend(request.bindings.clone());
        } else {
            self.intent_event("intent_rejected", "human rejected intent associations");
            if !is_postedit {
                self.replan_missing_intent("Intent associations were rejected. Revise the plan rather than recreating the rejected links.");
            }
        }
        if let Some(state) = self.task.postedit_association.as_mut().filter(|state| {
            state.pending_link_operation_id.as_deref() == Some(&request.operation_id)
        }) {
            if !accept {
                state.rejected_bindings.extend(request.bindings.clone());
                for binding in &request.bindings {
                    if !self.task.postedit_rejected_bindings.contains(binding) {
                        self.task.postedit_rejected_bindings.push(binding.clone());
                    }
                }
            }
            state.pending_link_operation_id = None;
            state.status = "resolved".into();
        }
        self.resolve_symbolic_association(&request.operation_id);
        self.update_knowledge_revision(response.revision);
        if is_postedit {
            // This task's association disposition changes graph revision but
            // does not invalidate its approved purpose or owned source chain.
            self.task.approved_revision = Some(self.task.knowledge_revision.clone());
            if let Some(scope) = self.task.approved_change_scope.as_mut() {
                scope.knowledge_revision = self.task.knowledge_revision.clone();
            }
            // The edit gate also compares the ready purpose selection's
            // revision; carry it across too or the next edit is refused.
            if let Some(selection) = self
                .task
                .purpose_selection
                .as_mut()
                .filter(|selection| selection.status == "ready")
            {
                selection.revision = self.task.knowledge_revision.clone();
            }
        }
        if self.task.reviews.is_empty() && (accept || is_postedit) {
            self.task.phase = if is_postedit {
                self.task.mode = Mode::Auto;
                Phase::Verifying
            } else {
                Phase::AwaitingPlan
            };
            self.task.review_continuation = None;
            let files = self.task.plan.as_ref().context("no plan")?.files.clone();
            self.task.intent_refresh_pending = files;
        }
        // The daemon receipt, dispositions, and continuation are one durable
        // local commit. Context refresh may fail and is retried independently.
        self.persist()?;
        self.resume_intent_refresh().await
    }

    pub(super) async fn resume_intent_refresh(&mut self) -> Result<()> {
        if self.task.intent_refresh_pending.is_empty() {
            return Ok(());
        }
        let files = self.task.intent_refresh_pending.clone();
        self.refresh(&files).await?;
        self.task.intent_refresh_pending.clear();
        self.persist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::daemon::intent::IntentEntity;

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Runner, Fixture) {
        let root =
            std::env::temp_dir().join(format!("moosedev-intent-unit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("code.py"), "def existing():\n    return 1\n").unwrap();
        let task: Task = serde_json::from_value(json!({
            "id":uuid::Uuid::new_v4().to_string(),"root":root,"objective":"Extract a shared helper","mode":"Plan","phase":"Planning",
            "events":[],"last_response":"","knowledge_revision":"v1","read_files":[],"check_results":[],"model_requests":[],
            "schema":1,"snapshots":{},"capture_due":false,"final_capture":false,"after_review":"Planning","resume_phase":"Planning","steps":0,"capture_operations":[],"capture_cursor":0,"source":{}
        })).unwrap();
        let (journal, lock) = Runner::storage(&root, &task.id).unwrap();
        let runner = Runner {
            task,
            workspace: Workspace::new(&root).unwrap(),
            daemon: "http://127.0.0.1:1".into(),
            http: reqwest::Client::new(),
            journal,
            _lock: lock,
            context: None,
            config: None,
            model_client: None,
            response_policy: None,
            progress: None,
            streaming: None,
            last_saved: Mutex::new(None),
            intent_choices: Some(IntentResolveResponse {
                revision: "v1".into(),
                records: vec![
                    CaptureTarget {
                        iri: "urn:purpose".into(),
                        label: "Existing behavior".into(),
                        kind: "Requirement".into(),
                    },
                    CaptureTarget {
                        iri: "urn:constraint".into(),
                        label: "Preserve order".into(),
                        kind: "Constraint".into(),
                    },
                ],
                entities: vec![IntentEntity {
                    handle: "entity_0".into(),
                    symbol: "python existing().".into(),
                    file: "code.py".into(),
                    name: "existing".into(),
                    source_digest: hash("def existing():\n    return 1\n"),
                    dossier_records: vec![],
                }],
                unresolved: vec![],
            }),
        };
        (runner, Fixture(root))
    }
    fn mapping() -> ChangeIntent {
        ChangeIntent {
            purpose: vec!["r0".into()],
            obligations: vec!["r1".into()],
            targets: vec![
                ChangeTarget {
                    file: "code.py".into(),
                    entity: "entity_0".into(),
                    planned: false,
                    records: vec!["r0".into(), "r1".into()],
                    source_digest: None,
                },
                ChangeTarget {
                    file: "code.py".into(),
                    entity: "_shared".into(),
                    planned: true,
                    records: vec!["r0".into(), "r1".into()],
                    source_digest: None,
                },
            ],
            missing: None,
        }
    }

    #[test]
    fn symbolic_policy_parses_and_mandates_the_postedit_contract() {
        assert_eq!(IntentPolicy::Symbolic.env_name(), "symbolic");
        assert_eq!(
            serde_json::to_value(IntentPolicy::Symbolic).unwrap(),
            serde_json::json!("symbolic")
        );
        assert!(IntentPolicy::Symbolic.mandates_postedit_associations());
        assert!(IntentPolicy::ChangeLevelV2.mandates_postedit_associations());
        assert!(!IntentPolicy::Current.mandates_postedit_associations());
        assert!(!IntentPolicy::ChangeLevel.mandates_postedit_associations());
        let (mut runner, _fixture) = fixture();
        runner.set_intent_policy(IntentPolicy::Symbolic).unwrap();
        assert_eq!(runner.task.postedit_association_contract, 1);
        // The symbolic policy never exposes the legacy mapping or the optional
        // associate action to the model.
        runner.task.mode = Mode::Auto;
        let schema = runner.action_schema().to_string();
        assert!(!schema.contains("change_intent"));
        assert!(!schema.contains("\"associate\""));
        assert!(runner
            .resolve_change_intent(Some(mapping()), &["code.py".into()])
            .is_err());
    }

    #[test]
    fn old_journals_default_off_and_both_arms_can_share_optional_link_schema() {
        let (mut runner, _fixture) = fixture();
        assert_eq!(runner.task.intent_policy, IntentPolicy::Current);
        assert!(!runner.task.entity_links);
        assert_eq!(runner.action_schema(), action_schema());
        assert!(runner
            .resolve_change_intent(Some(mapping()), &["code.py".into()])
            .is_err());
        runner.task.entity_links = true;
        runner.task.mode = Mode::Auto;
        let current = runner.action_schema();
        assert!(current["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["properties"]["action"]["const"] == "associate"));
        assert!(!current.to_string().contains("change_intent"));
        runner.task.intent_policy = IntentPolicy::ChangeLevel;
        runner.task.mode = Mode::Plan;
        assert!(runner.action_schema().to_string().contains("change_intent"));
    }

    #[test]
    fn shared_existing_and_planned_targets_resolve_without_minting_records() {
        let (mut runner, _fixture) = fixture();
        runner.task.intent_policy = IntentPolicy::ChangeLevel;
        let resolved = runner
            .resolve_change_intent(Some(mapping()), &["code.py".into()])
            .unwrap()
            .unwrap();
        assert_eq!(resolved.purpose, ["urn:purpose"]);
        assert_eq!(resolved.obligations, ["urn:constraint"]);
        assert_eq!(resolved.targets[0].entity, "python existing().");
        assert!(resolved.targets[0].source_digest.is_some());
        assert_eq!(resolved.targets[1].entity, "_shared");
        assert!(resolved.targets[1].source_digest.is_none());
        assert_eq!(resolved.targets[0].records, resolved.targets[1].records);
        assert!(runner.task.pending_intent_links.is_none());
        assert!(runner.task.reviews.is_empty());
    }

    #[test]
    fn references_scope_and_source_proofs_fail_before_proposal_side_effects() {
        let (mut runner, fixture) = fixture();
        runner.task.intent_policy = IntentPolicy::ChangeLevel;
        for corrupt in [0, 1, 2, 3] {
            let mut candidate = mapping();
            match corrupt {
                0 => candidate.purpose[0] = "urn:invented".into(),
                1 => candidate.targets[0].records = vec!["r99".into()],
                2 => candidate.targets[0].file = "elsewhere.py".into(),
                _ => candidate.obligations.clear(),
            }
            assert!(runner
                .resolve_change_intent(Some(candidate), &["code.py".into()])
                .is_err());
        }
        std::fs::write(fixture.0.join("code.py"), "changed externally").unwrap();
        assert!(runner
            .resolve_change_intent(Some(mapping()), &["code.py".into()])
            .unwrap_err()
            .to_string()
            .contains("source proof"));
        assert!(runner.task.pending_intent_links.is_none());
    }

    #[test]
    fn intent_discovery_choices_fit_encoded_byte_budget() {
        let (mut runner, _fixture) = fixture();
        runner.task.intent_policy = IntentPolicy::ChangeLevel;
        let choices = runner.intent_choices.as_mut().unwrap();
        for i in 0..500 {
            choices.records.push(CaptureTarget {
                iri: format!("urn:{i}"),
                label: "λ\"\n".repeat(400),
                kind: "Lesson".into(),
            });
        }
        for budget in [0, 500, 1400, 2000, 8000, 12000] {
            let prompt = match runner.intent_prompt(budget) {
                Ok(prompt) => prompt,
                Err(error) => {
                    assert!(error.to_string().contains("context space"));
                    continue;
                }
            };
            assert!(prompt.len() <= budget);
            for line in prompt
                .lines()
                .filter(|l| l.starts_with("Record ") || l.starts_with("Entity "))
            {
                serde_json::from_str::<Value>(line.split_once(' ').unwrap().1).unwrap();
            }
        }
    }

    #[test]
    fn missing_intent_recovery_and_cycle_survive_journal_reload() {
        let (mut runner, fixture) = fixture();
        runner.task.intent_policy = IntentPolicy::ChangeLevel;
        runner.start_intent_cycle();
        let cycle = runner.task.intent_cycle.clone();
        runner.start_intent_cycle();
        assert_eq!(runner.task.intent_cycle, cycle);
        runner.persist().unwrap();
        let id = runner.task.id.clone();
        drop(runner);
        let mut runner = Runner::load(fixture.0.clone(), "http://127.0.0.1:1".into(), &id).unwrap();
        assert_eq!(runner.task.intent_policy, IntentPolicy::ChangeLevel);
        assert_eq!(runner.task.intent_cycle, cycle);
        for _ in 0..3 {
            runner.replan_missing_intent("Rejected association");
        }
        assert_eq!(runner.task.phase, Phase::AwaitingInput);
        assert_eq!(runner.task.mode, Mode::Plan);
        assert!(runner.task.approved_revision.is_none());
        let ids: BTreeSet<_> = runner.task.intent_events.iter().map(|e| &e.id).collect();
        assert_eq!(ids.len(), runner.task.intent_events.len());
        assert_eq!(
            runner
                .task
                .intent_events
                .iter()
                .filter(|e| e.kind == "cycle_started")
                .count(),
            1
        );
    }
}
