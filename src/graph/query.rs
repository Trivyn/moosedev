//! Natural-language query wrapper over MOOSE graph-walk NLQ.
//! The public API returns both answer and trace for auditability.

use std::collections::HashSet;
use std::sync::Arc;

use moose::entity_index::EntityIndexCache;
use moose::pipeline::execute_graph_walk_nlq_with_context;
use moose::traits::{EngineConfig, LlmClient};
use moose::types::{LlmAssistLevel, PipelineTimings};
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};
use oxigraph::store::Store;

use super::lifecycle::is_hidden_from_authoritative_reads;
use super::state::AppState;
use super::util::local_name;
use super::PROJECT_KG_GRAPH_IRI;

/// Result of an NLQ query: the synthesized answer, a confidence label, and a
/// human-readable reasoning trace (auditability — invariant #6).
pub struct QueryResult {
    pub answer: String,
    pub confidence: String,
    pub trace: String,
}

const SCHEMA_QUERY_SPEC_INTERNAL_ERROR: &str =
    "Schema query intent set but no SchemaQuerySpec was attached";

/// Answer a natural-language question over the project KG using MOOSE's
/// symbolic-first graph-walk pipeline. Returns the answer plus an execution
/// trace; the LLM sensor fires only at assist levels ≥ Standard.
pub async fn query(state: &AppState, nlq: &str) -> anyhow::Result<QueryResult> {
    // Fork the client so token usage is attributed to *this* query only (safe
    // under concurrent backend use), then surface the NLQ model's token cost in
    // the trace — the benchmark harness parses this to account B2's internal
    // LLM cost.
    let llm = state.llm.with_fresh_usage();
    let mut result = query_with_llm_client(state, &llm, &state.model, nlq).await?;
    let (prompt, completion) = llm.take_usage();
    result.trace.push_str(&format!(
        "\ntokens: prompt={prompt} completion={completion}"
    ));
    Ok(result)
}

/// Variant of [`query`] that lets integration tests inject a deterministic LLM
/// sensor while still exercising MOOSEDev's wrapper behavior.
#[doc(hidden)]
pub async fn query_with_llm_client(
    state: &AppState,
    llm: &dyn LlmClient,
    model: &str,
    nlq: &str,
) -> anyhow::Result<QueryResult> {
    let first = execute_query(state, llm, &state.engine_config, model, nlq).await?;
    if state.engine_config.llm_assist_level != LlmAssistLevel::PureSymbolic
        && first.answer.contains(SCHEMA_QUERY_SPEC_INTERNAL_ERROR)
    {
        let mut fallback_config = state.engine_config.clone();
        fallback_config.llm_assist_level = LlmAssistLevel::PureSymbolic;
        return execute_query(state, llm, &fallback_config, model, nlq).await;
    }

    Ok(first)
}

async fn execute_query(
    state: &AppState,
    llm: &dyn LlmClient,
    engine_config: &EngineConfig,
    model: &str,
    nlq: &str,
) -> anyhow::Result<QueryResult> {
    // Fresh inferred edges before a structural walk (the query class that benefits most).
    state.ensure_enriched();
    let query_store = authoritative_query_store(state)?;
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];
    let output = execute_graph_walk_nlq_with_context(
        &query_store,
        llm,
        &state.ontology_resolver,
        engine_config,
        nlq,
        &data_graphs,
        model,
        Arc::new(EntityIndexCache::new(64)),
        None,
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("graph walk failed: {e:?}"))?;

    let trace = render_trace(&output.timings);

    if output.clarification.is_some() {
        return Ok(QueryResult {
            answer: "The query needs clarification (not supported in v1 single-shot mode)."
                .to_string(),
            confidence: "low".to_string(),
            trace,
        });
    }

    Ok(QueryResult {
        answer: output.synthesis.summary,
        confidence: output.synthesis.confidence,
        trace,
    })
}

/// Build a request-local read projection for authoritative NLQ. Proposed and
/// rejected records remain in the durable project graph for inbox/audit tools,
/// but neither those subjects nor an incident edge may enter a graph walk.
///
/// The domain and shape graphs are copied unchanged because MOOSE derives its
/// query vocabulary from the same reader it uses for project data.
fn authoritative_query_store(state: &AppState) -> anyhow::Result<Store> {
    let project_graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let status = NamedNodeRef::new(&state.capture.status)?;
    // One readable transaction pins both passes to the same Oxigraph snapshot:
    // a proposal committed between status discovery and graph copying must not
    // enter the projection without having been classified as hidden.
    let transaction = state.store.start_transaction()?;
    let mut hidden = HashSet::new();
    for quad in transaction.quads_for_pattern(
        None,
        Some(status),
        None,
        Some(GraphNameRef::NamedNode(project_graph)),
    ) {
        let quad = quad?;
        let Term::Literal(value) = &quad.object else {
            continue;
        };
        if !is_hidden_from_authoritative_reads(value.value()) {
            continue;
        }
        if let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = &quad.subject {
            hidden.insert(subject.as_str().to_string());
        }
    }

    let graph_iris = [
        crate::ontology::SE_DOMAIN_GRAPH_IRI,
        crate::ontology::SE_SHAPES_GRAPH_IRI,
        crate::ontology::ARCH_DOMAIN_GRAPH_IRI,
        crate::ontology::ARCH_SHAPES_GRAPH_IRI,
        crate::ontology::CODE_DOMAIN_GRAPH_IRI,
        crate::ontology::CODE_SHAPES_GRAPH_IRI,
        PROJECT_KG_GRAPH_IRI,
    ];
    let mut visible = Vec::new();
    for graph_iri in graph_iris {
        let graph = NamedNodeRef::new(graph_iri)?;
        for quad in
            transaction.quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph)))
        {
            let quad = quad?;
            if graph_iri == PROJECT_KG_GRAPH_IRI {
                let hidden_subject = match &quad.subject {
                    oxigraph::model::NamedOrBlankNode::NamedNode(subject) => {
                        hidden.contains(subject.as_str())
                    }
                    oxigraph::model::NamedOrBlankNode::BlankNode(_) => false,
                };
                let hidden_object = match &quad.object {
                    Term::NamedNode(object) => hidden.contains(object.as_str()),
                    _ => false,
                };
                if hidden_subject || hidden_object {
                    continue;
                }
            }
            visible.push(quad);
        }
    }
    drop(transaction);

    let store = Store::new()?;
    store.extend(visible)?;
    Ok(store)
}

/// Render MOOSE's per-stage timings into a compact, human-readable trace.
fn render_trace(t: &PipelineTimings) -> String {
    let mut lines = vec![
        format!("total: {:.1}ms", t.total.as_secs_f64() * 1000.0),
        format!("assist level: {:?}", t.llm_assist_level),
        format!("stages executed: {}", t.stages_executed),
        format!("triples walked: {}", t.triples_walked),
    ];
    if let Some(strategy) = &t.walk_strategy_label {
        lines.push(format!("walk strategy: {strategy}"));
    }
    if t.llm_sensors_fired.is_empty() {
        lines.push("LLM sensors fired: none (pure symbolic path)".to_string());
    } else {
        lines.push(format!(
            "LLM sensors fired: {}",
            t.llm_sensors_fired.join(", ")
        ));
    }
    for st in &t.stage_traces {
        let stage = local_name(&st.stage_iri);
        let detail = st.detail.as_deref().unwrap_or("");
        lines.push(format!("  • {stage} ({:.1}ms) {detail}", st.duration_ms));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Utc;
    use oxigraph::model::NamedNode;

    use super::*;
    use crate::graph::{self, RecordInput};

    fn record(state: &AppState, kind: &str, title: &str, status: Option<&str>) -> String {
        let mut properties = vec![(state.capture.title.clone(), title.to_string())];
        if let Some(status) = status {
            properties.push((state.capture.status.clone(), status.to_string()));
        }
        graph::record_instance(
            state,
            &RecordInput {
                class_iri: state.resolve_class(kind).unwrap(),
                class_local: kind.to_string(),
                properties,
            },
            "tester",
            Utc::now(),
        )
        .unwrap()
    }

    fn contains_subject(store: &Store, iri: &str) -> bool {
        let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
        store
            .quads_for_pattern(
                Some(NamedNodeRef::new(iri).unwrap().into()),
                None,
                None,
                Some(GraphNameRef::NamedNode(graph)),
            )
            .next()
            .is_some()
    }

    #[test]
    fn query_projection_removes_hidden_records_and_every_incident_edge() {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-query-authoritative-view-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
        let state = AppState::bootstrap(&dir, &ontology_dir).unwrap();

        let component = record(&state, "SystemComponent", "Query target", Some("accepted"));
        let proposed = record(
            &state,
            "ArchitecturalDecision",
            "Proposed query secret",
            Some("proposed"),
        );
        let rejected = record(
            &state,
            "ArchitecturalDecision",
            "Rejected query secret",
            Some("rejected"),
        );
        let superseded = record(
            &state,
            "ArchitecturalDecision",
            "Superseded audit record",
            Some("superseded"),
        );
        let deprecated = record(
            &state,
            "ArchitecturalDecision",
            "Deprecated audit record",
            Some("deprecated"),
        );
        let legacy = record(&state, "ArchitecturalDecision", "Status-less legacy", None);
        let legacy_node = NamedNodeRef::new(&legacy).unwrap();
        let status = NamedNodeRef::new(&state.capture.status).unwrap();
        let project = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
        let legacy_status: Vec<_> = state
            .store
            .quads_for_pattern(
                Some(legacy_node.into()),
                Some(status),
                None,
                Some(GraphNameRef::NamedNode(project)),
            )
            .collect::<Result<_, _>>()
            .unwrap();
        let mut txn = state.store.start_transaction().unwrap();
        for quad in &legacy_status {
            txn.remove(quad.as_ref());
        }
        txn.commit().unwrap();
        graph::relate(&state, &proposed, "concerns", &component).unwrap();
        graph::relate(&state, &rejected, "concerns", &component).unwrap();
        state.ensure_enriched();

        let view = authoritative_query_store(&state).unwrap();
        assert!(!contains_subject(&view, &proposed));
        assert!(!contains_subject(&view, &rejected));
        for visible in [&component, &superseded, &deprecated, &legacy] {
            assert!(
                contains_subject(&view, visible),
                "{visible} should remain visible"
            );
        }

        for hidden in [&proposed, &rejected] {
            let hidden = NamedNode::new(hidden).unwrap();
            assert!(
                view.quads_for_pattern(
                    None,
                    None,
                    Some(hidden.as_ref().into()),
                    Some(GraphNameRef::NamedNode(project)),
                )
                .next()
                .is_none(),
                "incoming edges to hidden records must be removed"
            );
        }
        for graph_iri in [
            crate::ontology::SE_DOMAIN_GRAPH_IRI,
            crate::ontology::SE_SHAPES_GRAPH_IRI,
            crate::ontology::ARCH_DOMAIN_GRAPH_IRI,
            crate::ontology::ARCH_SHAPES_GRAPH_IRI,
            crate::ontology::CODE_DOMAIN_GRAPH_IRI,
            crate::ontology::CODE_SHAPES_GRAPH_IRI,
        ] {
            assert!(
                view.quads_for_pattern(
                    None,
                    None,
                    None,
                    Some(GraphNameRef::NamedNode(
                        NamedNodeRef::new(graph_iri).unwrap()
                    )),
                )
                .next()
                .is_some(),
                "query projection must retain {graph_iri}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
