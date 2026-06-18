//! Build + open the ontology embedding vector store — MOOSE's L2 alignment tier.
//!
//! MOOSE reads ontology vectors from a SQLite `ontology_vectors` table via
//! [`VecStore::open`] but exposes no public *write* path, so MOOSEDev builds the
//! table itself using MOOSE's public encoding ([`embedding_to_blob`]) and stamp
//! ([`VecStore::write_stamp`]). Vectors are embedded with the **document-side**
//! recipe (label + definition + altLabels, no query prefix), matching what
//! MOOSE's query side compares against (template: MOOSE `…/chinook.rs`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use moose::embeddings::vec_store::{ElementType, StoreStamp, VecStore};
use moose::embeddings::{default_backbone, embedding_to_blob};
use moose::types::VocabularyEntry;
use moose::vocabulary::extract_compact_vocabulary;
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};
use oxigraph::store::Store;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// `skos:definition` — the ontologies' definition predicate (MOOSE exposes
/// `SKOS_ALT_LABEL`/`SKOS_PREF_LABEL` constants but not this one).
const SKOS_DEFINITION: &str = "http://www.w3.org/2004/02/skos/core#definition";

/// One ontology element's embed inputs: the identity we store and the exact text
/// we embed. Collected once and used for **both** the freshness fingerprint and
/// the build, so the cache key can never drift from what actually goes into the
/// vectors. `label` is stored verbatim in row metadata (also embedded in `content`).
struct EmbedInput {
    iri: String,
    element_type: ElementType,
    label: String,
    content: String,
}

/// Build the ontology vector store at `db_path` from the given domain graphs and
/// open it. Embeds every `owl:Class` and `owl:DatatypeProperty` (object properties
/// aren't ranked on, so they're skipped).
///
/// **Cached:** a previously built store is reused when its ontology fingerprint
/// still matches *and* it opens cleanly. The shipped ontology only changes on a
/// version bump, so the common startup is a cache hit — no embedding-backbone load
/// and no re-embedding. A rebuild is forced when the ontology content changes (the
/// fingerprint flips) or the embedding model changes (`VecStore::open` validates
/// the stamp against the compiled-in active model and errors on drift).
pub async fn build_and_open(
    store: &Store,
    domain_graph_iris: &[&str],
    db_path: &Path,
) -> anyhow::Result<VecStore> {
    let inputs = collect_embed_inputs(store, domain_graph_iris)?;
    let fingerprint = ontology_fingerprint(&inputs);
    let fp_path = fingerprint_path(db_path);

    // Fast path: reuse the persisted store when nothing that affects the vectors
    // has changed. `open` is cheap (no backbone load) and rejects model drift.
    if let Some(vec_store) = try_reuse(db_path, &fp_path, &fingerprint).await {
        tracing::info!(
            "[vectors] reusing cached ontology vector store ({} vectors, ontology + model unchanged): {}",
            inputs.len(),
            db_path.display()
        );
        return Ok(vec_store);
    }

    let backbone =
        default_backbone().map_err(|e| anyhow::anyhow!("load embedding backbone: {e}"))?;

    // Fresh build: drop any prior DB (and WAL/SHM) plus the stale fingerprint so
    // rows don't accumulate and a crash mid-build can't leave a "fresh"-looking
    // store (the fingerprint is rewritten only after a successful stamp, below).
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
    }
    let _ = std::fs::remove_file(&fp_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create vector store dir {}: {e}", parent.display()))?;
    }

    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .map_err(|e| anyhow::anyhow!("vector db path {}: {e}", db_path.display()))?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("open vector db for writing: {e}"))?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ontology_vectors \
         (id TEXT, element_type TEXT, metadata TEXT, embedding BLOB)",
    )
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("create ontology_vectors table: {e}"))?;

    for input in &inputs {
        let vector = backbone
            .embed_document(&input.content)
            .map_err(|e| anyhow::anyhow!("embed {}: {e}", input.iri))?;
        let metadata = serde_json::json!({ "label": input.label }).to_string();
        sqlx::query(
            "INSERT INTO ontology_vectors (id, element_type, metadata, embedding) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&input.iri)
        .bind(input.element_type.as_db_value())
        .bind(metadata)
        .bind(embedding_to_blob(&vector))
        .execute(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("insert vector {}: {e}", input.iri))?;
    }
    pool.close().await;

    // Stamp with the active model identity so MOOSE can reject query/index drift.
    VecStore::write_stamp(
        db_path,
        &StoreStamp {
            model_id: backbone.model_id().to_string(),
            dim: backbone.dim(),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("stamp vector store: {e}"))?;

    // Record the fingerprint last: a store is only advertised as fresh once its
    // rows and model stamp are fully written.
    std::fs::write(&fp_path, &fingerprint)
        .map_err(|e| anyhow::anyhow!("write fingerprint {}: {e}", fp_path.display()))?;

    tracing::info!(
        "[vectors] built ontology vector store: {} vectors at {}",
        inputs.len(),
        db_path.display()
    );

    VecStore::open(None, Some(db_path))
        .await
        .map_err(|e| anyhow::anyhow!("open vector store: {e}"))
}

/// Try to reuse a persisted store: returns the opened store iff the fingerprint
/// sidecar matches `fingerprint` and it opens cleanly with vectors. `open`
/// validates the embedding-model stamp against the compiled-in active model, so a
/// model change (or a corrupt/empty store) returns `None` and the caller rebuilds.
async fn try_reuse(db_path: &Path, fp_path: &Path, fingerprint: &str) -> Option<VecStore> {
    if !db_path.exists() || std::fs::read_to_string(fp_path).ok().as_deref() != Some(fingerprint) {
        return None;
    }
    match VecStore::open(None, Some(db_path)).await {
        Ok(vec_store) if vec_store.is_enabled() => Some(vec_store),
        Ok(_) => {
            tracing::info!("[vectors] cached store has no vectors; rebuilding");
            None
        }
        Err(e) => {
            tracing::info!("[vectors] cached store unusable ({e}); rebuilding");
            None
        }
    }
}

/// Collect the embed inputs for every `owl:Class` and `owl:DatatypeProperty` in
/// `domain_graph_iris`, in a deterministic order. Pure graph reads — no model load
/// — so it's cheap enough to run on every startup to compute the fingerprint.
fn collect_embed_inputs(
    store: &Store,
    domain_graph_iris: &[&str],
) -> anyhow::Result<Vec<EmbedInput>> {
    let mut inputs = Vec::new();
    for graph_iri in domain_graph_iris {
        let vocab = extract_compact_vocabulary(store, graph_iri, None)
            .map_err(|e| anyhow::anyhow!("extract_compact_vocabulary({graph_iri}): {e:?}"))?;
        for (entries, kind) in [
            (&vocab.classes, ElementType::Class),
            (&vocab.datatype_properties, ElementType::DatatypeProperty),
        ] {
            for entry in entries {
                inputs.push(EmbedInput {
                    iri: entry.iri.clone(),
                    element_type: kind,
                    label: entry
                        .label
                        .clone()
                        .unwrap_or_else(|| entry.local_name.clone()),
                    content: embed_text(store, graph_iri, entry)?,
                });
            }
        }
    }
    Ok(inputs)
}

/// A content fingerprint over the exact `(iri, element_type, embed-text)` tuples
/// that determine the stored vectors — the cache key for deciding whether a
/// persisted store is still fresh. Deterministic across runs of the same binary
/// (fixed-seed `DefaultHasher`); a compiler/std change can only perturb it, which
/// merely forces a (safe) rebuild. `label` is omitted because it's already part of
/// `content`.
fn ontology_fingerprint(inputs: &[EmbedInput]) -> String {
    let mut hasher = DefaultHasher::new();
    inputs.len().hash(&mut hasher);
    for input in inputs {
        input.iri.hash(&mut hasher);
        input.element_type.as_db_value().hash(&mut hasher);
        input.content.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Sidecar path recording the ontology fingerprint a built store was made from
/// (co-located with the DB so it's cleaned up with the data dir).
fn fingerprint_path(db_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.fingerprint", db_path.display()))
}

/// Document-side embed text for one ontology element: `Term: <label>. Definition:
/// <def>. Alternative labels: <alts>` — the recipe MOOSE's index side uses.
fn embed_text(store: &Store, graph_iri: &str, entry: &VocabularyEntry) -> anyhow::Result<String> {
    let label = entry
        .label
        .clone()
        .unwrap_or_else(|| entry.local_name.clone());
    let mut content = format!("Term: {label}");

    let def = literals_for(store, graph_iri, &entry.iri, SKOS_DEFINITION)?
        .into_iter()
        .next()
        .or_else(|| entry.comment.clone());
    if let Some(def) = def.filter(|d| !d.trim().is_empty()) {
        content.push_str(&format!(". Definition: {def}"));
    }

    let alts = literals_for(store, graph_iri, &entry.iri, moose::SKOS_ALT_LABEL)?;
    if !alts.is_empty() {
        content.push_str(&format!(". Alternative labels: {}", alts.join(", ")));
    }
    Ok(content)
}

/// Collect the literal objects of `(iri, predicate, *)` in the given graph.
fn literals_for(
    store: &Store,
    graph_iri: &str,
    iri: &str,
    predicate: &str,
) -> anyhow::Result<Vec<String>> {
    let subject = NamedNodeRef::new(iri).map_err(|e| anyhow::anyhow!("iri {iri}: {e}"))?;
    let pred =
        NamedNodeRef::new(predicate).map_err(|e| anyhow::anyhow!("predicate {predicate}: {e}"))?;
    let graph =
        NamedNodeRef::new(graph_iri).map_err(|e| anyhow::anyhow!("graph {graph_iri}: {e}"))?;
    Ok(store
        .quads_for_pattern(
            Some(subject.into()),
            Some(pred),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
        .collect())
}
