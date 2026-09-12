//! Read-only, snapshot-bound intent candidate projections.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use sha2::{Digest, Sha256};

use super::{accepted_revision, current_status, validate_path};
use crate::api::error::ApiError;
use crate::code::substrate::{DefinitionScope, SourceRange};
use crate::graph::{self, AppState};
use crate::harness::protocol::*;

const MAX_FILES: usize = 32;
const MAX_RANGES: usize = 256;
const MAX_PAGE: usize = 16;
const MAX_PAGE_BYTES: usize = 96 * 1024;
const MAX_SCOPES: usize = 256;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

struct CandidateSnapshot<'a> {
    index_revision: &'a str,
    scope_digest: &'a str,
    index_status: &'a IntentIndexStatus,
}

pub async fn purpose_candidates(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PurposeCandidateRequest>,
) -> Result<Json<PurposeCandidatePage>, ApiError> {
    Ok(Json(purpose_page(&state, &request)?))
}

pub async fn intent_candidates(
    State(state): State<Arc<AppState>>,
    Json(request): Json<IntentCandidateRequest>,
) -> Result<Json<IntentCandidatePage>, ApiError> {
    Ok(Json(
        tokio::task::spawn_blocking(move || intent_page(&state, &request))
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

pub fn purpose_page(
    state: &AppState,
    request: &PurposeCandidateRequest,
) -> anyhow::Result<PurposeCandidatePage> {
    anyhow::ensure!(
        !request.objective.trim().is_empty(),
        "purpose objective is empty"
    );
    validate_files(&request.files)?;
    let revision = accepted_revision(state)?;
    let request_digest = digest(&(&request.objective, &request.files, request.limit))?;
    let limit = request.limit.unwrap_or(8).clamp(1, MAX_PAGE);
    let mut iris = graph::relevant_context_snapshot(state, Some(&request.objective), 100, false)?
        .into_iter()
        .filter(|item| is_record_kind(&item.kind))
        .map(|item| item.iri)
        .collect::<Vec<_>>();
    if let Some(substrate) = state.substrate() {
        for file in &request.files {
            let _ = substrate.indexed_source_digest(file);
            for scope in substrate.definition_scopes_in_file(file) {
                if let Some(dossier) = graph::get_entity_dossier(
                    state,
                    &graph::DossierTarget::Symbol(scope.definition.entry.normalized_symbol),
                )? {
                    for record in dossier
                        .direct_records
                        .into_iter()
                        .chain(dossier.component_records)
                    {
                        if accepted_status(&record.status) && !iris.contains(&record.iri) {
                            iris.push(record.iri);
                        }
                        if iris.len() == 100 {
                            break;
                        }
                    }
                }
            }
        }
    }
    iris.retain(|iri| {
        current_status(state, iri)
            .as_deref()
            .is_none_or(accepted_status)
    });
    let retrieval_digest = digest(&iris)?;
    let bound_revision = format!("{revision}.{retrieval_digest}");
    let offset = decode_cursor(
        request.cursor.as_deref(),
        &request_digest,
        &bound_revision,
        "purpose",
    )?;
    anyhow::ensure!(offset <= iris.len(), "cursor offset is out of range");
    let mut candidates = Vec::new();
    let mut bytes = 0usize;
    let mut end = offset;
    while end < iris.len() && candidates.len() < limit {
        let candidate = project_record(state, &iris[end], end)?;
        let size = serde_json::to_vec(&candidate)?.len();
        anyhow::ensure!(
            !candidates.is_empty() || size <= MAX_PAGE_BYTES,
            "one purpose candidate exceeds the response budget"
        );
        if !candidates.is_empty() && bytes.saturating_add(size) > MAX_PAGE_BYTES {
            break;
        }
        bytes = bytes.saturating_add(size);
        candidates.push(candidate);
        end += 1;
    }
    anyhow::ensure!(
        accepted_revision(state)? == revision,
        "knowledge changed during purpose lookup; retry"
    );
    Ok(PurposeCandidatePage {
        revision: revision.clone(),
        retrieval: if candidates.is_empty() && end >= iris.len() {
            PurposeRetrieval::ExhaustedEmpty
        } else {
            PurposeRetrieval::Page
        },
        candidates,
        next_cursor: (end < iris.len())
            .then(|| encode_cursor(end, &request_digest, &bound_revision, "purpose")),
    })
}

pub fn intent_page(
    state: &AppState,
    request: &IntentCandidateRequest,
) -> anyhow::Result<IntentCandidatePage> {
    anyhow::ensure!(
        !request.files.is_empty(),
        "intent candidates need at least one changed file"
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
        anyhow::ensure!(
            changed.before_digest.is_some() || changed.after_digest.is_some(),
            "changed file needs a before or after digest"
        );
        for range in &changed.changed_ranges {
            anyhow::ensure!(
                range.start <= range.end,
                "changed range ends before it starts"
            );
        }
    }
    // Preserve deleted definitions before an explicitly requested refresh removes
    // them from the current index. They remain audit evidence, never link targets.
    let historical_scopes = state
        .substrate()
        .map(|substrate| {
            request
                .files
                .iter()
                .filter(|file| file.after_digest.is_none())
                .map(|file| {
                    if let Some(history) = substrate.meta().historical_files.get(&file.file) {
                        let scopes = history
                            .definitions
                            .iter()
                            .map(|scope| {
                                deleted_from_parts(
                                    &file.file,
                                    &scope.symbol,
                                    scope.name.clone(),
                                    scope.definition_range,
                                    scope.enclosing_range,
                                    &history.source_digest,
                                )
                            })
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        return Ok((
                            file.file.clone(),
                            (scopes, Some(history.source_digest.clone())),
                        ));
                    }
                    let proven = substrate.indexed_source_digest(&file.file);
                    let scopes = substrate
                        .definition_scopes_in_file(&file.file)
                        .into_iter()
                        .map(|scope| {
                            deleted_from_parts(
                                &file.file,
                                &scope.definition.entry.normalized_symbol,
                                scope.definition.entry.display_name,
                                range_array(scope.definition.range),
                                scope.enclosing_range.map(range_array),
                                proven.as_deref().unwrap_or(""),
                            )
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    Ok((file.file.clone(), (scopes, proven)))
                })
                .collect::<anyhow::Result<std::collections::HashMap<_, _>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let refresh_action = if request.cursor.is_none() {
        maybe_refresh(state, &request.refresh_policy)
    } else {
        IntentRefreshAction::NotRequested
    };
    let knowledge_revision = accepted_revision(state)?;
    let request_digest = digest(&(&request.files, &request.refresh_policy, request.limit))?;
    let Some(substrate) = state.substrate() else {
        return Ok(IntentCandidatePage {
            knowledge_revision,
            index: IntentIndexSnapshot {
                revision: None,
                producer: None,
                status: IntentIndexStatus::Unavailable,
                refresh_action,
            },
            scope_digest: digest(&request.files)?,
            candidates: Vec::new(),
            unresolved: request
                .files
                .iter()
                .map(|f| IntentCandidateUnresolved {
                    file: f.file.clone(),
                    reason: "index unavailable".into(),
                })
                .collect(),
            deleted: Vec::new(),
            next_cursor: None,
        });
    };
    let indexed_source_proofs = request
        .files
        .iter()
        .map(|file| {
            (
                file.file.as_str(),
                substrate
                    .read_indexed_source(&file.file)
                    .map(|source| format!("{:x}", Sha256::digest(source.as_bytes()))),
            )
        })
        .collect::<Vec<_>>();
    let meta = substrate.meta();
    let index_revision = digest(&(
        meta.schema_version,
        &meta.indexed_commit,
        meta.indexed_at,
        &meta.generation,
        &meta.producers,
        indexed_source_proofs,
    ))?;
    let producer = Some(
        substrate
            .meta()
            .producers
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );
    let mut status = if substrate.is_stale() {
        IntentIndexStatus::Stale
    } else {
        IntentIndexStatus::Current
    };
    if request
        .files
        .iter()
        .any(|changed| match &changed.after_digest {
            Some(expected) => {
                substrate
                    .read_indexed_source(&changed.file)
                    .map(|source| format!("{:x}", Sha256::digest(source.as_bytes())))
                    .as_ref()
                    != Some(expected)
            }
            None => substrate.covers_file(&changed.file),
        })
    {
        status = IntentIndexStatus::Stale;
    }
    if request
        .files
        .iter()
        .filter(|file| file.after_digest.is_none())
        .any(|file| {
            historical_scopes
                .get(&file.file)
                .and_then(|(_, digest)| digest.as_ref())
                != file.before_digest.as_ref()
        })
    {
        status = IntentIndexStatus::Stale;
    }
    let scope_digest = digest(&(&request.files, &index_revision, &knowledge_revision))?;
    let cursor_revision = format!("{knowledge_revision}.{index_revision}.{scope_digest}");
    let offset = decode_cursor(
        request.cursor.as_deref(),
        &request_digest,
        &cursor_revision,
        "intent",
    )?;
    let mut unresolved = Vec::new();
    let mut all = Vec::new();
    let mut deleted = Vec::new();
    let candidate_snapshot = CandidateSnapshot {
        index_revision: &index_revision,
        scope_digest: &scope_digest,
        index_status: &status,
    };
    for changed in &request.files {
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
        if changed.after_digest.is_none() {
            let Some((scopes, proven_digest)) = historical_scopes.get(&changed.file) else {
                unresolved.push(IntentCandidateUnresolved {
                    file: changed.file.clone(),
                    reason: "deleted source has no retained indexed scope proof".into(),
                });
                continue;
            };
            if proven_digest.as_ref() != changed.before_digest.as_ref() {
                unresolved.push(IntentCandidateUnresolved {
                    file: changed.file.clone(),
                    reason: "deleted source digest was not proven by the indexed snapshot".into(),
                });
                continue;
            }
            for scope in scopes {
                deleted.push(scope.clone());
                anyhow::ensure!(
                    all.len().saturating_add(deleted.len()) <= MAX_SCOPES,
                    "intent scope exceeds 256 definitions; narrow changed files or ranges"
                );
            }
            continue;
        }
        let scopes = substrate.definition_scopes_in_file(&changed.file);
        let mut matched = false;
        for scope in scopes {
            let basis = if changed
                .changed_ranges
                .iter()
                .any(|r| intersects(scope.definition.range, (*r).into()))
            {
                Some(IntentScopeBasis::ChangedDefinition)
            } else if scope.enclosing_range.is_some_and(|enclosing| {
                changed
                    .changed_ranges
                    .iter()
                    .any(|r| intersects(enclosing, (*r).into()))
            }) {
                Some(IntentScopeBasis::EnclosingDefinition)
            } else {
                None
            };
            let Some(basis) = basis else { continue };
            matched = true;
            let candidate = post_candidate(
                state,
                &changed.file,
                &source_digest,
                scope,
                basis,
                &candidate_snapshot,
            )?;
            anyhow::ensure!(
                serde_json::to_vec(&candidate)?.len() <= MAX_PAGE_BYTES,
                "one intent candidate exceeds the response budget"
            );
            all.push(candidate);
            anyhow::ensure!(
                all.len().saturating_add(deleted.len()) <= MAX_SCOPES,
                "intent scope exceeds 256 definitions; narrow changed files or ranges"
            );
        }
        if !matched {
            let candidate =
                conservative_candidate(state, &changed.file, &source_digest, &candidate_snapshot)?;
            anyhow::ensure!(
                serde_json::to_vec(&candidate)?.len() <= MAX_PAGE_BYTES,
                "one intent candidate exceeds the response budget"
            );
            all.push(candidate);
            anyhow::ensure!(
                all.len().saturating_add(deleted.len()) <= MAX_SCOPES,
                "intent scope exceeds 256 definitions; narrow changed files or ranges"
            );
        }
    }
    all.sort_by(|a, b| {
        (&a.file, &a.definition_range, &a.symbol).cmp(&(&b.file, &b.definition_range, &b.symbol))
    });
    let limit = request.limit.unwrap_or(8).clamp(1, MAX_PAGE);
    let fixed_bytes = serde_json::to_vec(&(&deleted, &unresolved))?.len();
    const RESPONSE_OVERHEAD_RESERVE: usize = 2 * 1024;
    anyhow::ensure!(
        fixed_bytes.saturating_add(RESPONSE_OVERHEAD_RESERVE) < MAX_PAGE_BYTES,
        "deleted and unresolved scope evidence exceeds the response budget; narrow changed files"
    );
    let end = bounded_end(
        &all,
        offset,
        limit,
        MAX_PAGE_BYTES - fixed_bytes - RESPONSE_OVERHEAD_RESERVE,
    )?;
    let candidates = all.get(offset..end).unwrap_or_default().to_vec();
    anyhow::ensure!(
        serde_json::to_vec(&(&candidates, &deleted))?.len() <= MAX_PAGE_BYTES,
        "intent candidate response exceeds 96 KiB; narrow changed files or ranges"
    );
    anyhow::ensure!(
        accepted_revision(state)? == knowledge_revision,
        "knowledge changed during intent candidate lookup; retry"
    );
    let response = IntentCandidatePage {
        knowledge_revision,
        index: IntentIndexSnapshot {
            revision: Some(index_revision),
            producer,
            status,
            refresh_action,
        },
        scope_digest,
        candidates,
        next_cursor: (end < all.len())
            .then(|| encode_cursor(end, &request_digest, &cursor_revision, "intent")),
        unresolved,
        deleted,
    };
    anyhow::ensure!(
        serde_json::to_vec(&response)?.len() <= MAX_PAGE_BYTES,
        "intent candidate response exceeds 96 KiB; narrow changed files or ranges"
    );
    Ok(response)
}

fn range_array(range: SourceRange) -> [u32; 4] {
    [
        range.start.line,
        range.start.col,
        range.end.line,
        range.end.col,
    ]
}
fn wire_range(range: [u32; 4]) -> HarnessSourceRange {
    HarnessSourceRange {
        start: HarnessSourcePosition {
            line: range[0],
            col: range[1],
        },
        end: HarnessSourcePosition {
            line: range[2],
            col: range[3],
        },
    }
}
fn deleted_from_parts(
    file: &str,
    symbol: &str,
    name: Option<String>,
    definition: [u32; 4],
    enclosing: Option<[u32; 4]>,
    source_digest: &str,
) -> anyhow::Result<DeletedDefinitionScope> {
    let definition_range = wire_range(definition);
    let enclosing_range = enclosing.map(wire_range);
    Ok(DeletedDefinitionScope {
        file: file.into(),
        symbol: symbol.into(),
        name,
        definition_range,
        enclosing_range,
        before_digest: source_digest.into(),
        scope_digest: digest(&(
            file,
            symbol,
            definition_range,
            enclosing_range,
            source_digest,
        ))?,
    })
}

fn project_record(state: &AppState, iri: &str, ordinal: usize) -> anyhow::Result<PurposeCandidate> {
    let class = graph::require_information_record(state, &oxigraph::model::NamedNode::new(iri)?)?;
    let (title, literals, relations, assertion_digest) =
        super::reconciliation::candidate_assertions(state, iri)?;
    let kind = graph::local_name(&class).to_string();
    let code_class = state.resolve_code_class("CodeEntity")?;
    let mut legal_predicates = state
        .catalogue
        .legal_predicates(&state.store, &class, &code_class)
        .into_iter()
        .map(|edge| edge.predicate_local)
        .collect::<Vec<_>>();
    legal_predicates.sort();
    legal_predicates.dedup();
    Ok(PurposeCandidate {
        handle: format!("record_{ordinal}_{}", &assertion_digest[..12]),
        iri: iri.into(),
        kind,
        title,
        claim: CompleteClaim { literals },
        lifecycle: current_status(state, iri).unwrap_or_else(|| "accepted".into()),
        assertion_digest,
        relations,
        legal_predicates,
    })
}

fn entity_record_context(
    state: &AppState,
    symbol: &str,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut direct = Vec::new();
    let mut component = Vec::new();
    if let Some(dossier) =
        graph::get_entity_dossier(state, &graph::DossierTarget::Symbol(symbol.into()))?
    {
        for record in dossier.direct_records {
            if accepted_status(&record.status) && !direct.contains(&record.iri) {
                direct.push(record.iri)
            }
        }
        for record in dossier.component_records {
            if accepted_status(&record.status) && !component.contains(&record.iri) {
                component.push(record.iri)
            }
        }
    }
    direct.truncate(16);
    component.truncate(16);
    Ok((direct, component))
}

fn post_candidate(
    state: &AppState,
    file: &str,
    source_digest: &str,
    scope: DefinitionScope,
    basis: IntentScopeBasis,
    snapshot: &CandidateSnapshot<'_>,
) -> anyhow::Result<PostEditCandidate> {
    let symbol = scope.definition.entry.normalized_symbol;
    let (existing_record_iris, component_records) = entity_record_context(state, &symbol)?;
    let mut record_choices = purpose_records_for_file(state, file, &component_records)?;
    record_choices.retain(|record| !existing_record_iris.contains(&record.iri));
    let definition_range = Some(scope.definition.range.into());
    let enclosing_range = scope.enclosing_range.map(Into::into);
    let candidate_digest = digest(&(
        file,
        &symbol,
        source_digest,
        &definition_range,
        &enclosing_range,
        &basis,
        &existing_record_iris,
        &record_choices,
        snapshot.index_revision,
        snapshot.scope_digest,
        snapshot.index_status,
    ))?;
    Ok(PostEditCandidate {
        id: format!("entity_{}", &candidate_digest[..16]),
        file: file.into(),
        symbol: Some(symbol),
        name: scope.definition.entry.display_name,
        definition_range,
        enclosing_range,
        source_digest: source_digest.into(),
        scope_basis: basis,
        candidate_digest,
        existing_record_iris,
        record_choices,
    })
}

fn conservative_candidate(
    state: &AppState,
    file: &str,
    source_digest: &str,
    snapshot: &CandidateSnapshot<'_>,
) -> anyhow::Result<PostEditCandidate> {
    let record_choices = purpose_records_for_file(state, file, &[])?;
    let basis = IntentScopeBasis::ConservativeFile;
    let candidate_digest = digest(&(
        file,
        source_digest,
        &basis,
        &record_choices,
        snapshot.index_revision,
        snapshot.scope_digest,
        snapshot.index_status,
    ))?;
    Ok(PostEditCandidate {
        id: format!("file_{}", &candidate_digest[..16]),
        file: file.into(),
        symbol: None,
        name: None,
        definition_range: None,
        enclosing_range: None,
        source_digest: source_digest.into(),
        scope_basis: basis,
        candidate_digest,
        existing_record_iris: Vec::new(),
        record_choices,
    })
}

fn purpose_records_for_file(
    state: &AppState,
    file: &str,
    component_records: &[String],
) -> anyhow::Result<Vec<PurposeCandidate>> {
    // This is an inventory of legal current choices, not a semantic relevance
    // judgment. File-focused hits lead; bounded current records fill the page.
    let mut iris = component_records.to_vec();
    for item in graph::relevant_context_snapshot(state, Some(file), 16, false)?
        .into_iter()
        .filter(|item| is_record_kind(&item.kind))
    {
        if !iris.contains(&item.iri) {
            iris.push(item.iri)
        }
    }
    for item in graph::relevant_context_snapshot(state, None, 32, false)? {
        if !is_record_kind(&item.kind) {
            continue;
        }
        if !iris.contains(&item.iri) {
            iris.push(item.iri);
        }
        if iris.len() == 32 {
            break;
        }
    }
    iris.truncate(8);
    iris.into_iter()
        .enumerate()
        .map(|(i, iri)| project_record(state, &iri, i))
        .collect()
}

fn is_record_kind(kind: &str) -> bool {
    matches!(
        kind,
        "ArchitecturalDecision"
            | "Requirement"
            | "Constraint"
            | "Lesson"
            | "Pattern"
            | "AntiPattern"
    )
}

fn accepted_status(status: &str) -> bool {
    status.is_empty() || status.eq_ignore_ascii_case("accepted")
}

fn prove_changed_source(
    state: &AppState,
    changed: &ChangedFile,
) -> anyhow::Result<(String, Option<String>)> {
    match &changed.after_digest {
        Some(expected) => {
            let root = state.project_root().canonicalize()?;
            let path = root.join(&changed.file);
            let canonical = path.canonicalize()?;
            anyhow::ensure!(
                canonical.starts_with(&root) && canonical.is_file(),
                "current source is outside the project or not a file"
            );
            let before = std::fs::metadata(&canonical)?;
            anyhow::ensure!(
                before.len() <= MAX_SOURCE_BYTES,
                "current source exceeds the 16 MiB intent proof limit"
            );
            let mut bytes = Vec::with_capacity(before.len() as usize);
            std::fs::File::open(&canonical)?
                .take(MAX_SOURCE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            let after = std::fs::metadata(&canonical)?;
            anyhow::ensure!(
                bytes.len() as u64 == before.len()
                    && before.len() == after.len()
                    && before.modified()? == after.modified()?,
                "current source changed while proving its digest"
            );
            let actual = format!("{:x}", Sha256::digest(&bytes));
            anyhow::ensure!(
                &actual == expected,
                "after_digest does not match current source"
            );
            Ok((actual, Some(String::from_utf8(bytes)?)))
        }
        None => {
            anyhow::ensure!(
                !state.project_root().join(&changed.file).exists(),
                "deleted source still exists"
            );
            changed
                .before_digest
                .clone()
                .map(|digest| (digest, None))
                .ok_or_else(|| anyhow::anyhow!("deleted source needs before_digest"))
        }
    }
}

fn validate_source_ranges(source: &str, ranges: &[HarnessSourceRange]) -> anyhow::Result<()> {
    // Retain the final empty line: insertion at EOF after a trailing newline is
    // represented by its zero column and is a valid end-exclusive position.
    let lines = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    for range in ranges {
        for point in [range.start, range.end] {
            let line = lines
                .get(point.line as usize)
                .ok_or_else(|| anyhow::anyhow!("changed range is outside current source"))?;
            anyhow::ensure!(
                (point.col as usize) <= line.len() && line.is_char_boundary(point.col as usize),
                "changed range column is not a UTF-8 byte boundary in current source"
            );
        }
    }
    Ok(())
}

fn maybe_refresh(state: &AppState, policy: &IntentRefreshPolicy) -> IntentRefreshAction {
    if matches!(policy, IntentRefreshPolicy::None) {
        return IntentRefreshAction::NotRequested;
    }
    let Some(substrate) = state.substrate() else {
        return IntentRefreshAction::Unsupported;
    };
    let supported = substrate.meta().producers.len() == 1
        && substrate.meta().producers[0].name == "scip-python"
        && std::env::var_os("MOOSEDEV_SCIP_PYTHON")
            .is_some_and(|p| Path::new(&p).is_absolute() && Path::new(&p).is_file());
    if !supported {
        return IntentRefreshAction::Unsupported;
    }
    let producers: Vec<_> = crate::code::substrate::registry()
        .iter()
        .filter(|p| p.name == "scip-python")
        .copied()
        .collect();
    match crate::code::substrate::producer::run_index_with(
        &producers,
        &state.project_root(),
        &state.data_dir,
    ) {
        Ok(_) => {
            state.load_substrate(&state.project_root());
            IntentRefreshAction::Refreshed
        }
        Err(_) => IntentRefreshAction::Failed,
    }
}

fn validate_files(files: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(files.len() <= MAX_FILES, "at most 32 files");
    let mut unique = std::collections::BTreeSet::new();
    for f in files {
        validate_path(f)?;
        anyhow::ensure!(unique.insert(f), "duplicate intent file `{f}`");
    }
    Ok(())
}
fn intersects(a: SourceRange, b: SourceRange) -> bool {
    a.start < b.end && b.start < a.end
}
fn digest<T: serde::Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn encode_cursor(offset: usize, request: &str, revision: &str, kind: &str) -> String {
    format!("{offset}:{kind}:{request}:{revision}")
}
fn decode_cursor(
    cursor: Option<&str>,
    request: &str,
    revision: &str,
    kind: &str,
) -> anyhow::Result<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    let mut p = cursor.splitn(4, ':');
    let offset = p
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("invalid cursor"))?;
    anyhow::ensure!(
        p.next() == Some(kind) && p.next() == Some(request) && p.next() == Some(revision),
        "cursor belongs to a different or stale snapshot"
    );
    Ok(offset)
}
fn bounded_end<T: serde::Serialize>(
    items: &[T],
    offset: usize,
    limit: usize,
    byte_budget: usize,
) -> anyhow::Result<usize> {
    anyhow::ensure!(offset <= items.len(), "cursor offset is out of range");
    let mut end = offset;
    let mut bytes = 0;
    while end < items.len() && end - offset < limit {
        let n = serde_json::to_vec(&items[end])?.len();
        anyhow::ensure!(
            end > offset || n <= byte_budget,
            "one candidate exceeds the response budget"
        );
        if end > offset && bytes + n > byte_budget {
            break;
        }
        bytes += n;
        end += 1
    }
    Ok(end)
}

impl From<HarnessSourceRange> for SourceRange {
    fn from(r: HarnessSourceRange) -> Self {
        Self {
            start: crate::code::substrate::Position {
                line: r.start.line,
                col: r.start.col,
            },
            end: crate::code::substrate::Position {
                line: r.end.line,
                col: r.end.col,
            },
        }
    }
}
impl From<SourceRange> for HarnessSourceRange {
    fn from(r: SourceRange) -> Self {
        Self {
            start: HarnessSourcePosition {
                line: r.start.line,
                col: r.start.col,
            },
            end: HarnessSourcePosition {
                line: r.end.line,
                col: r.end.col,
            },
        }
    }
}
