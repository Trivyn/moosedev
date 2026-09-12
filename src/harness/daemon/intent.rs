//! Derived code associations use existing graph predicates and ratification.
//! A symbol is usable only while filesystem evidence proves its indexed source.
use super::journal::{journal_path, load, load_or_store, lock_operations, save_operation};
use super::revision::ensure_unchanged;
use super::*;
use crate::harness::digest::sha256_hex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentResolveRequest {
    pub files: Vec<String>,
    #[serde(default)]
    pub refresh_index: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentEntity {
    pub handle: String,
    pub symbol: String,
    pub file: String,
    pub name: String,
    pub source_digest: String,
    pub dossier_records: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentResolveResponse {
    pub revision: String,
    pub records: Vec<CaptureTarget>,
    pub entities: Vec<IntentEntity>,
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentBinding {
    pub record_iri: String,
    pub file: String,
    pub symbol: String,
    #[serde(default)]
    pub source_digest: Option<String>,
}

impl IntentBinding {
    /// The reviewable binding a derived association proposes: the resolved
    /// symbol with the source proof the daemon derived it from.
    pub fn from_derived(derived: &DerivedBinding) -> Self {
        Self {
            record_iri: derived.record_iri.clone(),
            file: derived.file.clone(),
            symbol: derived.symbol.clone(),
            source_digest: Some(derived.source_digest.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentLinkRequest {
    pub operation_id: String,
    pub revision: String,
    pub bindings: Vec<IntentBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentLinkResponse {
    pub links: Vec<String>,
    pub resolved: Vec<IntentBinding>,
    pub unresolved: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct LinkOperation {
    request: IntentLinkRequest,
    response: IntentLinkResponse,
    predicates: Vec<String>,
    prepared: bool,
    review: Option<bool>,
    reviewed: bool,
}

const LINK_EVIDENCE: &str = "derived post-edit association; relevance requires human review";

pub async fn resolve(
    State(state): State<Arc<AppState>>,
    Json(request): Json<IntentResolveRequest>,
) -> Result<Json<IntentResolveResponse>, ApiError> {
    // Indexing is blocking producer work; keep the HTTP executor responsive.
    Ok(Json(
        tokio::task::spawn_blocking(move || resolve_entities(&state, &request))
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

pub fn resolve_entities(
    state: &AppState,
    request: &IntentResolveRequest,
) -> anyhow::Result<IntentResolveResponse> {
    anyhow::ensure!(request.files.len() <= 32, "at most 32 intent files");
    for file in &request.files {
        validate_path(file)?;
    }
    if request.refresh_index {
        // This experimental refresher is deliberately restricted to the pilot's
        // frozen Python producer. Other projects may index through the CLI.
        let binary = std::env::var_os("MOOSEDEV_SCIP_PYTHON").ok_or_else(|| {
            anyhow::anyhow!(
                "intent refresh requires frozen MOOSEDEV_SCIP_PYTHON; no PATH or npx fallback"
            )
        })?;
        anyhow::ensure!(
            Path::new(&binary).is_absolute() && Path::new(&binary).is_file(),
            "frozen Python indexer must be an existing absolute path"
        );
        anyhow::ensure!(
            !crate::code::substrate::registry().iter().any(|producer|
                producer.name != "scip-python" && (producer.detect)(&state.project_root()).is_some()),
            "automatic intent indexing currently supports Python-only projects; index this project externally with frozen producers"
        );
        let producers: Vec<_> = crate::code::substrate::registry()
            .iter()
            .filter(|producer| producer.name == "scip-python")
            .copied()
            .collect();
        crate::code::substrate::producer::run_index_with(
            &producers,
            &state.project_root(),
            &state.data_dir,
        )?;
        state.load_substrate(&state.project_root());
    }
    let mut response = IntentResolveResponse {
        revision: accepted_revision(state)?,
        records: current_record_targets(state)?,
        entities: Vec::new(),
        unresolved: Vec::new(),
    };
    let Some(substrate) = state.substrate() else {
        response
            .unresolved
            .push("missing index: run the frozen indexer before resolving entities".into());
        return Ok(response);
    };
    for file in &request.files {
        let Some(source) = substrate.read_indexed_source(file) else {
            response.unresolved.push(format!(
                "{file}: missing index coverage or source changed since indexing"
            ));
            continue;
        };
        let digest = sha256_hex(&source);
        let definitions = substrate.definitions_in_file(file);
        if definitions.is_empty() {
            response
                .unresolved
                .push(format!("{file}: no resolvable indexed entities"));
        }
        for definition in definitions {
            if response.entities.len() == 256 {
                response.unresolved.push(format!(
                    "{file}: entity choice budget exceeded; narrow files"
                ));
                break;
            }
            let entry = definition.entry;
            let dossier = graph::get_entity_dossier(
                state,
                &graph::DossierTarget::Symbol(entry.normalized_symbol.clone()),
            )?;
            response.entities.push(IntentEntity {
                handle: format!("entity_{}", response.entities.len()),
                symbol: entry.normalized_symbol,
                file: file.clone(),
                name: entry.display_name.unwrap_or_default(),
                source_digest: digest.clone(),
                dossier_records: dossier
                    .into_iter()
                    .flat_map(|d| d.direct_records)
                    .filter(|record| graph::in_working_set(&record.status))
                    .map(|record| record.iri)
                    .collect(),
            });
        }
    }
    ensure_unchanged(
        state,
        &response.revision,
        "knowledge changed during intent resolution; retry",
    )?;
    Ok(response)
}

pub async fn link(
    State(state): State<Arc<AppState>>,
    Json(request): Json<IntentLinkRequest>,
) -> Result<Json<IntentLinkResponse>, ApiError> {
    Ok(Json(link_operation(&state, request)?))
}

fn link_path(state: &AppState, id: &str) -> anyhow::Result<PathBuf> {
    journal_path(state, id, "intent.json")
}

pub fn link_operation(
    state: &AppState,
    request: IntentLinkRequest,
) -> anyhow::Result<IntentLinkResponse> {
    let _guard = lock_operations()?;
    let path = link_path(state, &request.operation_id)?;
    let check = |operation: &LinkOperation| {
        anyhow::ensure!(
            operation.request == request,
            "intent operation ID reused with different request"
        );
        anyhow::ensure!(
            operation.review != Some(false),
            "intent operation was abandoned or rejected; use a new plan operation"
        );
        Ok(())
    };
    let (mut operation, _) = load_or_store(&path, check, || {
        anyhow::ensure!(
            !request.bindings.is_empty() && request.bindings.len() <= 128,
            "intent links require 1..128 bindings"
        );
        ensure_unchanged(
            state,
            &request.revision,
            "knowledge changed before intent binding",
        )?;
        let files: BTreeSet<_> = request
            .bindings
            .iter()
            .map(|binding| binding.file.clone())
            .collect();
        let choices = resolve_entities(
            state,
            &IntentResolveRequest {
                files: files.into_iter().collect(),
                refresh_index: false,
            },
        )?;
        let mut response = IntentLinkResponse {
            links: Vec::new(),
            resolved: Vec::new(),
            unresolved: Vec::new(),
        };
        let mut predicates = Vec::new();
        for binding in &request.bindings {
            // The record is verified directly against the graph; the bounded
            // inventory in `choices.records` is informational only.
            let record_class =
                graph::require_information_record(state, &NamedNode::new(&binding.record_iri)?)
                    .map_err(|error| {
                        anyhow::anyhow!("intent record is not a knowledge record: {error}")
                    })?;
            anyhow::ensure!(
                current_status(state, &binding.record_iri)
                    .as_deref()
                    .is_some_and(graph::in_working_set),
                "intent record is not current accepted knowledge"
            );
            let candidates: Vec<_> = choices
                .entities
                .iter()
                .filter(|entity| entity.file == binding.file && entity.symbol == binding.symbol)
                .collect();
            if candidates.len() != 1 {
                response.unresolved.push(format!(
                    "{}: target missing, ambiguous, or unindexed: {}",
                    binding.file, binding.symbol
                ));
                continue;
            }
            let entity = candidates[0];
            if let Some(expected) = &binding.source_digest {
                anyhow::ensure!(
                    expected == &entity.source_digest,
                    "intent source proof changed for {}",
                    binding.file
                );
            }
            let resolved = IntentBinding {
                record_iri: binding.record_iri.clone(),
                file: entity.file.clone(),
                symbol: entity.symbol.clone(),
                source_digest: Some(entity.source_digest.clone()),
            };
            if !response.resolved.contains(&resolved) {
                response.resolved.push(resolved);
                predicates
                    .push(graph::link_predicate_for_kind(graph::local_name(&record_class)).into());
            }
        }
        // An unresolved batch is an assessment with zero graph side effects.
        // The runner may replan under a new operation ID without stranding a
        // partially visible ratification queue.
        if !response.unresolved.is_empty() {
            response.resolved.clear();
            predicates.clear();
        }
        Ok(LinkOperation {
            request: request.clone(),
            response,
            predicates,
            prepared: false,
            review: None,
            reviewed: false,
        })
    })?;
    if !operation.prepared {
        let _proposal_guard = state.lock_proposal_writes()?;
        for (binding, predicate) in operation
            .response
            .resolved
            .iter()
            .zip(&operation.predicates)
        {
            verify_binding(state, binding)?;
            // The graph primitive deduplicates pending retries; reuse accepted
            // associations too, without multiplying the review queue.
            let normalized = &binding.symbol;
            let association_present = association_exists(state, binding, predicate)?;
            let prior = graph::list_proposals(state, None)?
                .into_iter()
                .find(|p| {
                    p.subject_iri == binding.record_iri
                        && p.predicate_local == *predicate
                        && p.target_symbol == *normalized
                        && p.status == "accepted"
                })
                .filter(|_| association_present);
            let iri = if let Some(prior) = prior {
                prior.iri
            } else {
                graph::propose_link_unlocked(
                    state,
                    &binding.record_iri,
                    predicate,
                    normalized,
                    &binding.file,
                    LINK_EVIDENCE,
                    AUTHOR,
                    Utc::now(),
                )?
            };
            if !operation.response.links.contains(&iri) {
                operation.response.links.push(iri);
            }
        }
        durable_flush(state)?;
        operation.prepared = true;
        save_operation(&path, &operation)?;
    }
    Ok(operation.response)
}

fn verify_binding(state: &AppState, binding: &IntentBinding) -> anyhow::Result<()> {
    anyhow::ensure!(
        current_status(state, &binding.record_iri)
            .as_deref()
            .is_some_and(graph::in_working_set),
        "intent record is no longer current"
    );
    let substrate = state
        .substrate()
        .ok_or_else(|| anyhow::anyhow!("intent index unavailable"))?;
    let source = substrate
        .read_indexed_source(&binding.file)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "intent source is no longer proven indexed: {}",
                binding.file
            )
        })?;
    anyhow::ensure!(
        binding.source_digest.as_deref() == Some(sha256_hex(&source).as_str()),
        "intent source changed after binding"
    );
    anyhow::ensure!(
        substrate
            .definitions_in_file(&binding.file)
            .iter()
            .any(|definition| definition.entry.normalized_symbol == binding.symbol),
        "intent symbol disappeared"
    );
    Ok(())
}

fn association_exists(
    state: &AppState,
    binding: &IntentBinding,
    predicate: &str,
) -> anyhow::Result<bool> {
    let Some(entity) =
        graph::entity_for_symbol(state, &graph::CodeTerms::resolve(state)?, &binding.symbol)?
    else {
        return Ok(false);
    };
    Ok(state.store.contains(
        Quad::new(
            NamedNode::new(&binding.record_iri)?,
            NamedNode::new(state.resolve_object_property(predicate)?)?,
            NamedNode::new(entity)?,
            NamedNode::new(PROJECT_KG_GRAPH_IRI)?,
        )
        .as_ref(),
    )?)
}

pub async fn review(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ReviewRequest>,
) -> Result<Json<CheckpointResponse>, ApiError> {
    let expected = headers
        .get("x-moosedev-expected-revision")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid expected revision"))?;
    Ok(Json(review_links_checked(&state, &request, expected)?))
}

pub fn review_links(
    state: &AppState,
    request: &ReviewRequest,
) -> anyhow::Result<CheckpointResponse> {
    review_links_checked(state, request, None)
}

pub async fn abandon(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ReviewRequest>,
) -> Result<Json<CheckpointResponse>, ApiError> {
    Ok(Json(abandon_links(&state, &request)?))
}

/// A durable cancellation tombstone closes uncertain requests before the runner
/// discards them. Delayed HTTP retries cannot resurrect a discarded operation.
pub fn abandon_links(
    state: &AppState,
    request: &ReviewRequest,
) -> anyhow::Result<CheckpointResponse> {
    anyhow::ensure!(
        !request.accept,
        "abandon only rejects unratified associations"
    );
    let _guard = lock_operations()?;
    let path = link_path(state, &request.operation_id)?;
    let mut operation: LinkOperation = match load(&path)? {
        Some(operation) => operation,
        None => LinkOperation {
            request: IntentLinkRequest {
                operation_id: request.operation_id.clone(),
                revision: "abandoned-before-preparation".into(),
                bindings: vec![],
            },
            response: IntentLinkResponse {
                links: vec![],
                resolved: vec![],
                unresolved: vec![],
            },
            predicates: vec![],
            prepared: false,
            review: None,
            reviewed: false,
        },
    };
    if operation.review == Some(true) {
        // A lost acknowledgment of a human acceptance must never retract the
        // resulting knowledge when subsequent guidance abandons its plan.
        return checkpoint_snapshot(state, None);
    }
    let proposal_guard = state.lock_proposal_writes()?;
    operation.review = Some(false);
    save_operation(&path, &operation)?;
    let proposals = graph::list_proposals(state, None)?;
    for link in proposals {
        let frozen = operation
            .response
            .resolved
            .iter()
            .zip(&operation.predicates)
            .any(|(binding, predicate)| {
                binding.record_iri == link.subject_iri
                    && predicate == &link.predicate_local
                    && binding.symbol == link.target_symbol
                    && binding.file == link.target_path
            });
        if frozen && link.status == "proposed" {
            graph::reject_proposal_unlocked(state, &link.iri, REVIEWER)?;
        }
    }
    durable_flush(state)?;
    operation.prepared = true;
    operation.reviewed = true;
    save_operation(&path, &operation)?;
    drop(proposal_guard);
    checkpoint_snapshot(state, None)
}

fn review_links_checked(
    state: &AppState,
    request: &ReviewRequest,
    expected: Option<&str>,
) -> anyhow::Result<CheckpointResponse> {
    let _guard = lock_operations()?;
    let path = link_path(state, &request.operation_id)?;
    let mut operation: LinkOperation = serde_json::from_slice(&std::fs::read(&path)?)?;
    anyhow::ensure!(
        operation.prepared,
        "intent proposal preparation is incomplete; retry link request"
    );
    anyhow::ensure!(
        operation.review.is_none_or(|prior| prior == request.accept),
        "intent operation already reviewed differently"
    );
    let proposal_guard = state.lock_proposal_writes()?;
    if !operation.reviewed {
        anyhow::ensure!(
            operation.response.links.len() == operation.response.resolved.len()
                && operation.predicates.len() == operation.response.resolved.len(),
            "intent journal association lengths differ"
        );
        if request.accept && operation.review.is_none() {
            ensure_unchanged(
                state,
                expected.unwrap_or(&operation.request.revision),
                "knowledge changed before intent review; refresh approval",
            )?;
        }
        let links = graph::list_proposals(state, None)?;
        for ((iri, binding), predicate) in operation
            .response
            .links
            .iter()
            .zip(&operation.response.resolved)
            .zip(&operation.predicates)
        {
            let link = links
                .iter()
                .find(|link| link.iri == *iri)
                .ok_or_else(|| anyhow::anyhow!("intent proposal disappeared"))?;
            anyhow::ensure!(
                link.subject_iri == binding.record_iri
                    && link.predicate_local == *predicate
                    && link.target_symbol == binding.symbol
                    && link.target_path == binding.file,
                "intent proposal changed before review"
            );
            if request.accept {
                verify_binding(state, binding)?;
            }
            // Already accepted shared associations are not rejected with a
            // later plan; they predate this operation and remain knowledge.
            if link.status != "accepted" {
                preflight_resolution(state, iri, request.accept)?;
            }
        }
        operation.review = Some(request.accept);
        save_operation(&path, &operation)?;
        for iri in &operation.response.links {
            if current_status(state, iri).as_deref() != Some("accepted") {
                resolve_proposal(state, iri, request.accept)?;
            }
        }
        durable_flush(state)?;
        operation.reviewed = true;
        save_operation(&path, &operation)?;
    }
    drop(proposal_guard);
    let mut checkpoint = checkpoint_snapshot(state, None)?;
    if request.accept {
        for ((iri, binding), predicate) in operation
            .response
            .links
            .iter()
            .zip(&operation.response.resolved)
            .zip(&operation.predicates)
        {
            if !association_exists(state, binding, predicate)? {
                checkpoint.pending.push(iri.clone());
            }
        }
    }
    for iri in &operation.response.links {
        if !matches!(
            current_status(state, iri).as_deref(),
            Some("accepted" | "rejected")
        ) {
            checkpoint.pending.push(iri.clone());
        }
    }
    Ok(checkpoint)
}
