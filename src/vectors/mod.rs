//! Build + open the ontology embedding vector store — MOOSE's L2 alignment tier.
//!
//! MOOSE reads ontology vectors from a SQLite `ontology_vectors` table via
//! [`VecStore::open`] but exposes no public *write* path, so MOOSEDev builds the
//! table itself using MOOSE's public encoding ([`embedding_to_blob`]) and stamp
//! ([`VecStore::write_stamp`]). Vectors are embedded with the **document-side**
//! recipe (label + definition + altLabels, no query prefix), matching what
//! MOOSE's query side compares against (template: MOOSE `…/chinook.rs`).
//!
//! Owning the writer means owning the table's contract. Every row carries a
//! `namespace` (the pool a read scopes to) and an `owning_graph` (the domain graph
//! the term was declared in), and no two rows may share an `(element_type, iri)` —
//! MOOSE rejects the entire store at open if two loaded rows collide.
//!
//! These rows are *derived* — a pure function of the shipped ontologies and the
//! embedding backbone — so this store is never migrated in place. It records the
//! layout generation it was written with in `store_meta`, and is regenerated whole
//! whenever that does not match `MOOSEDEV_VECTOR_SCHEMA`. That makes a jump across
//! several generations no different from a single one. Bump that constant when
//! changing the DDL, the columns written, or the namespace.

use std::collections::HashSet;
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

/// The `namespace` every row this producer writes carries, and the pool label a
/// read scopes to. MOOSE assigns no meaning to the string — it is an exact-match
/// SQL filter — but it is a *storage contract*: changing it strands every store
/// written under the old value until it is rebuilt.
pub const ONTOLOGY_VECTOR_NAMESPACE: &str = "domain";

/// The scope every read of this store must pass. A `const` (not a runtime
/// `Vec<&str>`) so call sites have no borrow to bind.
pub const ONTOLOGY_VECTOR_SCOPE: &[&str] = &[ONTOLOGY_VECTOR_NAMESPACE];

/// Layout generation of the `ontology_vectors` table this producer writes,
/// recorded in `store_meta` under [`MOOSEDEV_VECTOR_SCHEMA_KEY`]. A store stamped
/// with anything else — older, newer, or absent — is regenerated rather than
/// migrated, which is why no version-to-version upgrade path is needed.
const MOOSEDEV_VECTOR_SCHEMA: &str = "2";

/// MOOSEDev's own `store_meta` key. Deliberately *not* Trivyn's
/// `trivyn_vector_schema`: MOOSE hard-errors on any value of that key other than
/// its own expected one and keeps the constants private, so MOOSEDev could not
/// track a future bump. MOOSE ignores keys it does not know.
const MOOSEDEV_VECTOR_SCHEMA_KEY: &str = "moosedev_vector_schema";

/// One ontology element's embed inputs: the identity we store and the exact text
/// we embed. Collected once and used for **both** the freshness fingerprint and
/// the build, so the cache key can never drift from what actually goes into the
/// vectors. `label` is stored verbatim in row metadata (also embedded in `content`).
struct EmbedInput {
    iri: String,
    element_type: ElementType,
    label: String,
    content: String,
    /// The domain graph this element was declared in, written verbatim to the row's
    /// `owning_graph`. Provenance only — MOOSE carries it through for diagnostics
    /// and never matches on it.
    owning_graph: String,
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
    // `NOT NULL` on the first four is load-bearing, not decoration: MOOSE decodes
    // them into non-optional `String`s, so a NULL is a decode error at read time.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ontology_vectors \
         (namespace TEXT NOT NULL, \
          owning_graph TEXT NOT NULL, \
          id TEXT NOT NULL, \
          element_type TEXT NOT NULL, \
          metadata TEXT, \
          embedding BLOB NOT NULL)",
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
            "INSERT INTO ontology_vectors \
             (namespace, owning_graph, id, element_type, metadata, embedding) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(ONTOLOGY_VECTOR_NAMESPACE)
        .bind(&input.owning_graph)
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

    // Record the layout generation, so a future build can tell at a glance whether
    // this store's shape is one it still writes.
    write_schema_version(db_path).await?;

    // Record the fingerprint last: a store is only advertised as fresh once its
    // rows and model stamp are fully written.
    std::fs::write(&fp_path, &fingerprint)
        .map_err(|e| anyhow::anyhow!("write fingerprint {}: {e}", fp_path.display()))?;

    tracing::info!(
        "[vectors] built ontology vector store: {} vectors at {}",
        inputs.len(),
        db_path.display()
    );

    VecStore::open(None, Some((db_path, ONTOLOGY_VECTOR_SCOPE)))
        .await
        .map_err(|e| anyhow::anyhow!("open vector store: {e}"))
}

/// Stamp the layout generation this producer just wrote into `store_meta`.
/// `VecStore::write_stamp` has already created that table by the time this runs,
/// but the `IF NOT EXISTS` keeps the two independent.
async fn write_schema_version(db_path: &Path) -> anyhow::Result<()> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .map_err(|e| anyhow::anyhow!("vector db path {}: {e}", db_path.display()))?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("open vector db to stamp schema version: {e}"))?;
    sqlx::query("CREATE TABLE IF NOT EXISTS store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("create store_meta: {e}"))?;
    sqlx::query(
        "INSERT INTO store_meta(key, value) VALUES(?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(MOOSEDEV_VECTOR_SCHEMA_KEY)
    .bind(MOOSEDEV_VECTOR_SCHEMA)
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("stamp schema version: {e}"))?;
    pool.close().await;
    Ok(())
}

/// Read this producer's layout stamp from a persisted store. Anything that stops
/// us reaching it — no file, no `store_meta`, no key — reads as `None`, which the
/// caller treats exactly like a mismatch and rebuilds. There is deliberately no
/// error path: an unreadable stamp and a wrong stamp warrant the same response.
async fn read_schema_version(db_path: &Path) -> Option<String> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .ok()?
        .create_if_missing(false)
        .read_only(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.ok()?;
    let found: Option<(String,)> = sqlx::query_as("SELECT value FROM store_meta WHERE key = ?")
        .bind(MOOSEDEV_VECTOR_SCHEMA_KEY)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();
    pool.close().await;
    found.map(|(value,)| value)
}

/// Try to reuse a persisted store: returns the opened store iff the fingerprint
/// sidecar matches `fingerprint` and it opens cleanly with vectors. `open`
/// validates the embedding-model stamp against the compiled-in active model, so a
/// model change (or a corrupt/empty store) returns `None` and the caller rebuilds.
async fn try_reuse(db_path: &Path, fp_path: &Path, fingerprint: &str) -> Option<VecStore> {
    if !db_path.exists() || std::fs::read_to_string(fp_path).ok().as_deref() != Some(fingerprint) {
        return None;
    }
    // Layout gate, before the content gate. A store written by a different
    // generation of this producer is regenerated wholesale rather than migrated:
    // the rows are derived, so a rebuild is total and lossless however many
    // generations were skipped, in either direction.
    match read_schema_version(db_path).await {
        Some(found) if found == MOOSEDEV_VECTOR_SCHEMA => {}
        found => {
            tracing::info!(
                "[vectors] cached store is schema {}, this build writes {}; rebuilding",
                found.as_deref().unwrap_or("<unstamped>"),
                MOOSEDEV_VECTOR_SCHEMA
            );
            return None;
        }
    }
    match VecStore::open(None, Some((db_path, ONTOLOGY_VECTOR_SCOPE))).await {
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
    let mut seen: HashSet<(&'static str, String)> = HashSet::new();
    for graph_iri in domain_graph_iris {
        let vocab = extract_compact_vocabulary(store, graph_iri, None)
            .map_err(|e| anyhow::anyhow!("extract_compact_vocabulary({graph_iri}): {e:?}"))?;
        for (entries, kind) in [
            (&vocab.classes, ElementType::Class),
            (&vocab.datatype_properties, ElementType::DatatypeProperty),
        ] {
            for entry in entries {
                // MOOSE refuses a store in which two loaded rows share an
                // (element_type, iri), so a term re-declared across graphs has to
                // be resolved here rather than at open. First declaration wins,
                // which makes the winner a function of the caller's graph order.
                if !seen.insert((kind.as_db_value(), entry.iri.clone())) {
                    tracing::warn!(
                        "[vectors] {} re-declared in {graph_iri}; keeping the first declaration",
                        entry.iri
                    );
                    continue;
                }
                inputs.push(EmbedInput {
                    iri: entry.iri.clone(),
                    element_type: kind,
                    label: entry
                        .label
                        .clone()
                        .unwrap_or_else(|| entry.local_name.clone()),
                    content: embed_text(store, graph_iri, entry)?,
                    owning_graph: (*graph_iri).to_string(),
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
