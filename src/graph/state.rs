//! Server state, ontology/index bootstrap, and lazy GROWL enrichment.
//! This module owns long-lived graph services and calls sibling modules only for focused primitives.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use moose::chat::session_db::SessionDb;
use moose::embeddings::vec_store::VecStore;
use moose::embeddings::{default_backbone, embed_and_index_instance, InstanceVecStore};
use moose::entity_index::EntityIndexCache;
use moose::kg::{AssertionLiteral, DatatypeAssertion};
use moose::moose_ontology::MooseOntologyCache;
use moose::traits::{ChatConfig, EngineConfig};
use moose::types::{CompactVocabulary, FallbackPolicy, HybridConfig, LlmAssistLevel, WalkBudgets};
use oxigraph::model::NamedNode;
use oxigraph::store::Store;

use crate::code::substrate::Substrate;
use crate::llm::{LlmConfig, OpenAiCompatClient};
use crate::ontology::{self, MooseDevOntologyResolver};

use super::capture::require_information_record;
use super::context::{first_literal, list_instances};
use super::relations::{build_relation_catalogue, RelationCatalogue};
use super::util::{datatype_property_iri, iri_by_local_name, object_property_iri};
use super::PROJECT_KG_GRAPH_IRI;

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
pub(crate) const LABEL_PROPERTY_LOCAL: &str = "labelProperty";
pub(crate) const DEFAULT_LIFECYCLE_STATUS: &str = "accepted";
pub(crate) const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

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
    /// Code vocabulary kept separate so code classes stay out of capture kinds,
    /// the dense instance index, and `get_relevant_context` seeding.
    pub code_vocab: CompactVocabulary,
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
    /// True only when `MOOSEDEV_LLM_BASE_URL` explicitly opts into an LLM provider.
    pub llm_configured: bool,
    pub ontology_resolver: MooseDevOntologyResolver,
    pub model: String,
    /// Durable multi-turn MOOSE chat sessions, enabled by the shared backend for
    /// the human web UI.
    pub session_db: Option<Arc<SessionDb>>,
    /// Data dir (the persistent KG store and the built vector DB live here).
    pub data_dir: PathBuf,
    /// Best-effort code substrate. Absent when no index has been built; position-
    /// based tools degrade with honest errors instead of blocking server startup.
    substrate: RwLock<Option<Arc<Substrate>>>,
    /// Serializes disk-backed substrate reloads after `moosedev index` updates
    /// the on-disk pair. The getter does no watcher or background work.
    substrate_reload_lock: std::sync::Mutex<()>,
    /// Set true by any write that changes the project graph; drained by
    /// [`AppState::ensure_enriched`] before a read, so GROWL re-materializes the
    /// inferred inverse/subproperty edges lazily — one pass per capture burst.
    pub inferred_stale: std::sync::atomic::AtomicBool,
    /// Monotonic signal for consumers that need to observe project-graph writes.
    project_write_generation: AtomicU64,
    /// Keeps the committed canonical text (`kg.nq`) in step with project-graph
    /// writes: isolated captures export synchronously, bulk bursts coalesce to
    /// a single trailing export once the burst goes quiet.
    canonical_throttle: crate::canonical::WriteThrottle,
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
        Self::bootstrap_with_llm_config(data_dir, ontology_dir, LlmConfig::from_env())
    }

    /// Variant of [`bootstrap`](Self::bootstrap) for tests and embedded hosts
    /// that already resolved LLM configuration.
    #[doc(hidden)]
    pub fn bootstrap_with_llm_config(
        data_dir: &Path,
        ontology_dir: &Path,
        llm_cfg: LlmConfig,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
        let store = open_store(data_dir)?;
        // Reconcile the committed canonical text (kg.nq) with the freshly opened
        // store before anything downstream reads it — the text is the committed
        // source of truth and this store a derived cache (Requirement d459cac2).
        // Fatal when the file exists but cannot be loaded (e.g. unresolved merge
        // conflict markers): continuing would let the next write-through clobber it.
        let sync = crate::canonical::sync_on_startup(&store, data_dir)?;
        tracing::info!(
            "[canonical] startup sync: {:?} ({} quad(s))",
            sync.action,
            sync.quad_count
        );
        // MOOSE loads its engine ontologies (MOOSE-Pipeline.ttl, …) via its own
        // search, which doesn't resolve the brew/curl symlink to the real install
        // dir. Hand it the dir moosedev already resolves correctly (and where the
        // tarball bundles MOOSE's ontologies) via an OPT-IN env var — no signature
        // change, so MOOSE's other consumers are unaffected.
        std::env::set_var("MOOSE_ONTOLOGY_DIR", ontology_dir);
        let moose_cache =
            moose::initialize(&store).map_err(|e| anyhow::anyhow!("moose::initialize: {e:?}"))?;
        let ontology::DomainVocabularies { arch, code } =
            ontology::load_ontologies(&store, ontology_dir)?;
        let capture = CapturePredicates::resolve(&arch)?;
        let catalogue = build_relation_catalogue(&store);
        let entity_index = Arc::new(EntityIndexCache::new(64));

        let llm_configured = llm_cfg.configured;
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
            discourse: None,
            moose_cache: moose_cache.clone(),
            llm_assist_level: assist_level_from_env(llm_configured),
            // The env dial is the single control; level 2 stays opt-in there.
            fallback_policy: FallbackPolicy::Allowed,
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
            arch_vocab: arch,
            code_vocab: code,
            capture,
            catalogue,
            engine_config,
            llm,
            llm_configured,
            ontology_resolver,
            model: llm_cfg.model,
            session_db: None,
            vector_store: None,
            data_dir: data_dir.to_path_buf(),
            substrate: RwLock::new(None),
            substrate_reload_lock: std::sync::Mutex::new(()),
            // Start stale so the first read after startup materializes inferred edges.
            inferred_stale: std::sync::atomic::AtomicBool::new(true),
            project_write_generation: AtomicU64::new(0),
            canonical_throttle: crate::canonical::WriteThrottle::default(),
            enrich_lock: std::sync::Mutex::new(()),
        })
    }

    /// Mark the reasoner-materialized edges stale — call after any write that changes the
    /// project graph, so the next read re-enriches.
    pub fn mark_inferred_stale(&self) {
        self.inferred_stale
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Post-write hook for every project-graph mutation: invalidate the lazily
    /// materialized inferred edges AND keep the committed canonical text
    /// (`kg.nq`) in step — synchronously for an isolated capture, coalesced to
    /// one trailing export for a bulk burst (see
    /// [`crate::canonical::WriteThrottle`]). Best-effort: a text-export failure
    /// must never fail the symbolic write (invariant #1).
    pub fn note_project_write(&self) {
        self.mark_inferred_stale();
        self.project_write_generation
            .fetch_add(1, Ordering::Relaxed);
        self.canonical_throttle
            .note_write(&self.store, &self.data_dir);
    }

    pub fn project_write_generation(&self) -> u64 {
        self.project_write_generation.load(Ordering::Relaxed)
    }

    /// Return the loaded code substrate, reloading a disk-backed one when the
    /// completed on-disk metadata identifies a newer index. Synthetic test
    /// substrates have no repository root and intentionally bypass this path.
    pub fn substrate(&self) -> Option<Arc<Substrate>> {
        let substrate = self.current_substrate()?;
        if substrate.repo_root().is_none() || !self.substrate_meta_changed(&substrate) {
            return Some(substrate);
        }

        let _reload_guard = self
            .substrate_reload_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A concurrent caller may have finished the reload while we waited.
        let substrate = self.current_substrate()?;
        let Some(repo_root) = substrate.repo_root().map(Path::to_path_buf) else {
            return Some(substrate);
        };
        if !self.substrate_meta_changed(&substrate) {
            return Some(substrate);
        }

        match Substrate::load(&self.data_dir, &repo_root) {
            Ok(reloaded) => {
                let definitions = reloaded.stats().definitions;
                let indexed_at = reloaded.meta().indexed_at;
                let reloaded = Arc::new(reloaded);
                self.set_substrate(reloaded.clone());
                tracing::info!(
                    "substrate reloaded: {definitions} definition(s), indexed at {indexed_at}"
                );
                Some(reloaded)
            }
            Err(error) => {
                tracing::warn!("substrate reload failed; serving previous substrate: {error}");
                Some(substrate)
            }
        }
    }

    fn current_substrate(&self) -> Option<Arc<Substrate>> {
        self.substrate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// A completed producer run writes metadata last. Unreadable metadata can
    /// therefore be a partial rewrite; retain the known-good in-memory index.
    fn substrate_meta_changed(&self, substrate: &Substrate) -> bool {
        let Ok(on_disk) = crate::code::substrate::SubstrateMeta::load(&self.data_dir) else {
            return false;
        };
        let loaded = substrate.meta();
        on_disk.indexed_commit != loaded.indexed_commit || on_disk.indexed_at != loaded.indexed_at
    }

    /// Replace the current code substrate. Used by runtime startup and tests.
    pub fn set_substrate(&self, substrate: Arc<Substrate>) {
        *self
            .substrate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(substrate);
    }

    /// Best-effort load of the code substrate from disk. Missing or invalid
    /// indexes are normal before `moosedev index`; callers should surface
    /// substrate absence at tool-use time.
    pub fn load_substrate(&self, repo_root: &Path) {
        match Substrate::load(&self.data_dir, repo_root) {
            Ok(substrate) => {
                let definitions = substrate.stats().definitions;
                self.set_substrate(Arc::new(substrate));
                tracing::info!("MOOSEDev: loaded code substrate ({definitions} definition(s))");
            }
            Err(e) => tracing::info!("MOOSEDev: code substrate unavailable: {e}"),
        }
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
    pub(crate) fn record_embed_text(&self, iri: &str) -> String {
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

    /// Resolve a code-domain class by local-name lookup in the loaded code
    /// vocabulary. Kept separate from [`Self::resolve_class`] so capture kinds
    /// and the dense instance index remain architecture-scoped.
    pub fn resolve_code_class(&self, local: &str) -> anyhow::Result<String> {
        iri_by_local_name(&self.code_vocab.classes, local).ok_or_else(|| {
            anyhow::anyhow!("unknown code class {local:?}: not a class in the code ontology")
        })
    }

    /// Resolve a code-domain datatype property by local name.
    pub fn resolve_code_datatype_property(&self, local: &str) -> anyhow::Result<String> {
        datatype_property_iri(&self.code_vocab, local).map_err(|_| {
            anyhow::anyhow!(
                "unknown code datatype property {local:?}: not a datatype property in the code ontology"
            )
        })
    }

    /// Resolve a relation local name (e.g. "supersedes", "hasRationale") to its
    /// full IRI from the loaded vocabularies, checking architecture first and
    /// then the code domain for code-side intent links.
    pub fn resolve_object_property(&self, local: &str) -> anyhow::Result<String> {
        if let Ok(iri) = object_property_iri(&self.arch_vocab, local) {
            return Ok(iri);
        }
        if let Ok(iri) = object_property_iri(&self.code_vocab, local) {
            return Ok(iri);
        }
        anyhow::bail!(
            "unknown relation {local:?}: not an object property in the architecture or code ontology"
        )
    }
}

/// LLM assist level from `MOOSEDEV_LLM_ASSIST_LEVEL` (0–2; legacy 3–5 accepted
/// for one release via `LlmAssistLevel::from_u8`). Without an explicit provider
/// config, assistance is pinned to pure symbolic regardless of env.
fn assist_level_from_env(llm_configured: bool) -> LlmAssistLevel {
    assist_level_from_raw(
        std::env::var("MOOSEDEV_LLM_ASSIST_LEVEL").ok().as_deref(),
        llm_configured,
    )
}

fn assist_level_from_raw(raw: Option<&str>, llm_configured: bool) -> LlmAssistLevel {
    if !llm_configured {
        return LlmAssistLevel::PureSymbolic;
    }
    raw.and_then(|s| s.trim().parse::<u8>().ok())
        .and_then(LlmAssistLevel::from_u8)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCH: &str = "https://trivyn.io/ontologies/software/architecture#";
    const CODE: &str = "https://trivyn.io/ontologies/software/code#";

    fn bootstrap_test_state(name: &str) -> AppState {
        let dir =
            std::env::temp_dir().join(format!("moosedev-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ontology_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
        AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
    }

    #[test]
    fn assist_level_is_pure_symbolic_without_provider() {
        assert!(matches!(
            assist_level_from_raw(None, false),
            LlmAssistLevel::PureSymbolic
        ));
        assert!(matches!(
            assist_level_from_raw(Some("5"), false),
            LlmAssistLevel::PureSymbolic
        ));
    }

    #[test]
    fn assist_level_defaults_to_sensor_when_provider_is_configured() {
        assert!(matches!(
            assist_level_from_raw(None, true),
            LlmAssistLevel::Sensor
        ));
        assert!(matches!(
            assist_level_from_raw(Some("0"), true),
            LlmAssistLevel::PureSymbolic
        ));
        assert!(matches!(
            assist_level_from_raw(Some("5"), true),
            LlmAssistLevel::SensorWithFallback
        ));
    }

    #[test]
    fn code_vocabulary_resolution_is_separate_from_capture_kinds() {
        let state = bootstrap_test_state("code-vocab-resolution");

        assert_eq!(
            state.resolve_code_class("CodeEntity").unwrap(),
            format!("{CODE}CodeEntity")
        );
        assert!(
            state.resolve_class("CodeEntity").is_err(),
            "CodeEntity must stay out of architecture capture kinds"
        );
        assert_eq!(
            state
                .resolve_code_datatype_property("hasSubstrateSymbol")
                .unwrap(),
            format!("{CODE}hasSubstrateSymbol")
        );
        assert_eq!(
            state.resolve_code_class("ProposedLink").unwrap(),
            format!("{CODE}ProposedLink")
        );
        assert!(
            state.resolve_class("ProposedLink").is_err(),
            "ProposedLink must stay out of architecture capture kinds"
        );
        for prop in [
            "proposesSubject",
            "proposesPredicate",
            "proposesTargetSymbol",
            "proposesTargetPath",
            "proposesTargetIri",
            "hasConfidence",
            "hasEscalation",
        ] {
            assert_eq!(
                state.resolve_code_datatype_property(prop).unwrap(),
                format!("{CODE}{prop}")
            );
        }
        // Judgment-stratum classes and playing-relations (un-parked).
        for class in ["CodeRole", "Criticality"] {
            assert_eq!(
                state.resolve_code_class(class).unwrap(),
                format!("{CODE}{class}")
            );
            assert!(
                state.resolve_class(class).is_err(),
                "{class} must stay out of architecture capture kinds"
            );
        }
        for prop in ["playsRole", "hasCriticality"] {
            assert_eq!(
                state.resolve_object_property(prop).unwrap(),
                format!("{CODE}{prop}")
            );
        }
        assert_eq!(
            state.resolve_object_property("realizes").unwrap(),
            format!("{CODE}realizes")
        );
        assert_eq!(
            state.resolve_object_property("concerns").unwrap(),
            format!("{ARCH}concerns")
        );
    }
}
