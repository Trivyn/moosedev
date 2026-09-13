//! Human review of a captured operation: ratify or reject its proposals
//! atomically, then attest the resulting knowledge revision.
use super::capture::{finish_capture, record_input};
use super::context::accepted_revision_masked;
use super::*;

pub async fn review(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ReviewRequest>,
) -> Result<(HeaderMap, Json<CheckpointResponse>), ApiError> {
    let expected = headers
        .get("x-moosedev-expected-revision")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid expected revision"))?;
    let (checkpoint, attestation) = review_operation_checked(&state, &request, expected)?;
    let mut response_headers = HeaderMap::new();
    if let Some((base, result)) = attestation {
        response_headers.insert(
            "x-moosedev-review-base-revision",
            base.parse().map_err(anyhow::Error::from)?,
        );
        response_headers.insert(
            "x-moosedev-review-result-revision",
            result.parse().map_err(anyhow::Error::from)?,
        );
    }
    Ok((response_headers, Json(checkpoint)))
}

pub fn review_operation(
    state: &AppState,
    request: &ReviewRequest,
) -> anyhow::Result<CheckpointResponse> {
    Ok(review_operation_checked(state, request, None)?.0)
}

type ReviewResult = (CheckpointResponse, Option<(String, String)>);

fn review_operation_checked(
    state: &AppState,
    request: &ReviewRequest,
    expected_revision: Option<&str>,
) -> anyhow::Result<ReviewResult> {
    let _guard = lock_operations()?;
    let path = journal_path(state, &request.operation_id, "json")?;
    let mut operation: Operation = serde_json::from_slice(&std::fs::read(&path)?)?;
    finish_capture(state, &path, &mut operation)?;
    let proposal_guard = state.lock_proposal_writes()?;
    state.try_ensure_enriched()?;
    anyhow::ensure!(
        operation
            .review
            .is_none_or(|accepted| accepted == request.accept),
        "this capture operation already has a different review decision"
    );
    // Validate the entire review before advancing any member. The human saw
    // the journaled claims; an externally edited pending record is a different
    // proposal and needs a fresh review rather than accepting unseen content.
    if request.accept && !operation.reviewed {
        for (entry, proposal) in operation.entries.iter().zip(&operation.request.proposals) {
            let input = record_input(state, proposal)?;
            anyhow::ensure!(
                graph::require_information_record(state, &NamedNode::new(&entry.response.iri)?)?
                    == entry.class,
                "proposal kind changed after capture"
            );
            for (predicate, expected) in &input.properties {
                // A previous attempt may already have ratified this member.
                // Lifecycle consistency is checked by resolve_proposal below.
                if predicate == &state.capture.status {
                    continue;
                }
                let actual: Vec<_> = state
                    .store
                    .quads_for_pattern(
                        Some(NamedNodeRef::new(&entry.response.iri)?.into()),
                        Some(NamedNodeRef::new(predicate)?),
                        None,
                        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                            PROJECT_KG_GRAPH_IRI,
                        )?)),
                    )
                    .collect::<Result<Vec<_>, _>>()?;
                anyhow::ensure!(
                    actual.len() == 1
                        && matches!(&actual[0].object, Term::Literal(value) if value.value() == expected),
                    "proposal content changed after capture; a fresh review is required"
                );
            }
            let mut expected_relations = entry.relations.clone();
            if let (Some(target), Some(rationale)) = (&proposal.supersedes, &entry.rationale) {
                expected_relations
                    .push((state.resolve_object_property("supersedes")?, target.clone()));
                expected_relations.push((
                    state.resolve_object_property("hasRationale")?,
                    rationale.clone(),
                ));
            }
            let links = graph::list_proposals(state, None)?;
            // An interrupted review may already have materialized a journaled
            // link. Permit only that exact accepted link and its frozen symbol.
            for iri in &entry.response.links {
                if let Some(link) = links.iter().find(|link| &link.iri == iri) {
                    if operation.review == Some(true) && link.status == "accepted" {
                        let target = graph::entity_for_symbol(
                            state,
                            &graph::CodeTerms::resolve(state)?,
                            &link.target_symbol,
                        )?
                        .ok_or_else(|| anyhow::anyhow!("accepted code link target disappeared"))?;
                        expected_relations.push((
                            state.resolve_object_property(&link.predicate_local)?,
                            target,
                        ));
                    }
                }
            }
            // Check every domain relation, including fields absent from the
            // claim. Added constrains/violates/etc. are as unseen as supersedes.
            let mut predicates: BTreeSet<String> = state
                .arch_vocab
                .object_properties
                .iter()
                .chain(state.code_vocab.object_properties.iter())
                .map(|property| property.iri.clone())
                .collect();
            predicates.extend(
                expected_relations
                    .iter()
                    .map(|(predicate, _)| predicate.clone()),
            );
            for predicate in predicates {
                let expected: HashSet<Term> = expected_relations
                    .iter()
                    .filter(|(p, _)| p == &predicate)
                    .map(|(_, target)| NamedNode::new(target).map(Term::NamedNode))
                    .collect::<Result<_, _>>()?;
                let actual: HashSet<Term> = state
                    .store
                    .quads_for_pattern(
                        Some(NamedNodeRef::new(&entry.response.iri)?.into()),
                        Some(NamedNodeRef::new(&predicate)?),
                        None,
                        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                            PROJECT_KG_GRAPH_IRI,
                        )?)),
                    )
                    .map(|quad| quad.map(|q| q.object))
                    .collect::<Result<_, _>>()?;
                anyhow::ensure!(
                    actual == expected,
                    "proposal relation changed after capture"
                );
            }
            preflight_resolution(state, &entry.response.iri, true)?;
            if let Some(target) = proposal.retracts.as_ref().or(proposal.supersedes.as_ref()) {
                graph::require_information_record(state, &NamedNode::new(target)?)
                    .map_err(|error| anyhow::anyhow!("lifecycle target disappeared after capture: {error}; reject this operation or restore the target"))?;
                let status = current_status(state, target).unwrap_or_default();
                let already_resolved = current_status(state, &entry.response.iri).as_deref()
                    == Some("accepted")
                    && status
                        == if proposal.retracts.is_some() {
                            "deprecated"
                        } else {
                            "superseded"
                        };
                anyhow::ensure!(graph::in_working_set(&status) || already_resolved,
                    "lifecycle target changed after capture; reject this operation or restore the target");
            }
            for iri in &entry.response.links {
                preflight_resolution(state, iri, true)?;
                let link = links
                    .iter()
                    .find(|p| &p.iri == iri)
                    .ok_or_else(|| anyhow::anyhow!("queued code link disappeared after capture"))?;
                let expected_predicate = graph::link_predicate_for_kind(&proposal.kind);
                anyhow::ensure!(
                    link.subject_iri == entry.response.iri
                        && link.predicate_local == expected_predicate
                        && entry.anchors.iter().any(|(symbol, file)| {
                            crate::code::substrate::symbols::normalize_symbol(symbol).as_deref()
                                == Some(link.target_symbol.as_str())
                                && file == &link.target_path
                        }),
                    "queued code link changed after capture"
                );
            }
        }
    }
    if !operation.reviewed && !request.accept {
        for entry in &operation.entries {
            for iri in std::iter::once(&entry.response.iri).chain(&entry.response.links) {
                preflight_resolution(state, iri, false)?;
            }
        }
    }
    let subjects: HashSet<String> = operation
        .entries
        .iter()
        .flat_map(|entry| std::iter::once(&entry.response.iri).chain(&entry.response.links))
        .map(|iri| NamedNode::new(iri).map(|node| node.to_string()))
        .collect::<Result<_, _>>()?;
    if let Some(expected) = expected_revision.filter(|_| operation.review.is_none()) {
        let actual = accepted_revision(state)?;
        anyhow::ensure!(
            actual == expected,
            "knowledge changed before review; refresh approval before accepting"
        );
        if request.accept && !operation.request.has_governing() {
            operation.review_base_revision = Some(actual);
            operation.review_claims = Some(review_claims(state, &subjects)?);
        }
    }
    operation.review = Some(request.accept);
    save_operation(&path, &operation)?;
    if !operation.reviewed {
        if !request.accept {
            let mut members: Vec<String> = operation
                .entries
                .iter()
                .flat_map(|entry| std::iter::once(&entry.response.iri).chain(&entry.response.links))
                .cloned()
                .collect();
            members.extend(
                operation
                    .entries
                    .iter()
                    .filter_map(|entry| entry.rationale.as_ref())
                    .filter(|iri| current_status(state, iri).as_deref() == Some("proposed"))
                    .cloned(),
            );
            graph::reject_frozen_proposals_unlocked(state, &members, REVIEWER)?;
        }
        for (entry, proposal) in operation.entries.iter().zip(&operation.request.proposals) {
            resolve_proposal(state, &entry.response.iri, request.accept)?;
            for iri in &entry.response.links {
                resolve_proposal(state, iri, request.accept)?;
            }
            if request.accept {
                if let Some(target) = &proposal.retracts {
                    match current_status(state, target).as_deref() {
                        Some("deprecated") => {} // The prior attempt committed its lifecycle transaction.
                        status if status.is_none_or(graph::in_working_set) => {
                            graph::retract_decision_unlocked(
                                state,
                                target,
                                &proposal.description,
                                REVIEWER,
                                Utc::now(),
                            )?;
                            state.note_project_write();
                        }
                        _ => anyhow::bail!("retraction target changed during review"),
                    }
                }
            }
        }
        durable_flush(state)?;
        operation.reviewed = true;
        save_operation(&path, &operation)?;
    }
    drop(proposal_guard);
    let generation = state.project_write_generation();
    let checkpoint = checkpoint_snapshot(state, Some(&request.operation_id))?;
    // Do not infer freshness from two separate revision reads. Mask only the
    // previously unratified subjects, and prove the remaining graph stayed
    // identical AND the owned claims changed only in lifecycle status. New
    // code nodes or any other unproven mutation conservatively fail this proof.
    if operation.review_result_revision.is_none() {
        if let (Some(base), Some(claims)) =
            (&operation.review_base_revision, &operation.review_claims)
        {
            if accepted_revision_masked(state, &subjects)? == *base
                && review_claims(state, &subjects)? == *claims
                && generation == state.project_write_generation()
            {
                operation.review_result_revision = Some(checkpoint.revision.clone());
                save_operation(&path, &operation)?;
            }
        }
    }
    let attestation = operation
        .review_base_revision
        .zip(operation.review_result_revision);
    Ok((checkpoint, attestation))
}

fn review_claims(state: &AppState, subjects: &HashSet<String>) -> anyhow::Result<Vec<String>> {
    let mut claims = state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                PROJECT_KG_GRAPH_IRI,
            )?)),
        )
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|quad| {
            (subjects.contains(&quad.subject.to_string())
                || subjects.contains(&quad.object.to_string()))
                && quad.predicate.as_str() != state.capture.status
        })
        .map(|quad| quad.to_string())
        .collect::<Vec<_>>();
    claims.sort();
    Ok(claims)
}

pub(super) fn preflight_resolution(
    state: &AppState,
    iri: &str,
    accept: bool,
) -> anyhow::Result<()> {
    let expected = if accept { "accepted" } else { "rejected" };
    anyhow::ensure!(
        current_status(state, iri)
            .as_deref()
            .is_some_and(|status| status == "proposed" || status == expected),
        "proposal {iri} was resolved differently outside this operation"
    );
    Ok(())
}

pub(super) fn resolve_proposal(state: &AppState, iri: &str, accept: bool) -> anyhow::Result<()> {
    let expected = if accept { "accepted" } else { "rejected" };
    match current_status(state, iri).as_deref() {
        Some("proposed") => {
            if accept {
                graph::accept_proposal_unlocked(state, iri, REVIEWER)?;
            } else {
                graph::reject_proposal_unlocked(state, iri, REVIEWER)?;
            }
        }
        Some(status) if status == expected => {}
        _ => anyhow::bail!("proposal {iri} was resolved differently outside this operation"),
    }
    Ok(())
}
