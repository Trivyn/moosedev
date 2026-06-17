//! Build + open the ontology embedding vector store — MOOSE's L2 alignment tier.
//!
//! MOOSE reads ontology vectors from a SQLite `ontology_vectors` table via
//! [`VecStore::open`] but exposes no public *write* path, so MOOSEDev builds the
//! table itself using MOOSE's public encoding ([`embedding_to_blob`]) and stamp
//! ([`VecStore::write_stamp`]). Vectors are embedded with the **document-side**
//! recipe (label + definition + altLabels, no query prefix), matching what
//! MOOSE's query side compares against (template: MOOSE `…/chinook.rs`).

use std::path::Path;
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

/// Build the ontology vector store at `db_path` from the given domain graphs and
/// open it. Rebuilt fresh each call (regen-safe). Embeds every `owl:Class` and
/// `owl:DatatypeProperty`; object properties aren't ranked on, so they're skipped.
pub async fn build_and_open(
    store: &Store,
    domain_graph_iris: &[&str],
    db_path: &Path,
) -> anyhow::Result<VecStore> {
    let backbone =
        default_backbone().map_err(|e| anyhow::anyhow!("load embedding backbone: {e}"))?;

    // Fresh build: drop any prior DB (and WAL/SHM) so rows don't accumulate.
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
    }
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

    let mut count = 0usize;
    for graph_iri in domain_graph_iris {
        let vocab = extract_compact_vocabulary(store, graph_iri, None)
            .map_err(|e| anyhow::anyhow!("extract_compact_vocabulary({graph_iri}): {e:?}"))?;
        for (entries, kind) in [
            (&vocab.classes, ElementType::Class),
            (&vocab.datatype_properties, ElementType::DatatypeProperty),
        ] {
            for entry in entries {
                let content = embed_text(store, graph_iri, entry)?;
                let vector = backbone
                    .embed_document(&content)
                    .map_err(|e| anyhow::anyhow!("embed {}: {e}", entry.iri))?;
                let metadata = serde_json::json!({
                    "label": entry.label.clone().unwrap_or_else(|| entry.local_name.clone()),
                })
                .to_string();
                sqlx::query(
                    "INSERT INTO ontology_vectors (id, element_type, metadata, embedding) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(&entry.iri)
                .bind(kind.as_db_value())
                .bind(metadata)
                .bind(embedding_to_blob(&vector))
                .execute(&pool)
                .await
                .map_err(|e| anyhow::anyhow!("insert vector {}: {e}", entry.iri))?;
                count += 1;
            }
        }
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

    tracing::info!(
        "[vectors] built ontology vector store: {count} vectors at {}",
        db_path.display()
    );

    VecStore::open(None, Some(db_path))
        .await
        .map_err(|e| anyhow::anyhow!("open vector store: {e}"))
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
