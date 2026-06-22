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
use moose::embeddings::{
    default_backbone, embed_and_index_instance, retrieval_embed_query, InstanceVecStore,
};
use moose::entity_index::{EntityIndexCache, DEFAULT_DENSE_FLOOR};
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
    /// Instance (ABox) dense vector index — the dense seed channel for
    /// `get_relevant_context`. A durable, model-stamped store opened by
    /// `build_instance_index`, reconciled incrementally at startup (only records
    /// not already persisted are embedded) and kept coherent on write by
    /// `index_record`. Bootstrap installs an empty ephemeral placeholder (so the
    /// hybrid seed soft-falls to pure BM25) until the durable store is opened.
    pub instance_store: Arc<InstanceVecStore>,
    pub moose_cache: Arc<MooseOntologyCache>,
    pub arch_vocab: CompactVocabulary,
    pub capture: CapturePredicates,
    /// Object-property domain/range table from the SHACL shapes, built once at
    /// bootstrap. The legality source for relation writes (`relate` + inline
    /// capture) and candidate enumeration for the link-suggester.
    pub catalogue: RelationCatalogue,
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
    /// Set true by any write that changes the project graph; drained by
    /// [`AppState::ensure_enriched`] before a read, so GROWL re-materializes the
    /// inferred inverse/subproperty edges lazily — one pass per capture burst.
    pub inferred_stale: std::sync::atomic::AtomicBool,
    /// Serializes enrichment so concurrent reads enrich at most once.
    enrich_lock: std::sync::Mutex<()>,
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
        let catalogue = build_relation_catalogue(&store);
        let entity_index = Arc::new(EntityIndexCache::new(64));

        let llm_cfg = LlmConfig::from_env();
        let llm = OpenAiCompatClient::new(llm_cfg.base_url, llm_cfg.api_key);
        let ontology_resolver = MooseDevOntologyResolver::new();

        let engine_config = EngineConfig {
            context_budget: 8_192,
            budgets: WalkBudgets::default(),
            hybrid: HybridConfig::default(),
            // Label-designator contract: take Core's default trip (>80 chars / >8
            // words / sentence-break = content → demoted out of name-resolution and
            // BM25F boost). Matches the ≤80-char handle convention used for capture.
            label_shape: Default::default(),
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
            // Empty ephemeral placeholder (hybrid seed soft-falls to BM25) until
            // `build_instance_index` swaps in the durable store; `index_record`
            // keeps that store coherent thereafter.
            instance_store: Arc::new(InstanceVecStore::ephemeral()),
            moose_cache,
            arch_vocab,
            capture,
            catalogue,
            engine_config,
            llm,
            ontology_resolver,
            model: llm_cfg.model,
            session_db: None,
            vector_store: None,
            data_dir: data_dir.to_path_buf(),
            // Start stale so the first read after startup materializes inferred edges.
            inferred_stale: std::sync::atomic::AtomicBool::new(true),
            enrich_lock: std::sync::Mutex::new(()),
        })
    }

    /// Mark the reasoner-materialized edges stale — call after any write that changes the
    /// project graph, so the next read re-enriches.
    pub fn mark_inferred_stale(&self) {
        self.inferred_stale
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Lazily re-run GROWL enrichment when a prior write invalidated the materialized
    /// inverse/subproperty edges, so a read traverses fresh edges. Best-effort: a reasoner
    /// failure is logged and leaves the flag set (retried next read) rather than failing
    /// the read. Serialized by `enrich_lock` so concurrent reads enrich at most once.
    pub fn ensure_enriched(&self) {
        use std::sync::atomic::Ordering;
        if !self.inferred_stale.load(Ordering::Acquire) {
            return; // fast path — nothing changed since the last enrichment
        }
        let _guard = self
            .enrich_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.inferred_stale.load(Ordering::Acquire) {
            return; // another reader enriched while we waited on the lock
        }
        match crate::reasoning::enrich_now(
            &self.store,
            PROJECT_KG_GRAPH_IRI,
            &[
                ontology::SE_DOMAIN_GRAPH_IRI,
                ontology::ARCH_DOMAIN_GRAPH_IRI,
            ],
        ) {
            Ok(n) => {
                self.inferred_stale.store(false, Ordering::Release);
                tracing::debug!("enrich: materialized {n} inferred edge(s)");
            }
            Err(e) => tracing::warn!("enrich failed (serving possibly-stale edges): {e}"),
        }
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

    /// Embed one record's text-bearing literals (label + description) into the
    /// instance vector store so the dense channel of `get_relevant_context` stays
    /// coherent with the write. Reads the record's class and text straight from the
    /// store, so every caller (startup backfill, capture, supersede, retract) needs
    /// only the IRI. Best-effort and idempotent: `upsert` overwrites any prior
    /// vector, and a record with no text — or an unavailable embedding backbone — is
    /// a no-op (`Ok(false)`), never an error, so the symbolic write stays primary
    /// (invariant #1). The label+description pair mirrors the BM25 `text_fields`, so
    /// the lexical and dense channels see the same document text. `hasTitle` is
    /// skipped because it duplicates `rdfs:label`.
    pub async fn index_record(&self, iri: &str) -> anyhow::Result<bool> {
        let Ok(subject) = NamedNode::new(iri) else {
            return Ok(false);
        };
        let Ok(class_iri) = require_information_record(self, &subject) else {
            return Ok(false); // not a typed record — nothing to index
        };
        let label = first_literal(&self.store, iri, moose::RDFS_LABEL);
        let description = first_literal(&self.store, iri, &self.capture.description);
        let mut props: Vec<DatatypeAssertion> = Vec::new();
        if let Some(value) = label.as_deref() {
            props.push(DatatypeAssertion {
                predicate_iri: moose::RDFS_LABEL,
                literal: AssertionLiteral::Simple(value),
            });
        }
        if let Some(value) = description.as_deref() {
            props.push(DatatypeAssertion {
                predicate_iri: self.capture.description.as_str(),
                literal: AssertionLiteral::Simple(value),
            });
        }
        let text_preds = [moose::RDFS_LABEL, self.capture.description.as_str()];
        embed_and_index_instance(&self.instance_store, iri, &class_iri, &props, &text_preds)
            .await
            .map_err(|e| anyhow::anyhow!("embed_and_index_instance({iri}): {e}"))
    }

    /// Open the durable instance (ABox) dense index — the dense seed channel for
    /// `get_relevant_context` — and reconcile it incrementally against the project
    /// KG. Mirrors [`Self::build_alignment_index`] (the TBox/ontology tier) and is
    /// likewise non-fatal: with no embedding backbone the store stays empty and the
    /// hybrid seed soft-falls to pure BM25.
    ///
    /// The store persists at `instance-vectors.db`, so a warm restart loads the
    /// vectors from disk and [`Self::sync_instance_index`] embeds only records not
    /// already present — startup cost is proportional to churn, not graph size
    /// (write-time [`Self::index_record`] keeps the store coherent in between). A
    /// model-stamp mismatch or corrupt store is rebuilt from scratch, mirroring the
    /// ontology store's reuse-or-rebuild contract. Returns the number of records
    /// embedded this pass (0 on a fully warm restart).
    pub async fn build_instance_index(&mut self) -> anyhow::Result<usize> {
        let db_path = self.data_dir.join("instance-vectors.db");
        let store = match InstanceVecStore::open(&db_path).await {
            Ok(store) => store,
            Err(e) => {
                // Stamp mismatch (the embedding model changed) or a corrupt store:
                // discard and rebuild fresh rather than fail the server start.
                tracing::warn!(
                    "[instance-vectors] reopening {} failed ({e}); rebuilding fresh",
                    db_path.display()
                );
                let _ = std::fs::remove_file(&db_path);
                InstanceVecStore::open(&db_path).await?
            }
        };
        self.instance_store = Arc::new(store);
        self.sync_instance_index().await
    }

    /// Embed every project record not already present in the (durable) instance
    /// store, in one batched document-side pass. Incremental by construction: a
    /// store warmed by a prior run leaves nothing to do. Returns the count embedded
    /// this pass. Soft-fails to a no-op when no embedding backbone is available.
    async fn sync_instance_index(&self) -> anyhow::Result<usize> {
        let class_iris: Vec<String> = self
            .arch_vocab
            .classes
            .iter()
            .map(|c| c.iri.clone())
            .collect();
        let records = list_instances(&self.store, &class_iris, usize::MAX);

        // The delta: records with no stored vector yet, paired with their document
        // text (label + description — the same text `index_record` embeds on write).
        let mut pending: Vec<(String, String, String)> = Vec::new();
        for (iri, class_iri) in &records {
            if self.instance_store.contains(iri) {
                continue;
            }
            let text = self.record_embed_text(iri);
            if !text.trim().is_empty() {
                pending.push((iri.clone(), class_iri.clone(), text));
            }
        }
        if pending.is_empty() {
            tracing::info!(
                "[instance-vectors] index warm ({} records, 0 to embed)",
                records.len()
            );
            return Ok(0);
        }

        // Batched document embedding — the matched counterpart to the query side,
        // identical to `retrieval_embed_document` per record but amortized for the
        // one-time cold build. No backbone → soft-fall to lexical-only seeding.
        let backbone = match default_backbone() {
            Ok(backbone) => backbone,
            Err(e) => {
                tracing::warn!(
                    "[instance-vectors] no embedding backbone ({e}); dense seed disabled"
                );
                return Ok(0);
            }
        };
        let texts: Vec<&str> = pending.iter().map(|(_, _, text)| text.as_str()).collect();
        let embeddings = backbone
            .embed_documents_batch(&texts)
            .map_err(|e| anyhow::anyhow!("batch-embed {} instances: {e}", texts.len()))?;

        let mut indexed = 0usize;
        for ((iri, class_iri, _), embedding) in pending.iter().zip(embeddings.iter()) {
            if let Err(e) = self.instance_store.upsert(iri, class_iri, embedding).await {
                tracing::warn!("[instance-vectors] upsert {iri}: {e}");
                continue;
            }
            indexed += 1;
        }
        tracing::info!(
            "[instance-vectors] embedded {indexed} new of {} project records",
            records.len()
        );
        Ok(indexed)
    }

    /// The document text indexed for one record: its label then description, joined
    /// like core's `gather_text`, so the batched build and per-record
    /// [`Self::index_record`] produce identical vectors.
    fn record_embed_text(&self, iri: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(label) = first_literal(&self.store, iri, moose::RDFS_LABEL) {
            if !label.is_empty() {
                parts.push(label);
            }
        }
        if let Some(description) = first_literal(&self.store, iri, &self.capture.description) {
            if !description.is_empty() {
                parts.push(description);
            }
        }
        parts.join("\n")
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

/// A forward relation written by [`record_instance_with_relation_args`].
#[derive(Debug, Clone)]
pub struct AppliedEdge {
    pub predicate_local: String,
    pub object_iri: String,
}

/// Result of a capture that may also assert inline relations.
#[derive(Debug, Clone)]
pub struct RecordOutcome {
    pub iri: String,
    pub applied_edges: Vec<AppliedEdge>,
}

/// Like [`record_instance`], but also asserts forward inline relations from the new
/// record (subject = the record being created). Each `(predicate_local, target)`
/// is resolved and SHACL-validated against the new record's class *before* any
/// write, so an invalid relation fails the whole capture — no orphan record is left
/// behind (validation precedes the single `assert_instance`). `target` is an
/// existing record IRI or its exact (normalized) title; an ambiguous title is
/// rejected (Req 5565038e). Identical `(predicate, object)` pairs are deduped.
pub fn record_instance_with_relation_args(
    state: &AppState,
    input: &RecordInput,
    relations: &[(String, String)],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<RecordOutcome> {
    let subject_types = std::slice::from_ref(&input.class_iri);
    let mut object_props: Vec<(String, String)> = Vec::new();
    let mut applied_edges: Vec<AppliedEdge> = Vec::new();

    for (predicate_local, target) in relations {
        let predicate_iri = state.resolve_object_property(predicate_local).map_err(|e| {
            anyhow::anyhow!(
                "unknown relationship {predicate_local:?} (not an object property in the architecture ontology): {e}"
            )
        })?;
        let object_iri = resolve_relation_target(state, target)?;
        let object = NamedNode::new(&object_iri)
            .map_err(|e| anyhow::anyhow!("invalid target IRI {object_iri:?}: {e}"))?;
        validate_relation_for_subject_types(
            state,
            subject_types,
            &input.class_local,
            &predicate_iri,
            &object,
        )?;
        if object_props
            .iter()
            .any(|(p, o)| p == &predicate_iri && o == &object_iri)
        {
            continue; // dedup identical (predicate, object) pairs
        }
        applied_edges.push(AppliedEdge {
            predicate_local: predicate_local.clone(),
            object_iri: object_iri.clone(),
        });
        object_props.push((predicate_iri, object_iri));
    }

    let iri = record_instance_with_relations(state, input, &object_props, author, when)?;
    Ok(RecordOutcome { iri, applied_edges })
}

/// Resolve an inline-relation target to an existing record IRI. Accepts an exact
/// project record IRI, or an exact (normalized) title — rejecting "not found" and
/// "ambiguous title" so a typo or a duplicate title can't silently mislink.
fn resolve_relation_target(state: &AppState, target: &str) -> anyhow::Result<String> {
    // An exact IRI of an existing record wins (titles with spaces fail IRI parse
    // and fall through to title resolution).
    if let Ok(node) = NamedNode::new(target) {
        if require_information_record(state, &node).is_ok() {
            return Ok(target.to_string());
        }
    }
    let matches = resolve_record_exact_all(state, target);
    match matches.len() {
        0 => anyhow::bail!(
            "relation target {target:?} matches no recorded item (by IRI or exact title)"
        ),
        1 => Ok(matches.into_iter().next().unwrap().0),
        n => anyhow::bail!(
            "relation target {target:?} is ambiguous — {n} records share that title; pass the IRI instead"
        ),
    }
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

/// A single SHACL-declared object-property constraint: `predicate_iri` admits a
/// subject of `subject_class` (the shape's `sh:targetClass`, the domain) pointing
/// at an object of `object_class` (the property branch's `sh:class`, the range).
#[derive(Debug, Clone)]
struct CatalogEntry {
    predicate_iri: String,
    predicate_local: String,
    subject_class: String,
    object_class: String,
}

/// Direction a legal edge must run for an ordered class pair `(a, b)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDirection {
    /// `a --predicate--> b` (a is the domain/subject).
    Forward,
    /// `b --predicate--> a` (b is the domain/subject).
    Inverse,
}

/// An object property legal between a class pair, with the direction it runs.
#[derive(Debug, Clone)]
pub struct LegalEdge {
    pub predicate_local: String,
    pub predicate_iri: String,
    pub direction: EdgeDirection,
}

/// The project's object-property domain/range table, read once from the loaded
/// SHACL shape graphs at bootstrap (shapes are static post-load, so it never needs
/// invalidation). The single in-memory source of truth for relation legality:
/// `relate` and inline capture validate against it, and the link-suggester
/// enumerates candidate predicates from it — replacing per-call SPARQL.
#[derive(Debug, Clone, Default)]
pub struct RelationCatalogue {
    entries: Vec<CatalogEntry>,
}

impl RelationCatalogue {
    /// Domain/range constraints declared for one predicate IRI — the subject/object
    /// class pairs its shapes permit. Replaces the old per-call shapes query.
    fn constraints_for_predicate(&self, predicate_iri: &str) -> Vec<RelationConstraint> {
        self.entries
            .iter()
            .filter(|e| e.predicate_iri == predicate_iri)
            .map(|e| RelationConstraint {
                subject_class: e.subject_class.clone(),
                object_class: e.object_class.clone(),
            })
            .collect()
    }

    /// Object properties legal between two record classes, in either direction,
    /// subclass-aware (so `InformationRecord`-level constraints apply to every
    /// pair). `Forward` ⇒ the edge is `a --pred--> b`; `Inverse` ⇒ `b --pred--> a`.
    /// Deduplicated by (predicate, direction).
    pub fn legal_predicates(&self, store: &Store, a_class: &str, b_class: &str) -> Vec<LegalEdge> {
        let mut out: Vec<LegalEdge> = Vec::new();
        for entry in &self.entries {
            let forward = is_subclass_of(store, a_class, &entry.subject_class)
                && is_subclass_of(store, b_class, &entry.object_class);
            let inverse = is_subclass_of(store, b_class, &entry.subject_class)
                && is_subclass_of(store, a_class, &entry.object_class);
            for (matched, direction) in [
                (forward, EdgeDirection::Forward),
                (inverse, EdgeDirection::Inverse),
            ] {
                if matched
                    && !out
                        .iter()
                        .any(|e| e.predicate_iri == entry.predicate_iri && e.direction == direction)
                {
                    out.push(LegalEdge {
                        predicate_local: entry.predicate_local.clone(),
                        predicate_iri: entry.predicate_iri.clone(),
                        direction,
                    });
                }
            }
        }
        out
    }

    /// Local names of every object property in the catalogue (for drift checks).
    #[cfg(test)]
    fn predicate_locals(&self) -> std::collections::HashSet<&str> {
        self.entries.iter().map(|e| e.predicate_local.as_str()).collect()
    }
}

/// Read every object-property constraint from the loaded SHACL shape graphs into a
/// [`RelationCatalogue`]. Generalizes the old per-predicate query: it binds
/// `?predicate` instead of fixing one, and keeps only property branches declaring
/// an `sh:class` (range) — `sh:datatype` branches drop out — so the table is
/// exactly the record→record object-property vocabulary, including `sh:or` union
/// ranges (e.g. `isMotivatedBy` → Constraint|Requirement).
fn build_relation_catalogue(store: &Store) -> RelationCatalogue {
    let sparql = format!(
        r#"
SELECT DISTINCT ?predicate ?subjectClass ?objectClass
WHERE {{
  VALUES ?shapeGraph {{ <{}> <{}> }}
  GRAPH ?shapeGraph {{
    ?shape <{}> ?subjectClass .
    {{
      ?shape <{}> ?propertyShape .
    }} UNION {{
      ?shape <{}>/<{}>*/<{}> ?propertyShape .
    }}
    ?propertyShape <{}> ?predicate ;
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
        SH_CLASS
    );

    let Ok(QueryResults::Solutions(solutions)) = run_sparql(store, &sparql) else {
        return RelationCatalogue::default();
    };
    let entries = solutions
        .flatten()
        .filter_map(|solution| {
            let predicate_iri = iri_value(solution.get("predicate"))?;
            let predicate_local = local_name(&predicate_iri).to_string();
            Some(CatalogEntry {
                predicate_iri,
                predicate_local,
                subject_class: iri_value(solution.get("subjectClass"))?,
                object_class: iri_value(solution.get("objectClass"))?,
            })
        })
        .collect();
    RelationCatalogue { entries }
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

/// Validate a relation against the loaded SHACL shape contract before writing, for
/// an *existing* subject record. Thin wrapper over
/// [`validate_relation_for_subject_types`] that reads the subject's asserted types.
fn validate_relation_endpoints(
    state: &AppState,
    subject: &NamedNode,
    predicate_iri: &str,
    object: &NamedNode,
) -> anyhow::Result<()> {
    let subject_types = asserted_project_types(state, subject);
    validate_relation_for_subject_types(
        state,
        &subject_types,
        subject.as_str(),
        predicate_iri,
        object,
    )
}

/// Validate a relation against the SHACL contract using *supplied* subject types,
/// so it works before the subject is minted (inline capture) as well as for an
/// existing subject (`relate`). `subject_desc` labels the subject in error
/// messages. The object must already exist in the project graph. If the predicate
/// has no object constraint in the shapes, preserve the legacy safe default: the
/// subject's types must include an `InformationRecord` and the object must be one.
fn validate_relation_for_subject_types(
    state: &AppState,
    subject_types: &[String],
    subject_desc: &str,
    predicate_iri: &str,
    object: &NamedNode,
) -> anyhow::Result<()> {
    let constraints = state.catalogue.constraints_for_predicate(predicate_iri);
    if constraints.is_empty() {
        let info_record = state.resolve_class("InformationRecord")?;
        if !subject_types
            .iter()
            .any(|t| is_subclass_of(&state.store, t, &info_record))
        {
            anyhow::bail!(
                "cannot relate subject {subject_desc}: actual class(es) [{}], expected [InformationRecord]",
                class_list(subject_types)
            );
        }
        require_information_record(state, object)
            .map_err(|e| anyhow::anyhow!("cannot relate object {}: {e}", object.as_str()))?;
        return Ok(());
    }

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
                subject_types,
                std::slice::from_ref(&constraint.subject_class),
            )
        })
        .collect();

    if matching_subject_constraints.is_empty() {
        anyhow::bail!(
            "cannot relate subject {subject_desc}: actual class(es) [{}], expected [{}]",
            class_list(subject_types),
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
    // Materialize inferred edges if a write invalidated them, so the typed expansion
    // traverses fresh inverse/subproperty links (bidirectional walk).
    state.ensure_enriched();
    let class_iris: Vec<String> = state
        .arch_vocab
        .classes
        .iter()
        .map(|c| c.iri.clone())
        .collect();
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];

    // Document text for both retrieval channels: rdfs:label (weighted) + description.
    // We search rdfs:label — every record carries it as its title — rather than hasTitle,
    // so label-only records are still found and the title text isn't double-counted. The
    // same two fields feed the dense document embedding (see `AppState::index_record`), so
    // the lexical and dense channels score the same text.
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    // A focused topic, trimmed to None when blank — drives both the seed and the
    // (topic-only) relational expansion below.
    let query = topic.map(str::trim).filter(|t| !t.is_empty());
    let subjects: Vec<(String, String)> = match query {
        Some(t) => {
            // Hybrid BM25F ⊕ dense seed: the dense channel surfaces records whose
            // meaning matches `t` with no shared term (paraphrase / vocabulary
            // mismatch) — the lexical blind spot that otherwise gates the whole
            // expansion. The confidence floor preserves the honest empty state
            // (invariant #6): an irrelevant query still seeds nothing. Soft-falls to
            // pure BM25 when the instance index is empty or the backbone is absent.
            let mut hits: Vec<(String, String)> = state
                .entity_index
                .search_records_hybrid(
                    t,
                    &class_iris,
                    &state.store,
                    &data_graphs,
                    &text_fields,
                    limit,
                    &state.instance_store,
                    dense_floor(),
                )
                .into_iter()
                .map(|h| (h.iri, h.class_iri))
                .collect();
            // Symbolic-first anchoring (invariant #1): when `t` names an existing
            // record by an exact label/title match, seed it FIRST — ahead of the
            // lexical+dense ranking — so the named record is guaranteed to expand.
            // Free-text topics that name no record fall through to the hybrid seed.
            if let Some(anchor) = resolve_topic_to_record(state, t) {
                hits.retain(|(iri, _)| iri != &anchor.0);
                hits.insert(0, anchor);
                hits.truncate(limit);
            }
            hits
        }
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
    // Candidate neighbors are RANKED (typed-edge priority, then dense topic-similarity)
    // before the EXPAND_MAX budget is spent, so the links that survive the cap are the
    // answer-completing ones rather than whatever order the store happened to yield.
    let max_hops = std::env::var("MOOSEDEV_EXPAND_HOPS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(EXPAND_MAX_HOPS);
    if let Some(t) = query.filter(|_| max_hops > 0) {
        // One query embedding for the whole expansion, reused to rank each hop's
        // neighbors by topic similarity. `None` (no backbone) → ranking falls back to
        // typed-edge priority then IRI; seeding was already lexical-only in that case.
        let query_emb = retrieval_embed_query(t).ok();
        let mut seen: std::collections::HashSet<String> =
            items.iter().map(|i| i.iri.clone()).collect();
        let mut expanded: Vec<ContextItem> = Vec::new();
        let mut frontier: Vec<String> = items
            .iter()
            .take(EXPAND_FROM_TOP)
            .map(|i| i.iri.clone())
            .collect();
        for _ in 0..max_hops {
            // Gather this hop's fresh neighbors across the whole frontier, deduped,
            // then rank the pool together so the budget is spent on the best — not on
            // whichever source/edge happened to come first in store order.
            let mut candidates: Vec<(String, String, String)> = Vec::new();
            let mut pooled: std::collections::HashSet<String> = std::collections::HashSet::new();
            for src in &frontier {
                for (pred, neighbor_iri, neighbor_class) in record_neighbors(state, src) {
                    if seen.contains(&neighbor_iri) || !pooled.insert(neighbor_iri.clone()) {
                        continue; // already a seed/expanded, or already pooled this hop
                    }
                    candidates.push((pred, neighbor_iri, neighbor_class));
                }
            }
            if candidates.is_empty() {
                break;
            }
            rank_neighbors(state, query_emb.as_deref(), &mut candidates);

            let mut next: Vec<String> = Vec::new();
            let mut budget_spent = false;
            for (pred, neighbor_iri, neighbor_class) in candidates {
                if !seen.insert(neighbor_iri.clone()) {
                    continue; // ranked pool is deduped, but keep `seen` authoritative
                }
                let mut item = build_context_item(state, neighbor_iri.clone(), neighbor_class);
                if !include_history && item.is_historical() {
                    continue; // stay in the current working set
                }
                item.properties.insert(0, ("linkedVia".to_string(), pred));
                expanded.push(item);
                next.push(neighbor_iri);
                if expanded.len() >= EXPAND_MAX {
                    budget_spent = true;
                    break;
                }
            }
            if budget_spent || next.is_empty() {
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
/// Confidence floor for the dense channel of the hybrid seed, from
/// `MOOSEDEV_DENSE_FLOOR` (an absolute cosine), defaulting to core's
/// [`DEFAULT_DENSE_FLOOR`]. The floor preserves the honest empty state (invariant
/// #6): cosine has no natural zero, so without it RRF would always promote *some*
/// nearest neighbor and manufacture a seed for an irrelevant query. Mirrors the
/// `MOOSEDEV_EXPAND_HOPS` override pattern. Always `Some` — config never disables
/// the guarantee (an unparseable value falls back to the default).
pub fn dense_floor() -> Option<f32> {
    let floor = std::env::var("MOOSEDEV_DENSE_FLOOR")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .unwrap_or(DEFAULT_DENSE_FLOOR);
    Some(floor)
}

/// Symbolic-first anchor: resolve a free-text `topic` to an existing record by an
/// exact (normalized) match on its `rdfs:label` or `hasTitle`, returning the
/// record's `(iri, class_iri)`. Lets [`relevant_context`] seed a *named* record as
/// the top anchor before lexical+dense ranking (invariant #1 — the symbolic layer
/// is primary; dense is the open-vocabulary fallback). Returns `None` for a topic
/// that names no record. Alias (`skos:altLabel`) anchoring is a later refinement —
/// records carry none today.
fn resolve_topic_to_record(state: &AppState, topic: &str) -> Option<(String, String)> {
    let needle = normalize_match(topic);
    if needle.is_empty() {
        return None;
    }
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    for pred_iri in [moose::RDFS_LABEL, state.capture.title.as_str()] {
        let Ok(pred) = NamedNodeRef::new(pred_iri) else {
            continue;
        };
        for q in state
            .store
            .quads_for_pattern(None, Some(pred), None, Some(GraphNameRef::NamedNode(graph)))
            .flatten()
        {
            let Term::Literal(lit) = &q.object else {
                continue;
            };
            if normalize_match(lit.value()) != needle {
                continue;
            }
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                if let Ok(class) = require_information_record(state, s) {
                    return Some((s.as_str().to_string(), class));
                }
            }
        }
    }
    None
}

/// Resolve a free-text target to recorded project items by an exact (normalized)
/// match on `rdfs:label` or `hasTitle`, returning *all* distinct matches as
/// `(iri, class)`. The many-match analogue of [`resolve_topic_to_record`], so a
/// caller can tell "not found" (empty) from "ambiguous" (>1). Deduped by IRI (a
/// record matches on both its label and its title).
fn resolve_record_exact_all(state: &AppState, target: &str) -> Vec<(String, String)> {
    let needle = normalize_match(target);
    if needle.is_empty() {
        return Vec::new();
    }
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut out: Vec<(String, String)> = Vec::new();
    for pred_iri in [moose::RDFS_LABEL, state.capture.title.as_str()] {
        let Ok(pred) = NamedNodeRef::new(pred_iri) else {
            continue;
        };
        for q in state
            .store
            .quads_for_pattern(None, Some(pred), None, Some(GraphNameRef::NamedNode(graph)))
            .flatten()
        {
            let Term::Literal(lit) = &q.object else {
                continue;
            };
            if normalize_match(lit.value()) != needle {
                continue;
            }
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                let iri = s.as_str().to_string();
                if out.iter().any(|(existing, _)| existing == &iri) {
                    continue;
                }
                if let Ok(class) = require_information_record(state, s) {
                    out.push((iri, class));
                }
            }
        }
    }
    out
}

/// Lightweight normalization for exact-ish anchor matching: collapse whitespace and
/// lowercase. Deliberately simpler than MOOSE's entity normalizer — anchoring wants
/// a high-precision exact match, not fuzzy recall (that is the dense channel's job).
fn normalize_match(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Rank candidate expansion neighbors in place before the `EXPAND_MAX` budget is
/// spent, so the links that survive the cap are the answer-completing ones rather
/// than whatever order `record_neighbors` yielded. Primary key: a typed-edge
/// priority tier ([`edge_priority`]); secondary: dense similarity of the neighbor to
/// the query topic (`None` query embedding → all 0.0, so ranking is edge-tier then
/// IRI); final tie-break: IRI, for determinism.
fn rank_neighbors(
    state: &AppState,
    query_emb: Option<&[f32]>,
    candidates: &mut [(String, String, String)],
) {
    let sims: std::collections::HashMap<String, f32> = match query_emb {
        Some(q) => {
            let iris: Vec<&str> = candidates.iter().map(|(_, iri, _)| iri.as_str()).collect();
            state
                .instance_store
                .score_candidates(q, &iris, None)
                .map(|scores| scores.into_iter().map(|s| (s.iri, s.cosine)).collect())
                .unwrap_or_default()
        }
        None => std::collections::HashMap::new(),
    };
    candidates.sort_by(|a, b| {
        let sa = sims.get(&a.1).copied().unwrap_or(0.0);
        let sb = sims.get(&b.1).copied().unwrap_or(0.0);
        edge_priority(&a.0)
            .cmp(&edge_priority(&b.0))
            .then(sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.1.cmp(&b.1))
    });
}

/// Object-property local names whose edges rank highest for graph-walk expansion —
/// the *why* and *what it touches* of a decision/constraint/lesson outrank
/// structural/containment edges. Host-side domain policy, kept out of MOOSE core
/// (which stays domain-neutral, invariant #11). Every name here is an object
/// property declared in the architecture/engineering SHACL shapes; the
/// `priority_edges_are_all_in_catalogue` test asserts each appears in the
/// [`RelationCatalogue`], so an ontology rename can't silently break ranking.
const PRIORITY_EDGES: &[&str] = &[
    "isMotivatedBy",
    "violates",
    "supersedes",
    "constrains",
    "concerns",
    "learnedFrom",
    "resultsIn",
    "weighs",
    "dependsOn",
];

/// Host-side priority tier for a typed edge (lower = expand first); see
/// [`PRIORITY_EDGES`].
fn edge_priority(predicate_local: &str) -> u8 {
    if PRIORITY_EDGES.contains(&predicate_local) {
        0
    } else {
        1
    }
}

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

// ============================================================================
// Link suggester — symbolic-first, suggest-only (invariants #1, #4, #6)
//
// Candidate generation is the hybrid retriever; legality is the SHACL relation
// catalogue; the LLM is at most a gated tiebreaker. Nothing here writes: it
// returns ranked legal candidates the agent confirms via `relate`/inline
// relations. Co-located with `relevant_context` because it reuses the same
// retrieval, neighbor, and catalogue primitives.
// ============================================================================

/// Lifecycle object properties owned by `supersede`/`retract` (and their inverses)
/// — legal between any record pair, but never *suggested*: they record decision
/// evolution, not an abductive semantic link.
const LIFECYCLE_PREDICATES: &[&str] = &[
    "supersedes",
    "isSupersededBy",
    "hasRationale",
    "isRationaleFor",
];

/// A candidate link from the suggester: a legal, currently-unasserted edge to a
/// similar record. Suggest-only — the agent confirms it through the validated
/// `relate` path (or inline relations); [`LinkSuggestion::confirm`] yields the
/// exact `relate` arguments.
#[derive(Debug, Clone)]
pub struct LinkSuggestion {
    pub predicate_local: String,
    /// The edge's subject IRI (orientation already resolved from the direction).
    pub subject_iri: String,
    /// The edge's object IRI.
    pub object_iri: String,
    /// Display label of the *other* record (the candidate, not the seed record).
    pub target_title: String,
    /// Class local name of the other record (e.g. "Requirement").
    pub target_kind: String,
    /// Relevance-derived rank score (higher = stronger).
    pub score: f32,
}

impl LinkSuggestion {
    /// Exact `(subject_iri, predicate_local, object_iri)` arguments for
    /// [`relate`] that assert this suggested edge.
    pub fn confirm(&self) -> (String, String, String) {
        (
            self.subject_iri.clone(),
            self.predicate_local.clone(),
            self.object_iri.clone(),
        )
    }
}

/// A record that the shapes say SHOULD carry a link it currently lacks.
#[derive(Debug, Clone)]
pub struct UnderLinked {
    pub iri: String,
    pub class_local: String,
    pub missing_predicate: String,
}

/// Whether the gated LLM predicate tiebreak runs (default OFF — symbolic-first).
fn llm_tiebreak_enabled(level: LlmAssistLevel) -> bool {
    matches!(
        level,
        LlmAssistLevel::AssistedValidation | LlmAssistLevel::FallbackExecutor
    )
}

/// True if any object-property edge (excluding `rdf:type`) already connects the two
/// records in the project graph, in either direction — so the suggester never
/// re-proposes an existing link.
fn record_pair_linked(state: &AppState, a: &str, b: &str) -> bool {
    object_edge_exists(state, a, b) || object_edge_exists(state, b, a)
}

fn object_edge_exists(state: &AppState, subject: &str, object: &str) -> bool {
    let (Ok(s), Ok(o)) = (NamedNodeRef::new(subject), NamedNodeRef::new(object)) else {
        return false;
    };
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    state
        .store
        .quads_for_pattern(
            Some(s.into()),
            None,
            Some(o.into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .any(|q| q.predicate.as_str() != moose::RDF_TYPE)
}

/// Choose the best legal *semantic* predicate for an ordered class pair (lifecycle
/// predicates excluded). Symbolic order is priority-tier then name; a gated LLM
/// tiebreak (default OFF) may reorder among the already-legal options when more
/// than one fits — it can never introduce a predicate. `None` ⇒ no semantic
/// predicate is legal, so prefer not-suggesting (anti-confabulation).
async fn pick_predicate(
    state: &AppState,
    iri: &str,
    hit_iri: &str,
    legal: &[LegalEdge],
    prioritize: Option<&str>,
) -> Option<LegalEdge> {
    let mut semantic: Vec<LegalEdge> = legal
        .iter()
        .filter(|e| !LIFECYCLE_PREDICATES.contains(&e.predicate_local.as_str()))
        .cloned()
        .collect();
    if semantic.is_empty() {
        return None;
    }
    // Gap-targeting: when the caller is filling a specific missing predicate and it
    // is legal for this candidate, use it — so a scan for a record "missing
    // isMotivatedBy" surfaces isMotivatedBy candidates, not just the most
    // lexically-similar legal link.
    if let Some(want) = prioritize {
        if let Some(edge) = semantic.iter().find(|e| e.predicate_local == want) {
            return Some(edge.clone());
        }
    }
    semantic.sort_by(|a, b| {
        edge_priority(&a.predicate_local)
            .cmp(&edge_priority(&b.predicate_local))
            .then_with(|| a.predicate_local.cmp(&b.predicate_local))
    });
    if semantic.len() > 1 && llm_tiebreak_enabled(state.engine_config.llm_assist_level) {
        if let Some(chosen) = llm_pick_predicate(state, iri, hit_iri, &semantic).await {
            return Some(chosen);
        }
    }
    semantic.into_iter().next()
}

/// Gated LLM tiebreak: ask the in-process sensor which single legal predicate (if
/// any) best holds between the two records. Reorders only among `candidates`; a
/// miss/"none"/error returns `None` so the caller keeps the symbolic top.
async fn llm_pick_predicate(
    state: &AppState,
    iri: &str,
    hit_iri: &str,
    candidates: &[LegalEdge],
) -> Option<LegalEdge> {
    let a = first_literal(&state.store, iri, moose::RDFS_LABEL)?;
    let a_desc = first_literal(&state.store, iri, &state.capture.description).unwrap_or_default();
    let b = first_literal(&state.store, hit_iri, moose::RDFS_LABEL)?;
    let b_desc = first_literal(&state.store, hit_iri, &state.capture.description).unwrap_or_default();
    let options: Vec<&str> = candidates.iter().map(|e| e.predicate_local.as_str()).collect();
    let prompt = format!(
        "Two software-project records:\n\
         A: \"{a}\" — {a_desc}\n\
         B: \"{b}\" — {b_desc}\n\n\
         Which ONE of these typed relationships best holds between A and B, if any?\n\
         Options: {}.\n\
         Reply with exactly one option name, or \"none\" if no relationship clearly holds.",
        options.join(", ")
    );
    let reply = state
        .llm
        .chat_completion(&state.model, &prompt, None)
        .await
        .ok()?
        .trim()
        .to_lowercase();
    candidates
        .iter()
        .find(|e| reply.contains(&e.predicate_local.to_lowercase()))
        .cloned()
}

/// Rank legal, currently-unasserted links from `iri` to records similar to it.
/// Symbolic candidate generation (hybrid retrieval) + symbolic legality (the SHACL
/// catalogue); self, already-linked pairs, and candidates with no legal semantic
/// predicate are dropped (prefer not-suggesting). Suggest-only — writes nothing.
pub async fn suggest_links_for_record(
    state: &AppState,
    iri: &str,
    top_n: usize,
    floor: Option<f32>,
    prioritize: Option<&str>,
) -> Vec<LinkSuggestion> {
    let Ok(subject) = NamedNode::new(iri) else {
        return Vec::new();
    };
    let Ok(class_iri) = require_information_record(state, &subject) else {
        return Vec::new();
    };
    let seed_text = state.record_embed_text(iri);
    if seed_text.trim().is_empty() {
        return Vec::new();
    }

    let class_iris: Vec<String> = state
        .arch_vocab
        .classes
        .iter()
        .map(|c| c.iri.clone())
        .collect();
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    // Over-fetch so the legality / already-linked filters still leave enough.
    let want = (top_n * 4).max(10);
    let ranked: Vec<(usize, String, String)> = state
        .entity_index
        .search_records_hybrid(
            &seed_text,
            &class_iris,
            &state.store,
            &data_graphs,
            &text_fields,
            want,
            &state.instance_store,
            floor,
        )
        .into_iter()
        .enumerate()
        .map(|(rank, h)| (rank, h.iri, h.class_iri))
        .collect();

    let mut suggestions: Vec<LinkSuggestion> = Vec::new();
    for (rank, hit_iri, hit_class) in ranked {
        if hit_iri == iri || record_pair_linked(state, iri, &hit_iri) {
            continue;
        }
        let legal = state
            .catalogue
            .legal_predicates(&state.store, &class_iri, &hit_class);
        let Some(edge) = pick_predicate(state, iri, &hit_iri, &legal, prioritize).await else {
            continue;
        };
        let (subject_iri, object_iri) = match edge.direction {
            EdgeDirection::Forward => (iri.to_string(), hit_iri.clone()),
            EdgeDirection::Inverse => (hit_iri.clone(), iri.to_string()),
        };
        suggestions.push(LinkSuggestion {
            predicate_local: edge.predicate_local,
            subject_iri,
            object_iri,
            target_title: first_literal(&state.store, &hit_iri, moose::RDFS_LABEL)
                .unwrap_or_else(|| hit_iri.clone()),
            target_kind: local_name(&hit_class).to_string(),
            score: 1.0 / (1.0 + rank as f32),
        });
    }
    // Prioritized predicate (the record's missing link) first, then typed-edge
    // priority, then similarity, then IRI for determinism.
    let prio_rank = |p: &str| -> u8 {
        match prioritize {
            Some(want) if want == p => 0,
            _ => 1,
        }
    };
    suggestions.sort_by(|a, b| {
        prio_rank(&a.predicate_local)
            .cmp(&prio_rank(&b.predicate_local))
            .then(edge_priority(&a.predicate_local).cmp(&edge_priority(&b.predicate_local)))
            .then(
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then_with(|| a.object_iri.cmp(&b.object_iri))
    });
    suggestions.truncate(top_n);
    suggestions
}

/// The `sh:or` "should-have-a-link" requirements: each NodeShape with an `sh:or`
/// maps its `sh:targetClass` to the branch predicates a conforming record SHOULD
/// carry at least one of. The declarative source of truth for the link advisory.
fn shacl_or_link_requirements(state: &AppState) -> Vec<(String, Vec<String>)> {
    let sparql = format!(
        r#"
SELECT DISTINCT ?targetClass ?predicate
WHERE {{
  VALUES ?shapeGraph {{ <{}> <{}> }}
  GRAPH ?shapeGraph {{
    ?shape <{}> ?targetClass ;
           <{}>/<{}>*/<{}> ?branch .
    ?branch <{}> ?predicate .
  }}
}}"#,
        ontology::SE_SHAPES_GRAPH_IRI,
        ontology::ARCH_SHAPES_GRAPH_IRI,
        SH_TARGET_CLASS,
        SH_OR,
        RDF_REST,
        RDF_FIRST,
        SH_PATH,
    );
    let Ok(QueryResults::Solutions(solutions)) = run_sparql(&state.store, &sparql) else {
        return Vec::new();
    };
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for sol in solutions.flatten() {
        let (Some(target_class), Some(predicate)) =
            (iri_value(sol.get("targetClass")), iri_value(sol.get("predicate")))
        else {
            continue;
        };
        if let Some((_, preds)) = groups.iter_mut().find(|(c, _)| c == &target_class) {
            if !preds.contains(&predicate) {
                preds.push(predicate);
            }
        } else {
            groups.push((target_class, vec![predicate]));
        }
    }
    groups
}

/// Records the shapes say SHOULD carry a link (an `sh:or` branch predicate) but
/// currently lack every one of those predicates. Drives the non-blocking validate
/// advisory and the `suggest_links` scan. Bounded by `max_records`.
pub fn under_linked_records(state: &AppState, max_records: usize) -> Vec<UnderLinked> {
    let mut out: Vec<UnderLinked> = Vec::new();
    for (target_class, predicates) in shacl_or_link_requirements(state) {
        let not_exists: String = predicates
            .iter()
            .enumerate()
            .map(|(i, p)| format!("    FILTER NOT EXISTS {{ ?node <{p}> ?v{i} }}\n"))
            .collect();
        // Records are typed directly as these leaf classes, so an exact `rdf:type`
        // match in the project graph suffices (no cross-graph subclass path).
        // Exclude superseded/deprecated records: like every other read, the advisory
        // concerns the current working set, not history.
        let sparql = format!(
            "SELECT DISTINCT ?node\nWHERE {{\n  GRAPH <{}> {{\n    ?node <{}> <{}> .\n{}    FILTER NOT EXISTS {{ ?node <{}> ?st . FILTER(STR(?st) = \"superseded\" || STR(?st) = \"deprecated\") }}\n  }}\n}}",
            PROJECT_KG_GRAPH_IRI,
            moose::RDF_TYPE,
            target_class,
            not_exists,
            state.capture.status
        );
        let Ok(QueryResults::Solutions(solutions)) = run_sparql(&state.store, &sparql) else {
            continue;
        };
        for sol in solutions.flatten() {
            if let Some(node) = iri_value(sol.get("node")) {
                out.push(UnderLinked {
                    iri: node,
                    class_local: local_name(&target_class).to_string(),
                    missing_predicate: local_name(&predicates[0]).to_string(),
                });
                if out.len() >= max_records {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Architecture-domain class namespace (matches the shipped ontologies).
    const ARCH: &str = "https://trivyn.io/ontologies/software/architecture/domain/";

    /// In-memory store with just the shipped domain + SHACL shape graphs loaded —
    /// enough to build and exercise the relation catalogue.
    fn shapes_store() -> Store {
        let store = Store::new().expect("in-memory store");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
        ontology::load_ontologies(&store, &dir).expect("load ontologies");
        store
    }

    fn cls(local: &str) -> String {
        format!("{ARCH}{local}")
    }

    #[test]
    fn catalogue_captures_object_properties_including_union_ranges() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        let locals = cat.predicate_locals();
        for p in [
            "isMotivatedBy",
            "violates",
            "constrains",
            "learnedFrom",
            "concerns",
            "weighs",
            "resultsIn",
            "supersedes",
            "dependsOn",
        ] {
            assert!(locals.contains(p), "catalogue missing object property {p:?}");
        }
        // `isMotivatedBy` has a SHACL `sh:or` union range — both branches present.
        let to_req =
            cat.legal_predicates(&store, &cls("ArchitecturalDecision"), &cls("Requirement"));
        let to_con =
            cat.legal_predicates(&store, &cls("ArchitecturalDecision"), &cls("Constraint"));
        assert!(to_req
            .iter()
            .any(|e| e.predicate_local == "isMotivatedBy" && e.direction == EdgeDirection::Forward));
        assert!(to_con
            .iter()
            .any(|e| e.predicate_local == "isMotivatedBy" && e.direction == EdgeDirection::Forward));
    }

    #[test]
    fn legal_predicates_respects_domain_range_and_direction() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        // No *semantic* object property links an AntiPattern to a Requirement
        // (`violates` ranges over Constraint, `isMotivatedBy`'s domain is a
        // decision). Only the InformationRecord-level lifecycle predicates
        // (supersedes/isSupersededBy) apply to any record pair — the suggester
        // filters those out.
        let semantic: Vec<_> = cat
            .legal_predicates(&store, &cls("AntiPattern"), &cls("Requirement"))
            .into_iter()
            .filter(|e| {
                !matches!(
                    e.predicate_local.as_str(),
                    "supersedes" | "isSupersededBy" | "hasRationale" | "isRationaleFor"
                )
            })
            .collect();
        assert!(semantic.is_empty(), "unexpected semantic links: {semantic:?}");
        // From a Requirement to a decision, `isMotivatedBy` is legal but Inverse
        // (the edge runs decision -> requirement).
        let from_req =
            cat.legal_predicates(&store, &cls("Requirement"), &cls("ArchitecturalDecision"));
        assert!(from_req
            .iter()
            .any(|e| e.predicate_local == "isMotivatedBy" && e.direction == EdgeDirection::Inverse));
    }

    #[test]
    fn priority_edges_are_all_in_catalogue() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        let locals = cat.predicate_locals();
        for p in PRIORITY_EDGES {
            assert!(
                locals.contains(p),
                "PRIORITY_EDGES lists {p:?} but no SHACL shape declares it (ontology drift)"
            );
        }
    }
}
