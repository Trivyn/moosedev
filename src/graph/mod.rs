//! Durable project knowledge graph: server state, instance-IRI minting, typed
//! capture (built on MOOSE's cache-coherent `kg::assert_instance`), and NLQ
//! query (via MOOSE's `execute_graph_walk_nlq_with_context`, answer + trace).
//!
//! MOOSEDev owns the *domain* semantics (what a decision is, IRI conventions,
//! the durable store); MOOSE owns the *mechanics* (transactional write + index
//! coherence; symbolic-first graph-walk query).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use moose::embeddings::vec_store::VecStore;
use moose::entity_index::EntityIndexCache;
use moose::kg::{
    assert_instance, AssertionLiteral, DatatypeAssertion, InstanceAssertion, ObjectAssertion,
};
use moose::moose_ontology::MooseOntologyCache;
use moose::pipeline::execute_graph_walk_nlq_with_context;
use moose::traits::EngineConfig;
use moose::types::{
    CompactVocabulary, HybridConfig, LlmAssistLevel, PipelineTimings, VocabularyEntry, WalkBudgets,
};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use oxigraph::store::Store;

use crate::llm::{LlmConfig, OpenAiCompatClient};
use crate::ontology::{self, MooseDevOntologyResolver};

/// Named graph holding recorded knowledge instances (the durable project KG).
pub const PROJECT_KG_GRAPH_IRI: &str = "https://moosedev.dev/kg/project";
/// Local names (in the architecture ontology) of the datatype properties the
/// capture tool populates. The code couples to the ontology only by these stable
/// local names — the full IRIs (namespace included) are resolved from the loaded
/// vocabulary at bootstrap, so the ontology can be regenerated under a different
/// namespace with no code change.
///
/// `hasTitle` is the label property of `InformationRecord`, the root every capture
/// class inherits. A fully class-generic title would read each class's
/// `trivyn:labelProperty` from the ontology, but MOOSE doesn't yet surface that
/// annotation (`VocabularyEntry.label_property` stays unpopulated for these), so
/// binding to the shared root property is the pragmatic choice for v1's capture
/// classes (all `InformationRecord` subclasses).
const CAPTURE_TITLE_LOCAL: &str = "hasTitle";
const CAPTURE_DESCRIPTION_LOCAL: &str = "hasDescription";
const CAPTURE_STATUS_LOCAL: &str = "hasLifecycleStatus";
const CAPTURE_AUTHOR_LOCAL: &str = "hasAuthor";
const CAPTURE_TIMESTAMP_LOCAL: &str = "hasTimestamp";
const DEFAULT_LIFECYCLE_STATUS: &str = "proposed";
const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

/// Architecture-ontology predicate IRIs the capture tool writes, resolved from
/// the loaded vocabulary at bootstrap by local name (see the `CAPTURE_*_LOCAL`
/// constants). Resolving up front fails fast if the ontology lacks an expected
/// property and keeps the volatile namespace out of the code.
#[derive(Debug, Clone)]
pub struct CapturePredicates {
    pub title: String,
    pub description: String,
    pub status: String,
    pub author: String,
    pub timestamp: String,
}

impl CapturePredicates {
    fn resolve(vocab: &CompactVocabulary) -> anyhow::Result<Self> {
        Ok(Self {
            title: datatype_property_iri(vocab, CAPTURE_TITLE_LOCAL)?,
            description: datatype_property_iri(vocab, CAPTURE_DESCRIPTION_LOCAL)?,
            status: datatype_property_iri(vocab, CAPTURE_STATUS_LOCAL)?,
            author: datatype_property_iri(vocab, CAPTURE_AUTHOR_LOCAL)?,
            timestamp: datatype_property_iri(vocab, CAPTURE_TIMESTAMP_LOCAL)?,
        })
    }
}

/// Find a vocabulary entry's full IRI by its local name — the one place the code
/// looks a term up in the loaded ontology, keeping the volatile namespace out.
fn iri_by_local_name(entries: &[VocabularyEntry], local: &str) -> Option<String> {
    entries
        .iter()
        .find(|e| e.local_name == local)
        .map(|e| e.iri.clone())
}

/// Resolve a datatype property's full IRI from the loaded vocabulary by local name.
fn datatype_property_iri(vocab: &CompactVocabulary, local: &str) -> anyhow::Result<String> {
    iri_by_local_name(&vocab.datatype_properties, local).ok_or_else(|| {
        anyhow::anyhow!("architecture ontology is missing datatype property {local:?}")
    })
}

/// Resolve an object property's (relation's) full IRI from the loaded vocabulary
/// by local name — the relation analogue of [`datatype_property_iri`], keeping the
/// volatile namespace out of the code.
fn object_property_iri(vocab: &CompactVocabulary, local: &str) -> anyhow::Result<String> {
    iri_by_local_name(&vocab.object_properties, local).ok_or_else(|| {
        anyhow::anyhow!("architecture ontology is missing object property {local:?}")
    })
}

/// Long-lived server state: the durable store, the entity-index cache MOOSE keeps
/// coherent on write, loaded vocabularies, the query `EngineConfig`, and the LLM
/// sensor + ontology resolver used by the query pipeline.
pub struct AppState {
    pub store: Store,
    pub entity_index: Arc<EntityIndexCache>,
    pub moose_cache: Arc<MooseOntologyCache>,
    pub arch_vocab: CompactVocabulary,
    pub capture: CapturePredicates,
    /// Ontology embedding vectors (L2 alignment tier); `None` until
    /// `build_alignment_index` runs. Also mirrored into `engine_config`.
    pub vector_store: Option<Arc<VecStore>>,
    pub engine_config: EngineConfig,
    pub llm: OpenAiCompatClient,
    pub ontology_resolver: MooseDevOntologyResolver,
    pub model: String,
    /// Data dir (the persistent KG store and the built vector DB live here).
    pub data_dir: PathBuf,
}

impl AppState {
    /// Open the persistent store, initialize MOOSE, load the shipped domain
    /// ontologies + SHACL shape graphs from `ontology_dir`, resolve the capture
    /// predicates from the loaded vocabulary, build the entity-index cache, and
    /// assemble the query engine configuration (LLM endpoint + assist level read
    /// from the environment).
    pub fn bootstrap(data_dir: &Path, ontology_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
        let store = Store::open(data_dir.join("kg"))
            .map_err(|e| anyhow::anyhow!("open persistent store: {e}"))?;
        let moose_cache =
            moose::initialize(&store).map_err(|e| anyhow::anyhow!("moose::initialize: {e:?}"))?;
        let arch_vocab = ontology::load_ontologies(&store, ontology_dir)?;
        let capture = CapturePredicates::resolve(&arch_vocab)?;
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
            capture,
            engine_config,
            llm,
            ontology_resolver,
            model: llm_cfg.model,
            vector_store: None,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// Build the ontology embedding vector store (MOOSE's L2 alignment tier) from
    /// the loaded domain graphs, hold it on `self`, and wire it into the query
    /// engine config. Kept separate from `bootstrap` so non-aligning paths (e.g.
    /// the M1 capture/query tests) don't pay the embedding-backbone load.
    pub async fn build_alignment_index(&mut self) -> anyhow::Result<()> {
        let db_path = self.data_dir.join("ontology-vectors.db");
        let vec_store = crate::vectors::build_and_open(
            &self.store,
            &[
                ontology::SE_DOMAIN_GRAPH_IRI,
                ontology::ARCH_DOMAIN_GRAPH_IRI,
            ],
            &db_path,
        )
        .await?;
        let vec_store = Arc::new(vec_store);
        self.engine_config.embedding_store = Some(vec_store.clone());
        self.vector_store = Some(vec_store);
        Ok(())
    }

    /// Resolve a knowledge `kind` (e.g. "ArchitecturalDecision") to its class IRI
    /// by local-name lookup in the loaded architecture vocabulary — so the class's
    /// full IRI (and namespace) comes from the ontology, not from code.
    pub fn resolve_class(&self, kind: &str) -> anyhow::Result<String> {
        iri_by_local_name(&self.arch_vocab.classes, kind).ok_or_else(|| {
            anyhow::anyhow!("unknown kind {kind:?}: not a class in the architecture ontology")
        })
    }

    /// Resolve a relation local name (e.g. "supersedes", "hasRationale") to its
    /// full IRI from the loaded architecture vocabulary.
    pub fn resolve_object_property(&self, local: &str) -> anyhow::Result<String> {
        object_property_iri(&self.arch_vocab, local)
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
pub fn record_instance(
    state: &AppState,
    input: &RecordInput,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<String> {
    record_instance_with_relations(state, input, &[], author, when)
}

/// Like [`record_instance`], but also writes IRI-valued relations
/// `(predicate_iri, object_iri)` — e.g. `isMotivatedBy`, `supersedes`. This is the
/// enabling layer for typed links between records (invariant #2): the writer
/// previously always passed an empty `object_props` slice, so no relation could be
/// captured. Resolve `predicate_iri` from the ontology via
/// [`AppState::resolve_object_property`].
pub fn record_instance_with_relations(
    state: &AppState,
    input: &RecordInput,
    object_props: &[(String, String)],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<String> {
    let subject = mint_instance_iri(&input.class_local);
    let timestamp = when.to_rfc3339();
    let mut datatype_props: Vec<DatatypeAssertion> = input
        .properties
        .iter()
        .map(|(predicate, value)| DatatypeAssertion {
            predicate_iri: predicate.as_str(),
            literal: AssertionLiteral::Simple(value.as_str()),
        })
        .collect();
    if !has_property(input, &state.capture.author) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.author.as_str(),
            literal: AssertionLiteral::Simple(author),
        });
    }
    if !has_property(input, &state.capture.timestamp) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.timestamp.as_str(),
            literal: AssertionLiteral::Typed {
                value: timestamp.as_str(),
                datatype_iri: XSD_DATETIME,
            },
        });
    }
    if !has_property(input, &state.capture.status) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.status.as_str(),
            literal: AssertionLiteral::Simple(DEFAULT_LIFECYCLE_STATUS),
        });
    }

    let object_assertions: Vec<ObjectAssertion> = object_props
        .iter()
        .map(|(predicate, object)| ObjectAssertion {
            predicate_iri: predicate.as_str(),
            object_iri: object.as_str(),
        })
        .collect();

    let assertion = InstanceAssertion {
        graph_iri: PROJECT_KG_GRAPH_IRI,
        subject_iri: &subject,
        class_iri: &input.class_iri,
        datatype_props: &datatype_props,
        object_props: &object_assertions,
    };

    assert_instance(&state.store, &state.entity_index, &assertion, None)
        .map_err(|e| anyhow::anyhow!("assert_instance: {e:?}"))?;
    Ok(subject)
}

/// Check whether the caller already supplied a property so write-path defaults
/// do not duplicate explicit values.
fn has_property(input: &RecordInput, predicate_iri: &str) -> bool {
    input
        .properties
        .iter()
        .any(|(predicate, _)| predicate == predicate_iri)
}

/// A decision change: the replacement to record, the decision it supersedes, and
/// the rationale (the *why*) for the change.
pub struct SupersedeInput {
    pub superseded_iri: String,
    pub new: RecordInput,
    pub rationale: String,
}

/// IRIs minted/affected by a supersede.
pub struct SupersedeOutcome {
    pub new_iri: String,
    pub rationale_iri: String,
    pub superseded_iri: String,
}

/// The write-path "stamp" applied to a captured instance: the capture predicate
/// IRIs plus the author, timestamp, and lifecycle status defaults to add when the
/// caller didn't supply them.
struct CaptureStamp<'a> {
    capture: &'a CapturePredicates,
    author: &'a str,
    timestamp: &'a str,
    status: &'a str,
}

/// Build the owned quads for one capture instance in the project graph: its type,
/// the caller's literal props, its IRI-valued relations, and the write-path
/// defaults (author, typed timestamp, lifecycle status) when the caller didn't
/// supply them. Returns quads rather than asserting so a supersede can commit
/// several instances *plus* a status change in one transaction. The default set
/// mirrors `record_instance_with_relations` — keep the two in sync.
fn capture_instance_quads(
    subject_iri: &str,
    class_iri: &str,
    literal_props: &[(String, String)],
    object_props: &[(String, String)],
    stamp: &CaptureStamp<'_>,
) -> anyhow::Result<Vec<Quad>> {
    let capture = stamp.capture;
    let author = stamp.author;
    let timestamp_rfc3339 = stamp.timestamp;
    let status = stamp.status;
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let subject = NamedNode::new(subject_iri)
        .map_err(|e| anyhow::anyhow!("invalid subject IRI {subject_iri}: {e}"))?;

    let mut quads = vec![Quad::new(
        subject.clone(),
        NamedNode::new(moose::RDF_TYPE)?,
        NamedNode::new(class_iri)
            .map_err(|e| anyhow::anyhow!("invalid class IRI {class_iri}: {e}"))?,
        graph.clone(),
    )];
    for (predicate, value) in literal_props {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(predicate)?,
            Literal::new_simple_literal(value.as_str()),
            graph.clone(),
        ));
    }
    for (predicate, object) in object_props {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(predicate)?,
            NamedNode::new(object)?,
            graph.clone(),
        ));
    }

    // Write-path defaults, only when the caller didn't supply them (mirrors
    // `record_instance_with_relations`). Timestamp is typed xsd:dateTime to satisfy
    // the InformationRecord shape; author/status are plain strings.
    let supplied = |p: &str| literal_props.iter().any(|(k, _)| k == p);
    if !supplied(&capture.author) {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(&capture.author)?,
            Literal::new_simple_literal(author),
            graph.clone(),
        ));
    }
    if !supplied(&capture.timestamp) {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(&capture.timestamp)?,
            Literal::new_typed_literal(timestamp_rfc3339, NamedNode::new(XSD_DATETIME)?),
            graph.clone(),
        ));
    }
    if !supplied(&capture.status) {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(&capture.status)?,
            Literal::new_simple_literal(status),
            graph,
        ));
    }
    Ok(quads)
}

/// `rdfs:subClassOf` — class-subsumption predicate (moose's const set omits it).
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// True if `class_iri` equals `ancestor_iri` or is a transitive `rdfs:subClassOf`
/// of it, per the loaded ontology. Bounded, cycle-safe walk over subClassOf edges
/// in any graph — class axioms live in the ontology graphs, not the project graph.
fn is_subclass_of(store: &Store, class_iri: &str, ancestor_iri: &str) -> bool {
    let sub_class_of = NamedNodeRef::new_unchecked(RDFS_SUBCLASS_OF);
    let mut stack = vec![class_iri.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == ancestor_iri {
            return true;
        }
        if !seen.insert(cur.clone()) {
            continue;
        }
        let Ok(node) = NamedNode::new(&cur) else {
            continue;
        };
        for q in store
            .quads_for_pattern(Some(node.as_ref().into()), Some(sub_class_of), None, None)
            .flatten()
        {
            if let Term::NamedNode(parent) = q.object {
                stack.push(parent.as_str().to_string());
            }
        }
    }
    false
}

/// Record a new knowledge item that supersedes an existing one, capture *why* it
/// changed as a linked `Rationale`, and mark the old item `superseded` — preserving
/// it as history (it is never deleted). The replacement is recorded with the SAME
/// class as the superseded item (type-preserving), so the caller's `new.class_*`
/// fields are ignored. Atomic: the new item, the `Rationale` node, the
/// `supersedes`/`hasRationale` edges, and the old item's status change all commit
/// in one transaction; the entity index is invalidated once on success. The
/// superseded subject must already be an `InformationRecord` (or subclass) in the
/// project graph — else this errors and writes nothing.
pub fn supersede_decision(
    state: &AppState,
    input: &SupersedeInput,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<SupersedeOutcome> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);

    // Precondition: the superseded subject must be a recorded knowledge item — an
    // instance of :InformationRecord (or a subclass). We then mint the replacement
    // with that SAME class (type-preserving): a Requirement is superseded by a
    // Requirement, a Constraint by a Constraint, and so on. This prevents nonsense
    // cross-kind supersedes and keeps the supersedes/hasRationale edges on a class
    // whose ontology domain is :InformationRecord. (Previously hardcoded to
    // ArchitecturalDecision, which blocked superseding any other knowledge class.)
    let old_subject = NamedNode::new(&input.superseded_iri)
        .map_err(|e| anyhow::anyhow!("invalid superseded IRI {}: {e}", input.superseded_iri))?;
    let info_record_class = state.resolve_class("InformationRecord")?;
    let superseded_class = state
        .store
        .quads_for_pattern(
            Some(old_subject.as_ref().into()),
            Some(NamedNodeRef::new_unchecked(moose::RDF_TYPE)),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::NamedNode(t) => Some(t.as_str().to_string()),
            _ => None,
        })
        .find(|t| is_subclass_of(&state.store, t, &info_record_class))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot supersede {}: not a recorded knowledge item (InformationRecord) in the project graph",
                input.superseded_iri
            )
        })?;
    let superseded_local = local_name(&superseded_class).to_string();

    // Resolve relation + class IRIs from the loaded ontology (by local name).
    let supersedes_pred = state.resolve_object_property("supersedes")?;
    let has_rationale_pred = state.resolve_object_property("hasRationale")?;
    let rationale_class = state.resolve_class("Rationale")?;

    let new_iri = mint_instance_iri(&superseded_local);
    let rationale_iri = mint_instance_iri("Rationale");
    let timestamp = when.to_rfc3339();

    // The Rationale node (the why): its description carries the reason; its title
    // is derived from the new decision's title so it reads well in listings.
    let new_title = input
        .new
        .properties
        .iter()
        .find(|(p, _)| p == &state.capture.title)
        .map(|(_, v)| v.as_str())
        .unwrap_or("decision");
    let rationale_title = format!("Rationale: {new_title}");
    let rationale_literals = vec![
        (moose::RDFS_LABEL.to_string(), rationale_title.clone()),
        (state.capture.title.clone(), rationale_title),
        (state.capture.description.clone(), input.rationale.clone()),
    ];
    // A superseding decision (and its rationale) is the now-current record, so
    // default the lifecycle status to "accepted".
    let stamp = CaptureStamp {
        capture: &state.capture,
        author,
        timestamp: &timestamp,
        status: "accepted",
    };
    let rationale_quads = capture_instance_quads(
        &rationale_iri,
        &rationale_class,
        &rationale_literals,
        &[],
        &stamp,
    )?;

    // The new decision: caller literals + edges to the rationale and the old one.
    // (The caller may still override status via `new.properties`.)
    let new_edges = vec![
        (has_rationale_pred, rationale_iri.clone()),
        (supersedes_pred, input.superseded_iri.clone()),
    ];
    let new_quads = capture_instance_quads(
        &new_iri,
        &superseded_class,
        &input.new.properties,
        &new_edges,
        &stamp,
    )?;

    // Flip the OLD decision's lifecycle status to "superseded": remove all its
    // existing status quads and assert the new one. Nothing else on the old
    // instance is touched — it remains as the historical record.
    let old_status_quads: Vec<Quad> = state
        .store
        .quads_for_pattern(
            Some(old_subject.as_ref().into()),
            Some(NamedNodeRef::new(&state.capture.status)?),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .collect();
    let superseded_status = Quad::new(
        old_subject.clone(),
        NamedNode::new(&state.capture.status)?,
        Literal::new_simple_literal("superseded"),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );

    // One atomic transaction: insert the new decision + rationale + the old's new
    // status, and remove the old's prior status quads.
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("supersede transaction: {e}"))?;
    txn.extend(rationale_quads.iter().map(Quad::as_ref));
    txn.extend(new_quads.iter().map(Quad::as_ref));
    for quad in &old_status_quads {
        txn.remove(quad.as_ref());
    }
    txn.insert(superseded_status.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("supersede commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok(SupersedeOutcome {
        new_iri,
        rationale_iri,
        superseded_iri: input.superseded_iri.clone(),
    })
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
        let stage = local_name(&st.stage_iri);
        let detail = st.detail.as_deref().unwrap_or("");
        lines.push(format!("  • {stage} ({:.1}ms) {detail}", st.duration_ms));
    }
    lines.join("\n")
}

/// Extract the local name of an IRI (after the last `/` or `#`).
fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#']).next().unwrap_or(iri)
}

/// A recorded knowledge item returned as structured context.
pub struct ContextItem {
    pub iri: String,
    pub kind: String,
    pub label: String,
    pub properties: Vec<(String, String)>,
}

impl ContextItem {
    /// True when this record has been retired from the current working set
    /// (lifecycle status `superseded` or `deprecated`).
    pub fn is_historical(&self) -> bool {
        self.properties.iter().any(|(k, v)| {
            k == "hasLifecycleStatus" && matches!(v.as_str(), "superseded" | "deprecated")
        })
    }
}

/// Retrieve recorded knowledge relevant to `topic` (label-matched via the
/// cache-coherent entity index), or list all recorded instances when `topic` is
/// empty. Symbolic — no LLM.
pub fn relevant_context(
    state: &AppState,
    topic: Option<&str>,
    limit: usize,
    include_history: bool,
) -> anyhow::Result<Vec<ContextItem>> {
    let class_iris: Vec<String> = state
        .arch_vocab
        .classes
        .iter()
        .map(|c| c.iri.clone())
        .collect();
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];

    let subjects: Vec<(String, String)> = match topic.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => state
            .entity_index
            .search_classes(
                t,
                &class_iris,
                &state.store,
                &data_graphs,
                moose::LABEL_PREDICATES,
                limit,
            )
            .into_iter()
            .map(|h| (h.iri, h.class_iri))
            .collect(),
        None => list_instances(&state.store, &class_iris, limit),
    };

    let mut items: Vec<ContextItem> = subjects
        .into_iter()
        .map(|(iri, class_iri)| build_context_item(state, iri, class_iri))
        .collect();

    // Default to the *current* working set: hide superseded/deprecated records
    // (history is one hop away — `include_history` lists them, and each current
    // item still surfaces its `supersedes` link + rationale). Filtering after the
    // fetch means a page can return fewer than `limit` items when history exists;
    // acceptable for v1's data volumes.
    if !include_history {
        items.retain(|item| !item.is_historical());
    }
    Ok(items)
}

/// List up to `limit` instances of the given classes in the project KG graph.
fn list_instances(store: &Store, class_iris: &[String], limit: usize) -> Vec<(String, String)> {
    let rdf_type = NamedNodeRef::new_unchecked(moose::RDF_TYPE);
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut out = Vec::new();
    for class_iri in class_iris {
        let Ok(class) = NamedNodeRef::new(class_iri) else {
            continue;
        };
        for q in store
            .quads_for_pattern(
                None,
                Some(rdf_type),
                Some(class.into()),
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
        {
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                out.push((s.as_str().to_string(), class_iri.clone()));
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// Fetch an instance's label, literal properties, and relations from the project
/// KG graph. Object-valued edges (e.g. `supersedes`, `hasRationale`) are surfaced
/// as `(local-name, target-IRI)` so the lifecycle chain is visible and walkable;
/// the linked `Rationale`'s text (the *why*) is dereferenced inline, and a
/// retired record also gets a `supersededBy` back-link to what replaced it.
fn build_context_item(state: &AppState, iri: String, class_iri: String) -> ContextItem {
    let store = &state.store;
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut label = String::new();
    let mut properties: Vec<(String, String)> = Vec::new();
    let mut rationale_iri: Option<String> = None;

    if let Ok(subject) = NamedNodeRef::new(&iri) {
        for q in store
            .quads_for_pattern(
                Some(subject.into()),
                None,
                None,
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
        {
            let pred = q.predicate.as_str();
            if pred == moose::RDF_TYPE {
                continue;
            }
            match &q.object {
                Term::Literal(lit) if pred == moose::RDFS_LABEL => {
                    label = lit.value().to_string();
                }
                Term::Literal(lit) => {
                    properties.push((local_name(pred).to_string(), lit.value().to_string()));
                }
                Term::NamedNode(obj) => {
                    let pname = local_name(pred);
                    if pname == "hasRationale" {
                        rationale_iri = Some(obj.as_str().to_string());
                    }
                    properties.push((pname.to_string(), obj.as_str().to_string()));
                }
                _ => {}
            }
        }
    }

    // Surface the rationale *text* (the why), not just the link to its node.
    if let Some(rat) = &rationale_iri {
        if let Some(text) = first_literal(store, rat, &state.capture.description) {
            properties.push(("rationale".to_string(), text));
        }
    }

    // For a retired record, surface what replaced it (inverse `supersedes`).
    let is_historical = properties.iter().any(|(k, v)| {
        k == "hasLifecycleStatus" && matches!(v.as_str(), "superseded" | "deprecated")
    });
    if is_historical {
        if let (Ok(subject), Ok(pred)) = (
            NamedNodeRef::new(&iri),
            state.resolve_object_property("supersedes"),
        ) {
            if let Ok(pred_ref) = NamedNodeRef::new(&pred) {
                for q in store
                    .quads_for_pattern(
                        None,
                        Some(pred_ref),
                        Some(subject.into()),
                        Some(GraphNameRef::NamedNode(graph)),
                    )
                    .flatten()
                {
                    if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                        properties.push(("supersededBy".to_string(), s.as_str().to_string()));
                    }
                }
            }
        }
    }

    ContextItem {
        iri,
        kind: local_name(&class_iri).to_string(),
        label,
        properties,
    }
}

/// First literal object of `(subject, predicate, *)` in the project graph, if any.
fn first_literal(store: &Store, subject_iri: &str, predicate_iri: &str) -> Option<String> {
    let subject = NamedNodeRef::new(subject_iri).ok()?;
    let predicate = NamedNodeRef::new(predicate_iri).ok()?;
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .find_map(|q| match q.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
}
