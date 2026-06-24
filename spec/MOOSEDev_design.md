# MOOSEDev v1 — Design of Record

**Status:** design approved to build · **Companion:** `spec/MOOSEDev_spec.md` (scope), `spec/core-moose-asks.md` (upstream asks)

## 1. Intent

MOOSEDev gives coding agents structured, queryable, long-term project memory to combat comprehension
debt, and is the first host application on the MOOSE engine — the proving ground for a family of
MOOSE-based tools. This document fixes the MOOSE/MOOSEDev boundary against verified ground truth,
resolves the design forks, and sequences the build.

## 2. Verified ground truth — what MOOSE provides

MOOSE is a single, **library-only** Rust crate (`moose`, proprietary license; no server/CLI/daemon),
**read-optimized**: load an oxigraph `Store` → build derived caches → answer queries; plus an
alignment engine and conversational salience. Public seams MOOSEDev composes:

- **Init** — `moose::initialize(&store) -> Arc<MooseOntologyCache>`.
- **Query + trace** — `execute_graph_walk_nlq[_with_context](...)` (`pipeline/mod.rs`) → answer +
  `PipelineTimings` (per-stage trace, LLM sensors fired/blocked, walk strategy).
- **Chat + focus stack** — `chat_pipeline(...) -> ChatResponse { moose.session_map: Vec<FocusEntry> }`,
  persisted in `SessionDb` (SQLite).
- **Alignment** — `alignment::suggest_parent` / `align_batch` (L1 keyword → L2 embedding RRF → L3
  constrained LLM) → `Resolved/Undecided/Unavailable` + rationale. Inputs all publicly constructible;
  `AlignmentConfig: Default`.
- **Vocabulary** — `extract_compact_vocabulary(store, graph_iri, mapping)` (public).
- **Embeddings** — `default_backbone()` (Candle; requires a `candle-*` feature); `VecStore::open(db)`
  reads precomputed vectors.
- **Trait seams MOOSEDev implements** — `LlmClient`, `OntologyResolver`. **Configs it builds** —
  `EngineConfig`, `ChatConfig`.

What MOOSE does **not** provide → MOOSEDev builds: the MCP server; the typed-instance **write path**;
the durable Store lifecycle/persistence; a public SPARQL endpoint; the trait impls; and embedding the
domain ontology (handled by shipping precomputed vectors). Eventual core moves: see
`spec/core-moose-asks.md`.

## 3. Resolved decisions

1. **Durable project KG is the source of truth** — persistent oxigraph named graphs of typed instances
   are canonical (invariants #2, #5). MOOSE's focus stack is an optional within-session continuity
   layer. The spec's `search_session_graph` runs over the durable KG (MOOSE's internal `session_graph`
   is `pub(crate)`/unreachable — name honored, semantics = "query the project KG").
2. **Vertical slice first** — prove the MOOSE seam end-to-end early, then widen.
3. **Optional, local-first OpenAI-compatible `LlmClient`** — `MOOSEDEV_LLM_BASE_URL` explicitly
   opts into assisted LLM sensors; without it, MOOSEDev pins assistance to `PureSymbolic` so core
   memory tools stay local and symbolic by default (invariants #1, #9); cloud remains opt-in.
4. **Bundle `candle-cpu` + `arctic-s`; ship precomputed ontology vectors** — alignment runs L1+L2+L3
   out of the box. The runtime backbone is still required to embed dynamic query/leaf text.
5. **Prototype the write primitive in MOOSEDev, promote to core later** — v1 writes via `store.insert`
   + `invalidate_graph`/`invalidate_all`; relies on core Ask 1 for complete coherence; discovers the
   shape for core Ask 2.

## 4. Architecture & ownership

| Compose from MOOSE (as-is) | Build in MOOSEDev (net-new) |
|---|---|
| NLQ query + execution trace | The MCP server (stdio JSON-RPC) |
| Chat pipeline + focus stack + `SessionDb` | Typed-instance **write path** (validate→mint→insert→invalidate→provenance) |
| Alignment engine (`suggest_parent`/`align_batch`) | Durable oxigraph `Store` lifecycle + on-disk persistence |
| `extract_compact_vocabulary`, embeddings backbone | Public **SPARQL** endpoint (wrap `oxigraph::sparql::SparqlEvaluator`) |
| Execution traces (RDF `moose:QueryExecution`) | `LlmClient` + `OntologyResolver` impls; `CategoryMappings` |
| `EngineConfig`/`ChatConfig` | The architecture ontology + precomputed vectors; bootstrap SKILL |

## 5. Crate & build integration

- New crate **`moosedev`** (edition 2021, `tokio`). Dependency:
  `moose = { path = "../moose", features = ["candle-cpu", "arctic-s"] }`.
- **Replicate MOOSE's entire `[patch.crates-io]` block** (the 12 oxigraph-trivyn crates,
  `moose/Cargo.toml:57-72`) — required because `oxigraph::store::Store` crosses the public API
  boundary and must be the same type. ⚠️ **Open-source build caveat:** needs access to
  `github.com/Trivyn/oxigraph-trivyn`; binary releases sidestep it (matches spec §7 distribution).
- **MCP transport:** official Rust MCP SDK (`rmcp`) over **stdio** (Claude Code/Desktop). Fallback:
  hand-rolled stdio JSON-RPC.
- **Persistence:** persistent `Store::open(path)` so the KG survives restarts. ⚠️ Verify the Trivyn
  fork keeps the on-disk (rocksdb) backend; fallback = serialize/load N-Quads on shutdown/startup.

## 6. Module layout

```
src/
  main.rs            # bootstrap: open Store → moose::initialize → load ontologies + VecStore
                     #            → build EntityIndexCache → EngineConfig + wire traits → start MCP
  config.rs          # env-driven configuration
  mcp/
    server.rs        # rmcp server, tool registry + dispatch
    tools/{memory,alignment,query,validation}.rs   # the 8 v1 tools
  graph/             # durable KG: Store lifecycle, named-graph conventions, IRI minting,
                     #   typed-instance WRITER (+ cache invalidation), SPARQL wrapper
  ontology/          # load *.ttl; extract_compact_vocabulary; load precomputed VecStore;
                     #   CategoryMappings; OntologyResolver impl
  llm/               # OpenAI-compatible LlmClient
ontologies/{software-engineering,architecture}.ttl
skills/bootstrap-existing-codebase.md
scripts/precompute-vectors.rs    # release-time: embed ontologies → ontology_vectors SQLite
```

## 7. Tool → MOOSE backing

| Tool | Backing |
|---|---|
| `get_relevant_context` | NLQ retrieval over the durable KG (`execute_graph_walk_nlq`) scoped to current focus |
| `get_focus_stack` | read `SessionContext.focus_stack` via `SessionDb` / `ChatResponse.session_map` |
| `search_session_graph` | NLQ / SPARQL over the durable KG |
| `record_important_decision` | **MOOSEDev write path**: validate → mint IRI → insert typed quads → invalidate cache → (opt) align → (opt) provenance |
| `align_concepts` | `extract_compact_vocabulary(architecture.ttl)` + precomputed `VecStore` + `suggest_parent`/`align_batch` |
| `suggest_mappings` | same machinery, surface `Undecided.top_candidates` |
| `query` | `execute_graph_walk_nlq[_with_context]` → answer + trace (`PipelineTimings` / session-graph `QueryExecution`) |
| `validate_against_architecture` | SHACL over the durable KG (consistency of recorded knowledge — not code scanning) |

## 8. Architecture ontology

`ontologies/architecture.ttl` + `software-engineering.ttl` define typed classes —
`ArchitecturalDecision`, `Lesson`, `Constraint`, `AntiPattern`, `Component`, `Rationale`, … — with
**upper-tier roots so `CategoryMappings` can bind MOOSE's neutral L0 categories** (BFO 2020 / CCO 2.0
alignment recommended, to match MOOSE's `MOOSE-Pipeline.ttl` conventions). Vectors are precomputed at
release time by `scripts/precompute-vectors` into the `ontology_vectors` SQLite schema
(`moose/src/embeddings/vec_store.rs:16`), loaded via `VecStore::open`.

## 9. Milestones & verification

- **M0 Skeleton** — crate builds against `moose`; oxigraph patch in place; persistent Store +
  `moose::initialize`; stdio MCP server with a ping tool. *Verify:* MCP client handshake; store
  persists across restart.
- **M1 Capture+Query slice** — minimal `architecture.ttl`; `record_important_decision`; `query`
  (NLQ+trace); `get_relevant_context`. *Verify (key E2E):* record a decision → **restart** → query it
  back **with a reasoning trace**.
- **M2 Alignment** — precompute VecStore; `align_concepts`/`suggest_mappings`. *Verify:* a new concept
  aligns under an existing class with rationale; an ambiguous one returns ranked candidates.
- **M3 SPARQL+Validation** — `sparql` tool; `validate_against_architecture` (SHACL). *Verify:* SPARQL
  returns recorded instances; a malformed decision fails validation with a clear report.
- **M4 Bootstrap+Focus** — `skills/bootstrap-existing-codebase.md`; `get_focus_stack`. *Verify:* run
  bootstrap on a sample repo → typed knowledge populated and queryable.
- **Stretch** — read-only local web UI over the focus stack + recorded decisions.

Each milestone ships Rust integration tests driving the tools against a temp persistent store, plus an
end-to-end check via an MCP client (point Claude Code/Desktop at the binary).

## 10. Out of scope (v1)

Per spec §2: full ontology generation, heavy code synthesis, language-specific ontologies, and
public/authenticated endpoints.
