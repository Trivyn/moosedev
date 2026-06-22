//! Natural-language query wrapper over MOOSE graph-walk NLQ.
//! The public API returns both answer and trace for auditability.

use moose::pipeline::execute_graph_walk_nlq_with_context;
use moose::traits::{EngineConfig, LlmClient};
use moose::types::{LlmAssistLevel, PipelineTimings};

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
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];
    let output = execute_graph_walk_nlq_with_context(
        &state.store,
        llm,
        &state.ontology_resolver,
        engine_config,
        nlq,
        &data_graphs,
        model,
        state.entity_index.clone(),
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
