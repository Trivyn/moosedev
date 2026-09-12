//! Deterministic post-edit associations. The runner
//! supplies the changed files and the governing records it derived at plan
//! approval; the daemon binds the kind-filtered changed definitions to those
//! records with the predicate the ontology catalogue allows. No model, no
//! writes: the result feeds the ordinary `intent/link` review path.
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use oxigraph::model::NamedNode;

use super::current_status;
use super::intent_candidates::{
    changed_scopes, entity_record_context, index_revision, index_status, intersects, maybe_refresh,
    producer_label, prove_changed_source, validate_files, validate_source_ranges,
};
use super::revision::ensure_unchanged;
use crate::api::error::ApiError;
use crate::code::substrate::{is_test_path, DefinitionEntry, DefinitionScope, SourceRange};
use crate::graph::{self, AppState};
use crate::harness::digest::sha256_json;
use crate::harness::protocol::*;

const MAX_RANGES: usize = 256;
const MAX_SCOPES: usize = 256;
const MAX_RECORDS_PER_FILE: usize = 16;

pub async fn associate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AssociateRequest>,
) -> Result<Json<AssociatePage>, ApiError> {
    Ok(Json(
        tokio::task::spawn_blocking(move || associate_page(&state, &request))
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

pub fn associate_page(
    state: &AppState,
    request: &AssociateRequest,
) -> anyhow::Result<AssociatePage> {
    anyhow::ensure!(
        !request.files.is_empty(),
        "association needs at least one changed file"
    );
    validate_files(
        &request
            .files
            .iter()
            .map(|f| f.file.clone())
            .collect::<Vec<_>>(),
    )?;
    anyhow::ensure!(
        request
            .files
            .iter()
            .map(|f| f.changed_ranges.len())
            .sum::<usize>()
            <= MAX_RANGES,
        "too many changed ranges"
    );
    for changed in &request.files {
        for range in &changed.changed_ranges {
            anyhow::ensure!(
                range.start <= range.end,
                "changed range ends before it starts"
            );
        }
    }
    ensure_unchanged(
        state,
        &request.knowledge_revision,
        "knowledge changed before association; refresh context and retry",
    )?;
    let knowledge_revision = request.knowledge_revision.clone();
    let refresh_action = maybe_refresh(state, &request.refresh_policy);
    let Some(substrate) = state.substrate() else {
        return Ok(AssociatePage {
            knowledge_revision,
            index: IntentIndexSnapshot {
                revision: None,
                producer: None,
                status: IntentIndexStatus::Unavailable,
                refresh_action,
            },
            scope_digest: sha256_json(&request.files)?,
            bindings: Vec::new(),
            skipped: Vec::new(),
            ungoverned: Vec::new(),
            unresolved: request
                .files
                .iter()
                .map(|f| IntentCandidateUnresolved {
                    file: f.file.clone(),
                    reason: "index unavailable".into(),
                })
                .collect(),
        });
    };
    let index_revision = index_revision(&substrate, &request.files)?;
    let status = index_status(&substrate, &request.files);
    let scope_digest = sha256_json(&(&request.files, &index_revision, &knowledge_revision))?;
    let code_class = state.resolve_code_class("CodeEntity")?;
    let mut bindings = Vec::new();
    let mut skipped = Vec::new();
    let mut ungoverned = Vec::new();
    let mut unresolved = Vec::new();
    let mut scope_count = 0usize;
    for changed in &request.files {
        if changed.after_digest.is_none() {
            unresolved.push(IntentCandidateUnresolved {
                file: changed.file.clone(),
                reason: "deleted source has no association target".into(),
            });
            continue;
        }
        let (source_digest, current_source) = match prove_changed_source(state, changed) {
            Ok(value) => value,
            Err(error) => {
                unresolved.push(IntentCandidateUnresolved {
                    file: changed.file.clone(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if let Some(source) = current_source.as_deref() {
            validate_source_ranges(source, &changed.changed_ranges)?;
        }
        let scopes = changed_scopes(&substrate, changed);
        scope_count = scope_count.saturating_add(scopes.len());
        anyhow::ensure!(
            scope_count <= MAX_SCOPES,
            "association scope exceeds 256 definitions; narrow changed files or ranges"
        );
        let mut kept = Vec::new();
        for (scope, basis) in scopes {
            match skip_reason(&scope.definition.entry) {
                Some(reason) => skipped.push(skip(&changed.file, &scope, reason, None)),
                None => kept.push((scope, basis)),
            }
        }
        // One innermost kept definition per changed range: a method wins over
        // the class whose body contains the same change.
        let mut chosen: Vec<usize> = Vec::new();
        for range in &changed.changed_ranges {
            let range: SourceRange = (*range).into();
            let best = kept
                .iter()
                .enumerate()
                .filter(|(_, (scope, _))| intersects(span(scope), range))
                .min_by_key(|(_, (scope, basis))| {
                    (
                        extent(span(scope)),
                        *basis != IntentScopeBasis::ChangedDefinition,
                    )
                })
                .map(|(index, _)| index);
            if let Some(index) = best {
                if !chosen.contains(&index) {
                    chosen.push(index);
                }
            }
        }
        chosen.sort_unstable();
        for (index, (scope, _)) in kept.iter().enumerate() {
            if !chosen.contains(&index) {
                skipped.push(skip(&changed.file, scope, SkipReason::Enclosing, None));
            }
        }
        let governing = request
            .governing
            .get(&changed.file)
            .cloned()
            .unwrap_or_default();
        let mut existing_by_symbol: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut file_records: Vec<String> = Vec::new();
        for index in &chosen {
            let symbol = kept[*index].0.definition.entry.normalized_symbol.clone();
            let (existing, _component) = entity_record_context(state, &symbol)?;
            for iri in &existing {
                if !file_records.contains(iri) {
                    file_records.push(iri.clone());
                }
            }
            existing_by_symbol.insert(symbol, existing);
        }
        let mut candidates: Vec<(String, DerivedBasis)> = Vec::new();
        for iri in &governing {
            if !candidates.iter().any(|(known, _)| known == iri) {
                candidates.push((iri.clone(), DerivedBasis::Obligation));
            }
        }
        for iri in &file_records {
            if !candidates.iter().any(|(known, _)| known == iri) {
                candidates.push((iri.clone(), DerivedBasis::FileDossier));
            }
        }
        candidates.truncate(MAX_RECORDS_PER_FILE);
        if candidates.is_empty() {
            ungoverned.push(changed.file.clone());
            continue;
        }
        for index in chosen {
            let (scope, scope_basis) = &kept[index];
            let entry = &scope.definition.entry;
            let symbol = entry.normalized_symbol.as_str();
            let existing = existing_by_symbol.get(symbol).cloned().unwrap_or_default();
            for (iri, basis) in &candidates {
                if existing.contains(iri) {
                    skipped.push(skip(
                        &changed.file,
                        scope,
                        SkipReason::AlreadyLinked,
                        Some(iri.clone()),
                    ));
                    continue;
                }
                if !current_status(state, iri)
                    .as_deref()
                    .is_none_or(graph::is_accepted)
                {
                    skipped.push(skip(
                        &changed.file,
                        scope,
                        SkipReason::NotAccepted,
                        Some(iri.clone()),
                    ));
                    continue;
                }
                let class = graph::require_information_record(state, &NamedNode::new(iri)?)?;
                let record_kind = graph::local_name(&class).to_string();
                let predicate = graph::link_predicate_for_kind(&record_kind);
                let legal = state
                    .catalogue
                    .legal_predicates(&state.store, &class, &code_class)
                    .iter()
                    .any(|edge| {
                        edge.predicate_local == predicate
                            && edge.direction == graph::EdgeDirection::Forward
                    });
                if !legal {
                    skipped.push(skip(
                        &changed.file,
                        scope,
                        SkipReason::NoLegalPredicate,
                        Some(iri.clone()),
                    ));
                    continue;
                }
                let (_, _, _, assertion_digest) =
                    super::reconciliation::candidate_assertions(state, iri)?;
                let definition_range: HarnessSourceRange = scope.definition.range.into();
                let candidate_digest = sha256_json(&(
                    &changed.file,
                    symbol,
                    &source_digest,
                    &definition_range,
                    basis,
                    iri,
                    &assertion_digest,
                    &index_revision,
                    &scope_digest,
                    &knowledge_revision,
                ))?;
                bindings.push(DerivedBinding {
                    file: changed.file.clone(),
                    symbol: symbol.to_string(),
                    name: entry.display_name.clone(),
                    kind: entry.kind.clone(),
                    definition_range,
                    scope_basis: scope_basis.clone(),
                    source_digest: source_digest.clone(),
                    record_iri: iri.clone(),
                    record_kind,
                    assertion_digest,
                    predicate: predicate.into(),
                    basis: *basis,
                    candidate_digest,
                });
            }
        }
    }
    bindings.sort_by(|a, b| {
        (&a.file, &a.definition_range, &a.symbol, &a.record_iri).cmp(&(
            &b.file,
            &b.definition_range,
            &b.symbol,
            &b.record_iri,
        ))
    });
    ensure_unchanged(
        state,
        &knowledge_revision,
        "knowledge changed during association; retry",
    )?;
    Ok(AssociatePage {
        knowledge_revision,
        index: IntentIndexSnapshot {
            revision: Some(index_revision),
            producer: producer_label(&substrate),
            status,
            refresh_action,
        },
        scope_digest,
        bindings,
        skipped,
        ungoverned,
        unresolved,
    })
}

/// Definitions that never carry a knowledge link of their own: test code,
/// parameters, type members and locals. Unknown kinds (syntactic anchors) are
/// kept; the binding still requires human review.
fn skip_reason(entry: &DefinitionEntry) -> Option<SkipReason> {
    if is_test_path(&entry.file) {
        return Some(SkipReason::TestPath);
    }
    if graph::is_type_member(entry) {
        return Some(SkipReason::TypeMember);
    }
    match entry.kind.as_deref() {
        Some("Parameter" | "TypeParameter") => Some(SkipReason::Parameter),
        Some("Variable" | "Local" | "Constant" | "Property") => Some(SkipReason::Local),
        _ => None,
    }
}

fn skip(
    file: &str,
    scope: &DefinitionScope,
    reason: SkipReason,
    record_iri: Option<String>,
) -> SkippedScope {
    SkippedScope {
        file: file.into(),
        symbol: scope.definition.entry.normalized_symbol.clone(),
        kind: scope.definition.entry.kind.clone(),
        reason,
        record_iri,
    }
}

fn span(scope: &DefinitionScope) -> SourceRange {
    scope.enclosing_range.unwrap_or(scope.definition.range)
}

fn extent(range: SourceRange) -> (u32, u32) {
    (
        range.end.line.saturating_sub(range.start.line),
        range.end.col.wrapping_sub(range.start.col),
    )
}
