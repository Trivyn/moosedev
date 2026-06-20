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
use moose::chat::session_db::SessionDb;
use moose::embeddings::vec_store::VecStore;
use moose::entity_index::EntityIndexCache;
use moose::kg::{
    assert_instance, AssertionLiteral, DatatypeAssertion, InstanceAssertion, ObjectAssertion,
};
use moose::moose_ontology::MooseOntologyCache;
use moose::pipeline::execute_graph_walk_nlq_with_context;
use moose::traits::{ChatConfig, EngineConfig, LlmClient};
use moose::types::{
    CompactVocabulary, HybridConfig, LlmAssistLevel, PipelineTimings, VocabularyEntry, WalkBudgets,
};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
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
const CAPTURE_TITLE_LOCAL: &str = "hasTitle";
const CAPTURE_DESCRIPTION_LOCAL: &str = "hasDescription";
const CAPTURE_STATUS_LOCAL: &str = "hasLifecycleStatus";
const CAPTURE_AUTHOR_LOCAL: &str = "hasAuthor";
const CAPTURE_TIMESTAMP_LOCAL: &str = "hasTimestamp";
const LABEL_PROPERTY_LOCAL: &str = "labelProperty";
const DEFAULT_LIFECYCLE_STATUS: &str = "proposed";
const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

/// Open the persistent Oxigraph store under a MOOSEDev data directory.
pub fn open_store(data_dir: &Path) -> anyhow::Result<Store> {
    Store::open(data_dir.join("kg")).map_err(|e| anyhow::anyhow!("open persistent store: {e}"))
}

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
    /// Durable multi-turn MOOSE chat sessions, enabled by the shared backend for
    /// the human web UI.
    pub session_db: Option<Arc<SessionDb>>,
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
        let store = open_store(data_dir)?;
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
            session_db: None,
            vector_store: None,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// Enable MOOSE's real multi-turn chat layer for host surfaces that need it
    /// (the web UI). Kept separate from `bootstrap` so existing synchronous tests
    /// and MCP-only paths do not need an async constructor.
    pub async fn enable_chat_sessions(&mut self) -> anyhow::Result<()> {
        let db_path = self.data_dir.join("moose_sessions.db");
        let session_db_url = format!("sqlite://{}", db_path.display());
        // MOOSE session state is intentionally separate from the project KG:
        // transcripts/focus stacks are UI conversation state, while durable
        // decisions/lessons/constraints remain typed records in Oxigraph.
        let session_db = SessionDb::new(&session_db_url)
            .await
            .map_err(|e| anyhow::anyhow!("open MOOSE chat session DB: {e}"))?;
        session_db
            .migrate()
            .await
            .map_err(|e| anyhow::anyhow!("migrate MOOSE chat session DB: {e}"))?;

        self.engine_config.chat = Some(ChatConfig {
            session_db_url,
            session_ttl_days: 7,
            max_turns: 100,
            salience_decay: 0.7,
            focus_stack_max: 20,
            subgraph_quad_cap: 20_000,
            symbolic_coref_threshold: 0.5,
            // Teach MOOSE chat's slash-command/search layer which project-record
            // literals matter for memory recall. This mirrors get_relevant_context
            // without adding an A-box vector index.
            search_text_fields: Some(vec![
                (moose::RDFS_LABEL.to_string(), 2.0),
                (self.capture.description.clone(), 1.0),
            ]),
            clarification: Default::default(),
        });
        self.session_db = Some(Arc::new(session_db));
        Ok(())
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
    let literal_props = normalize_capture_literal_props(
        &state.store,
        &state.capture,
        &input.class_iri,
        &input.properties,
    );
    let mut datatype_props: Vec<DatatypeAssertion> = literal_props
        .iter()
        .map(|(predicate, value)| DatatypeAssertion {
            predicate_iri: predicate.as_str(),
            literal: AssertionLiteral::Simple(value.as_str()),
        })
        .collect();
    if !has_literal_property(&literal_props, &state.capture.author) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.author.as_str(),
            literal: AssertionLiteral::Simple(author),
        });
    }
    if !has_literal_property(&literal_props, &state.capture.timestamp) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.timestamp.as_str(),
            literal: AssertionLiteral::Typed {
                value: timestamp.as_str(),
                datatype_iri: XSD_DATETIME,
            },
        });
    }
    if !has_literal_property(&literal_props, &state.capture.status) {
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

/// Check whether the caller already supplied a property so write-path defaults do
/// not duplicate explicit values.
fn has_literal_property(literal_props: &[(String, String)], predicate_iri: &str) -> bool {
    literal_props
        .iter()
        .any(|(predicate, _)| predicate == predicate_iri)
}

/// Mirror the canonical `rdfs:label` value into the class-specific datatype
/// property identified by the ontology's `labelProperty` annotation. This keeps
/// retrieval label-driven while satisfying shapes such as
/// `SystemComponent.hasComponentName minCount 1`.
fn normalize_capture_literal_props(
    store: &Store,
    capture: &CapturePredicates,
    class_iri: &str,
    literal_props: &[(String, String)],
) -> Vec<(String, String)> {
    let label_mirror_property = class_label_mirror_property_iri(store, capture, class_iri);
    let title_value = literal_props
        .iter()
        .find(|(predicate, _)| predicate == &label_mirror_property)
        .or_else(|| {
            literal_props
                .iter()
                .find(|(predicate, _)| predicate == &capture.title)
        })
        .map(|(_, value)| value.clone());

    let mut out = Vec::with_capacity(literal_props.len() + 1);
    for (predicate, value) in literal_props {
        if predicate == &capture.title && label_mirror_property != capture.title {
            continue;
        }
        out.push((predicate.clone(), value.clone()));
    }

    if !has_literal_property(&out, &label_mirror_property) {
        if let Some(title) = title_value {
            out.push((label_mirror_property, title));
        }
    }

    out
}

/// Read the datatype property that mirrors `rdfs:label` for a class. If the
/// class has no direct annotation, preserve the existing `hasTitle` behavior.
fn class_label_mirror_property_iri(
    store: &Store,
    capture: &CapturePredicates,
    class_iri: &str,
) -> String {
    let Ok(class) = NamedNode::new(class_iri) else {
        return capture.title.clone();
    };
    store
        .quads_for_pattern(Some(class.as_ref().into()), None, None, None)
        .flatten()
        .find_map(|q| {
            if local_name(q.predicate.as_str()) != LABEL_PROPERTY_LOCAL {
                return None;
            }
            match q.object {
                Term::NamedNode(label_property) => Some(label_property.as_str().to_string()),
                _ => None,
            }
        })
        .unwrap_or_else(|| capture.title.clone())
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
    store: &Store,
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
    let literal_props = normalize_capture_literal_props(store, capture, class_iri, literal_props);

    let mut quads = vec![Quad::new(
        subject.clone(),
        NamedNode::new(moose::RDF_TYPE)?,
        NamedNode::new(class_iri)
            .map_err(|e| anyhow::anyhow!("invalid class IRI {class_iri}: {e}"))?,
        graph.clone(),
    )];
    for (predicate, value) in &literal_props {
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
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const SH_TARGET_CLASS: &str = "http://www.w3.org/ns/shacl#targetClass";
const SH_PROPERTY: &str = "http://www.w3.org/ns/shacl#property";
const SH_PATH: &str = "http://www.w3.org/ns/shacl#path";
const SH_CLASS: &str = "http://www.w3.org/ns/shacl#class";
const SH_OR: &str = "http://www.w3.org/ns/shacl#or";

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

/// Verify `subject` is a recorded knowledge item — an instance of
/// `:InformationRecord` (or a subclass) in the project graph — and return its
/// class IRI. The lifecycle tools (`supersede_decision`, `retract_decision`)
/// share this precondition so they never mutate a non-record subject, and the
/// returned class lets a supersede mint its replacement type-preservingly.
fn require_information_record(state: &AppState, subject: &NamedNode) -> anyhow::Result<String> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let info_record_class = state.resolve_class("InformationRecord")?;
    state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
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
                "{} is not a recorded knowledge item (InformationRecord) in the project graph",
                subject.as_str()
            )
        })
}

#[derive(Debug, Clone)]
struct RelationConstraint {
    subject_class: String,
    object_class: String,
}

/// Asserted rdf:type classes for a project-graph subject. No inference is
/// performed here; callers compare with `is_subclass_of` against ontology axioms.
fn asserted_project_types(state: &AppState, subject: &NamedNode) -> Vec<String> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(NamedNodeRef::new_unchecked(moose::RDF_TYPE)),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::NamedNode(t) => Some(t.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// Load the SHACL object constraints for a relationship predicate. The shape
/// target class is the subject/domain constraint; each property branch's
/// `sh:class` is the object/range constraint.
fn shacl_relation_constraints(state: &AppState, predicate_iri: &str) -> Vec<RelationConstraint> {
    let sparql = format!(
        r#"
SELECT DISTINCT ?subjectClass ?objectClass
WHERE {{
  VALUES ?shapeGraph {{ <{}> <{}> }}
  GRAPH ?shapeGraph {{
    ?shape <{}> ?subjectClass .
    {{
      ?shape <{}> ?propertyShape .
    }} UNION {{
      ?shape <{}>/<{}>*/<{}> ?propertyShape .
    }}
    ?propertyShape <{}> <{}> ;
                   <{}> ?objectClass .
  }}
}}"#,
        ontology::SE_SHAPES_GRAPH_IRI,
        ontology::ARCH_SHAPES_GRAPH_IRI,
        SH_TARGET_CLASS,
        SH_PROPERTY,
        SH_OR,
        RDF_REST,
        RDF_FIRST,
        SH_PATH,
        predicate_iri,
        SH_CLASS
    );

    let Ok(QueryResults::Solutions(solutions)) = run_sparql(&state.store, &sparql) else {
        return Vec::new();
    };
    solutions
        .flatten()
        .filter_map(|solution| {
            Some(RelationConstraint {
                subject_class: iri_value(solution.get("subjectClass"))?,
                object_class: iri_value(solution.get("objectClass"))?,
            })
        })
        .collect()
}

fn run_sparql<'a>(store: &'a Store, sparql: &str) -> anyhow::Result<QueryResults<'a>> {
    let prepared = SparqlEvaluator::new()
        .parse_query(sparql)
        .map_err(|e| anyhow::anyhow!("graph query parse failed: {e}\n{sparql}"))?;
    prepared
        .on_store(store)
        .execute()
        .map_err(|e| anyhow::anyhow!("graph query failed: {e}"))
}

fn iri_value(term: Option<&Term>) -> Option<String> {
    match term {
        Some(Term::NamedNode(node)) => Some(node.as_str().to_string()),
        _ => None,
    }
}

fn any_subclass_of(store: &Store, actual: &[String], expected: &[String]) -> bool {
    actual
        .iter()
        .any(|a| expected.iter().any(|e| is_subclass_of(store, a, e)))
}

fn class_list(classes: &[String]) -> String {
    if classes.is_empty() {
        "<none>".to_string()
    } else {
        classes
            .iter()
            .map(|iri| local_name(iri).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn unique_classes(classes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for class in classes {
        if !out.contains(&class) {
            out.push(class);
        }
    }
    out
}

/// Validate a relation against the loaded SHACL shape contract before writing.
/// If the predicate has no object constraint in the shapes, preserve the legacy
/// safe default: both endpoints must be InformationRecords.
fn validate_relation_endpoints(
    state: &AppState,
    subject: &NamedNode,
    predicate_iri: &str,
    object: &NamedNode,
) -> anyhow::Result<()> {
    let constraints = shacl_relation_constraints(state, predicate_iri);
    if constraints.is_empty() {
        require_information_record(state, subject)
            .map_err(|e| anyhow::anyhow!("cannot relate subject {}: {e}", subject.as_str()))?;
        require_information_record(state, object)
            .map_err(|e| anyhow::anyhow!("cannot relate object {}: {e}", object.as_str()))?;
        return Ok(());
    }

    let subject_types = asserted_project_types(state, subject);
    let object_types = asserted_project_types(state, object);
    let expected_subjects = unique_classes(
        constraints
            .iter()
            .map(|constraint| constraint.subject_class.clone()),
    );

    let matching_subject_constraints: Vec<&RelationConstraint> = constraints
        .iter()
        .filter(|constraint| {
            any_subclass_of(
                &state.store,
                &subject_types,
                std::slice::from_ref(&constraint.subject_class),
            )
        })
        .collect();

    if matching_subject_constraints.is_empty() {
        anyhow::bail!(
            "cannot relate subject {}: actual class(es) [{}], expected [{}]",
            subject.as_str(),
            class_list(&subject_types),
            class_list(&expected_subjects)
        );
    }

    let expected_objects = unique_classes(
        matching_subject_constraints
            .iter()
            .map(|constraint| constraint.object_class.clone()),
    );
    if !any_subclass_of(&state.store, &object_types, &expected_objects) {
        anyhow::bail!(
            "cannot relate object {}: actual class(es) [{}], expected [{}]",
            object.as_str(),
            class_list(&object_types),
            class_list(&expected_objects)
        );
    }

    Ok(())
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
    let superseded_class = require_information_record(state, &old_subject)
        .map_err(|e| anyhow::anyhow!("cannot supersede {}: {e}", input.superseded_iri))?;
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
        &state.store,
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
        &state.store,
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

/// IRIs affected by a retract: the record withdrawn and the `Rationale` minted.
pub struct RetractOutcome {
    pub retracted_iri: String,
    pub rationale_iri: String,
}

/// Retract a recorded knowledge item in place: flip its lifecycle status to
/// `deprecated` (so it drops out of the current working set, while the record and
/// all its other triples are preserved as history) and attach a `Rationale`
/// capturing *why* it was withdrawn. Unlike [`supersede_decision`], no replacement
/// is minted — this is the "this entry should no longer apply" transition (e.g. a
/// duplicate, or a decision abandoned without a successor). Atomic: the `Rationale`
/// node, the `hasRationale` edge, and the status change commit in one transaction;
/// the entity index is invalidated once on success. The subject must already be an
/// `InformationRecord` (or subclass) in the project graph — else this errors and
/// writes nothing.
pub fn retract_decision(
    state: &AppState,
    target_iri: &str,
    rationale: &str,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<RetractOutcome> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let subject = NamedNode::new(target_iri)
        .map_err(|e| anyhow::anyhow!("invalid target IRI {target_iri}: {e}"))?;

    // Precondition: only recorded knowledge items can be retracted (writes nothing
    // on failure, since this returns before the transaction).
    require_information_record(state, &subject)
        .map_err(|e| anyhow::anyhow!("cannot retract {target_iri}: {e}"))?;

    let has_rationale_pred = state.resolve_object_property("hasRationale")?;
    let rationale_class = state.resolve_class("Rationale")?;
    let rationale_iri = mint_instance_iri("Rationale");
    let timestamp = when.to_rfc3339();

    // Title the Rationale after the retracted record so it reads well in listings.
    let target_title = first_literal(&state.store, target_iri, &state.capture.title)
        .unwrap_or_else(|| "record".to_string());
    let rationale_title = format!("Rationale: retract {target_title}");
    let rationale_literals = vec![
        (moose::RDFS_LABEL.to_string(), rationale_title.clone()),
        (state.capture.title.clone(), rationale_title),
        (state.capture.description.clone(), rationale.to_string()),
    ];
    // The rationale is itself a current record.
    let stamp = CaptureStamp {
        capture: &state.capture,
        author,
        timestamp: &timestamp,
        status: "accepted",
    };
    let rationale_quads = capture_instance_quads(
        &state.store,
        &rationale_iri,
        &rationale_class,
        &rationale_literals,
        &[],
        &stamp,
    )?;

    // The hasRationale edge hangs off the retracted record itself — unlike a
    // supersede, there is no successor record to carry it.
    let rationale_edge = Quad::new(
        subject.clone(),
        NamedNode::new(&has_rationale_pred)?,
        NamedNode::new(&rationale_iri)?,
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );

    // Flip the target's lifecycle status to "deprecated": remove its existing
    // status quads and assert the new one. Nothing else on the record is touched.
    let old_status_quads: Vec<Quad> = state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(NamedNodeRef::new(&state.capture.status)?),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .collect();
    let deprecated_status = Quad::new(
        subject.clone(),
        NamedNode::new(&state.capture.status)?,
        Literal::new_simple_literal("deprecated"),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );

    // One atomic transaction: insert the rationale + its edge + the new status, and
    // remove the prior status quads.
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("retract transaction: {e}"))?;
    txn.extend(rationale_quads.iter().map(Quad::as_ref));
    txn.insert(rationale_edge.as_ref());
    for quad in &old_status_quads {
        txn.remove(quad.as_ref());
    }
    txn.insert(deprecated_status.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("retract commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok(RetractOutcome {
        retracted_iri: target_iri.to_string(),
        rationale_iri,
    })
}

/// The edge written by [`relate`]: subject, the resolved predicate IRI, object.
pub struct RelateOutcome {
    pub subject_iri: String,
    pub predicate_iri: String,
    pub object_iri: String,
}

/// Assert a typed relationship edge between two existing recorded knowledge items
/// — e.g. an `AntiPattern` `violates` a `Constraint`, or an `ArchitecturalDecision`
/// `isMotivatedBy` a `Requirement` / `concerns` a component. The predicate is an
/// object property resolved from the loaded ontology by local name (keeping the
/// volatile namespace out of the code and rejecting ad-hoc, untyped edges). Both
/// endpoints must already be `InformationRecord`s (or subclasses) in the project
/// graph — else this errors and writes nothing. Atomic and idempotent: one quad is
/// inserted in a transaction (re-asserting an existing edge is a no-op) and the
/// entity index is invalidated once on success. This is the primitive that turns
/// capture from a typed *list* into a traversable *graph*: the ontology already
/// declares these relations (`supersedes`, `violates`, `isMotivatedBy`, …), but
/// only `supersede_decision` ever wrote one before.
pub fn relate(
    state: &AppState,
    subject_iri: &str,
    predicate_local: &str,
    object_iri: &str,
) -> anyhow::Result<RelateOutcome> {
    let subject = NamedNode::new(subject_iri)
        .map_err(|e| anyhow::anyhow!("invalid subject IRI {subject_iri}: {e}"))?;
    let object = NamedNode::new(object_iri)
        .map_err(|e| anyhow::anyhow!("invalid object IRI {object_iri}: {e}"))?;

    // Resolve the relation IRI from the ontology by local name. Restricting to a
    // declared object property keeps the graph well-typed and the namespace out of
    // the code (decouple-code-from-ontology-ttl).
    let predicate_iri = state.resolve_object_property(predicate_local).map_err(|e| {
        anyhow::anyhow!(
            "unknown relationship {predicate_local:?} (not an object property in the architecture ontology): {e}"
        )
    })?;

    // Preconditions: endpoint classes must satisfy the predicate's SHACL shape
    // contract. Checked before the transaction, so a bad edge writes nothing.
    validate_relation_endpoints(state, &subject, &predicate_iri, &object)?;

    let edge = Quad::new(
        subject,
        NamedNode::new(&predicate_iri)?,
        object,
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("relate transaction: {e}"))?;
    txn.insert(edge.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("relate commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok(RelateOutcome {
        subject_iri: subject_iri.to_string(),
        predicate_iri,
        object_iri: object_iri.to_string(),
    })
}

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

/// Retrieve recorded knowledge relevant to `topic` — BM25 lexical relevance over
/// each record's title + description via moose `search_records` — or list all
/// recorded instances when `topic` is empty. Records sharing no query term are
/// excluded, so an empty result is reported honestly as "nothing relevant"
/// rather than padded with noise (invariant #6: be correct, don't sound correct).
/// Symbolic — no LLM.
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

    // BM25 relevance over each record's label (weighted) + description. We search rdfs:label —
    // every record carries it as its title — rather than hasTitle, so label-only records are still
    // found and the title text isn't double-counted. Scores are raw/corpus-relative, so we rank and
    // take top-k rather than applying an absolute floor; records sharing no query term are excluded
    // by search_records, preserving the honest empty state (invariant #6).
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    let subjects: Vec<(String, String)> = match topic.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => state
            .entity_index
            .search_records(
                t,
                &class_iris,
                &state.store,
                &data_graphs,
                &text_fields,
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

    // Bounded relational expansion (Constraint aa8b3fa3): for a focused topic, reach
    // the few linked records that COMPLETE an answer — the lexically-distant neighbor
    // BM25 alone misses — WITHOUT dumping the neighborhood (context efficiency is the
    // whole point, AD 7b824b26). Skipped for list-all and when MOOSEDEV_EXPAND_HOPS=0.
    let max_hops = std::env::var("MOOSEDEV_EXPAND_HOPS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(EXPAND_MAX_HOPS);
    if topic.map(str::trim).filter(|t| !t.is_empty()).is_some() && max_hops > 0 {
        let mut seen: std::collections::HashSet<String> =
            items.iter().map(|i| i.iri.clone()).collect();
        let mut expanded: Vec<ContextItem> = Vec::new();
        let mut frontier: Vec<String> = items
            .iter()
            .take(EXPAND_FROM_TOP)
            .map(|i| i.iri.clone())
            .collect();
        'hops: for _ in 0..max_hops {
            let mut next: Vec<String> = Vec::new();
            for src in &frontier {
                for (pred, neighbor_iri, neighbor_class) in record_neighbors(state, src) {
                    if !seen.insert(neighbor_iri.clone()) {
                        continue; // already a seed or already expanded
                    }
                    let mut item = build_context_item(state, neighbor_iri.clone(), neighbor_class);
                    if !include_history && item.is_historical() {
                        continue; // stay in the current working set
                    }
                    item.properties.insert(0, ("linkedVia".to_string(), pred));
                    expanded.push(item);
                    next.push(neighbor_iri);
                    if expanded.len() >= EXPAND_MAX {
                        break 'hops;
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        items.extend(expanded);
    }

    Ok(items)
}

/// Bounds for relational expansion in [`relevant_context`] (Constraint aa8b3fa3):
/// expansion is a budgeted reach to the few neighbors that complete an answer, never
/// a neighborhood dump — context efficiency is the whole point (AD 7b824b26).
const EXPAND_FROM_TOP: usize = 3; // expand only from the top-N most-relevant seed hits
const EXPAND_MAX_HOPS: usize = 2; // follow at most this many hops outward
const EXPAND_MAX: usize = 5; // hard cap on total appended (linked) records

/// Outbound edges from `iri` to other recorded knowledge items, as
/// `(predicate_local, neighbor_iri, neighbor_class)`. Only edges whose object is an
/// `InformationRecord` (or subclass) are returned — excluding the `prov:*` metadata
/// firehose and literal properties — and `hasRationale` is skipped because its text
/// is already inlined by [`build_context_item`].
fn record_neighbors(state: &AppState, iri: &str) -> Vec<(String, String, String)> {
    let Ok(subject) = NamedNodeRef::new(iri) else {
        return Vec::new();
    };
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut out = Vec::new();
    for q in state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
    {
        if q.predicate.as_str() == moose::RDF_TYPE {
            continue;
        }
        let pred_local = local_name(q.predicate.as_str()).to_string();
        if pred_local == "hasRationale" {
            continue; // its text is already inlined by build_context_item
        }
        if let Term::NamedNode(obj) = &q.object {
            if let Ok(class) = require_information_record(state, obj) {
                out.push((pred_local, obj.as_str().to_string(), class));
            }
        }
    }
    out
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
