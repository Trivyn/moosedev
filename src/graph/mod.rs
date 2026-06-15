//! Durable project knowledge graph: server state, instance-IRI minting, typed
//! capture (built on MOOSE's cache-coherent `kg::assert_instance`), and NLQ
//! query (via MOOSE's `execute_graph_walk_nlq_with_context`, answer + trace).
//!
//! MOOSEDev owns the *domain* semantics (what a decision is, IRI conventions,
//! the durable store); MOOSE owns the *mechanics* (transactional write + index
//! coherence; symbolic-first graph-walk query).

use std::path::Path;
use std::sync::Arc;

use moose::entity_index::EntityIndexCache;
use moose::kg::{assert_instance, AssertionLiteral, DatatypeAssertion, InstanceAssertion};
use moose::moose_ontology::MooseOntologyCache;
use moose::pipeline::execute_graph_walk_nlq_with_context;
use moose::traits::EngineConfig;
use moose::types::{CompactVocabulary, HybridConfig, LlmAssistLevel, PipelineTimings, WalkBudgets};
use oxigraph::store::Store;

use crate::llm::{LlmConfig, OpenAiCompatClient};
use crate::ontology::{self, MooseDevOntologyResolver};

/// Named graph holding recorded knowledge instances (the durable project KG).
pub const PROJECT_KG_GRAPH_IRI: &str = "https://moosedev.dev/kg/project";
/// Namespace of the architecture ontology's terms.
pub const ARCHITECTURE_NS: &str = "https://moosedev.dev/ontologies/architecture#";
/// Architecture-ontology predicate IRIs the capture tools map their fields onto.
/// (Stub-ontology predicates; revisited when the generated ontology lands.)
pub const ARCH_DESCRIPTION: &str = "https://moosedev.dev/ontologies/architecture#description";
pub const ARCH_STATUS: &str = "https://moosedev.dev/ontologies/architecture#status";

/// Long-lived server state: the durable store, the entity-index cache MOOSE keeps
/// coherent on write, loaded vocabularies, the query `EngineConfig`, and the LLM
/// sensor + ontology resolver used by the query pipeline.
pub struct AppState {
    pub store: Store,
    pub entity_index: Arc<EntityIndexCache>,
    pub moose_cache: Arc<MooseOntologyCache>,
    pub arch_vocab: CompactVocabulary,
    pub engine_config: EngineConfig,
    pub llm: OpenAiCompatClient,
    pub ontology_resolver: MooseDevOntologyResolver,
    pub model: String,
}

impl AppState {
    /// Open the persistent store, initialize MOOSE, load the architecture
    /// ontology, build the entity-index cache, and assemble the query engine
    /// configuration (LLM endpoint + assist level read from the environment).
    pub fn bootstrap(data_dir: &Path, architecture_ttl: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
        let store = Store::open(data_dir.join("kg"))
            .map_err(|e| anyhow::anyhow!("open persistent store: {e}"))?;
        let moose_cache =
            moose::initialize(&store).map_err(|e| anyhow::anyhow!("moose::initialize: {e:?}"))?;
        let arch_vocab = ontology::load_architecture(&store, architecture_ttl)?;
        let entity_index = Arc::new(EntityIndexCache::new(64));

        let llm_cfg = LlmConfig::from_env();
        let llm = OpenAiCompatClient::new(llm_cfg.base_url, llm_cfg.api_key);
        let ontology_resolver = MooseDevOntologyResolver::new();

        let engine_config = EngineConfig {
            context_budget: 8_192,
            budgets: WalkBudgets::default(),
            hybrid: HybridConfig::default(),
            chat: None,
            moose_cache: moose_cache.clone(),
            llm_assist_level: assist_level_from_env(),
            response_cache: None,
            embedding_store: None,
            category_mappings: Default::default(),
            clarification_provenance_writer: None,
            training_mode: false,
            domain_adapters: Vec::new(),
        };

        Ok(Self {
            store,
            entity_index,
            moose_cache,
            arch_vocab,
            engine_config,
            llm,
            ontology_resolver,
            model: llm_cfg.model,
        })
    }

    /// Resolve a knowledge `kind` (e.g. "ArchitecturalDecision") to its class IRI,
    /// validating it against the loaded architecture ontology.
    pub fn resolve_class(&self, kind: &str) -> anyhow::Result<String> {
        let class_iri = format!("{ARCHITECTURE_NS}{kind}");
        if self.arch_vocab.classes.iter().any(|c| c.iri == class_iri) {
            Ok(class_iri)
        } else {
            anyhow::bail!("unknown kind {kind:?}: not a class in the architecture ontology")
        }
    }
}

/// LLM assist level from `MOOSEDEV_LLM_ASSIST_LEVEL` (0–5); defaults to Standard.
fn assist_level_from_env() -> LlmAssistLevel {
    match std::env::var("MOOSEDEV_LLM_ASSIST_LEVEL")
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
    {
        Some(0) => LlmAssistLevel::PureSymbolic,
        Some(2) => LlmAssistLevel::RelaxedExtraction,
        Some(3) => LlmAssistLevel::AssistedPlanning,
        Some(4) => LlmAssistLevel::AssistedValidation,
        Some(5) => LlmAssistLevel::FallbackExecutor,
        _ => LlmAssistLevel::Standard,
    }
}

/// Mint a fresh instance IRI for a class local name, e.g.
/// `https://moosedev.dev/kg/ArchitecturalDecision/<uuid>`.
pub fn mint_instance_iri(class_local: &str) -> String {
    format!(
        "https://moosedev.dev/kg/{}/{}",
        class_local,
        uuid::Uuid::new_v4()
    )
}

/// A validated knowledge item to record: a resolved class plus its literal
/// property assertions as `(predicate_iri, value)` pairs. Domain-neutral — the
/// caller maps its fields to predicates, so new knowledge classes need no change
/// to the writer below.
pub struct RecordInput {
    pub class_iri: String,
    pub class_local: String,
    pub properties: Vec<(String, String)>,
}

/// Record a typed knowledge instance into the durable project KG via MOOSE's
/// cache-coherent assertion primitive. Returns the minted subject IRI.
pub fn record_instance(state: &AppState, input: &RecordInput) -> anyhow::Result<String> {
    let subject = mint_instance_iri(&input.class_local);
    let datatype_props: Vec<DatatypeAssertion> = input
        .properties
        .iter()
        .map(|(predicate, value)| DatatypeAssertion {
            predicate_iri: predicate.as_str(),
            literal: AssertionLiteral::Simple(value.as_str()),
        })
        .collect();

    let assertion = InstanceAssertion {
        graph_iri: PROJECT_KG_GRAPH_IRI,
        subject_iri: &subject,
        class_iri: &input.class_iri,
        datatype_props: &datatype_props,
        object_props: &[],
    };

    assert_instance(&state.store, &state.entity_index, &assertion, None)
        .map_err(|e| anyhow::anyhow!("assert_instance: {e:?}"))?;
    Ok(subject)
}

/// Result of an NLQ query: the synthesized answer, a confidence label, and a
/// human-readable reasoning trace (auditability — invariant #6).
pub struct QueryResult {
    pub answer: String,
    pub confidence: String,
    pub trace: String,
}

/// Answer a natural-language question over the project KG using MOOSE's
/// symbolic-first graph-walk pipeline. Returns the answer plus an execution
/// trace; the LLM sensor fires only at assist levels ≥ Standard.
pub async fn query(state: &AppState, nlq: &str) -> anyhow::Result<QueryResult> {
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];
    let output = execute_graph_walk_nlq_with_context(
        &state.store,
        &state.llm,
        &state.ontology_resolver,
        &state.engine_config,
        nlq,
        &data_graphs,
        &state.model,
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
        let stage = st
            .stage_iri
            .rsplit(['/', '#'])
            .next()
            .unwrap_or(st.stage_iri.as_str());
        let detail = st.detail.as_deref().unwrap_or("");
        lines.push(format!("  • {stage} ({:.1}ms) {detail}", st.duration_ms));
    }
    lines.join("\n")
}
