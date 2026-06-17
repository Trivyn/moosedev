# MOOSEDev v1 — Build Checklist

> Plan of record: `spec/MOOSEDev_design.md`. Upstream asks: `spec/core-moose-asks.md`.

## M0 — Crate skeleton
- [x] `moosedev` crate (edition 2021); `moose = { path = "../moose", features = ["candle-cpu","arctic-s"] }` — links + runs (`src/main.rs` smoke)
- [x] Replicate MOOSE `[patch.crates-io]` oxigraph-trivyn block; **builds offline** (fork cached at rev `92f650d`, oxigraph 0.5.7)
- [x] Confirm persistent `Store::open(path)` on the Trivyn fork — rocksdb backend works; persists across reopen (`tests/m0_integration.rs`). N-Quads fallback **not needed**.
- [x] `moose::initialize` works from moosedev (resolves ttl via moose's `CARGO_MANIFEST_DIR`); pipeline cache builds (stages non-empty)
- [x] Add `tokio` (done). `EngineConfig` + `LlmClient`/`OntologyResolver` wiring → moved to **M1** (needs the trait impls).
- [x] stdio MCP server (`rmcp` 1.7) with a `ping` tool (`src/mcp/mod.rs`)
- [x] Verify: MCP handshake end-to-end — `initialize` (proto `2025-06-18`, serverInfo `moosedev/0.1.0`) → `tools/list` (ping registered) → `tools/call ping` → `pong`. Logs to stderr, stdout clean.
- Note: outside contributors can't build from source (fork is SSH/private + `url.insteadOf` rewrite); binaries are their route — matches `spec/MOOSEDev_design.md` §5.

**M0 complete** ✅ — foundational integration, persistence, and MCP transport all proven.

## M1 — Capture + Query vertical slice
- [x] Ontology generation brief for Trivyn's generator (`spec/architecture-ontology-brief.md`)
- [x] Crate restructured into lib + bin (`src/lib.rs`) so modules are test-reachable
- [x] Content-agnostic ontology loader (`src/ontology/mod.rs`) + test (`tests/ontology_loader.rs`)
- [x] `ontologies/architecture.ttl` — **TEMP stub** (swapped for generator output, no code change)
- [x] App state wired into the server (persistent `Store` + `EntityIndexCache` + arch vocab) at bootstrap (`src/graph/mod.rs`)
- [x] `graph/` writer → `moose::kg::assert_instance` (transactional, cache-coherent); IRI minting + ontology-validated `kind`
- [x] `record_important_decision` tool — MCP E2E verified (records typed instance; rejects unknown kinds); test `tests/write_path.rs`
- [x] `LlmClient` (OpenAI-compatible, env-config: `src/llm/mod.rs`) + `OntologyResolver` (`src/ontology/mod.rs`)
- [x] `query` tool → `execute_graph_walk_nlq_with_context` (answer + reasoning trace); pure-symbolic test (`tests/query.rs`) + **live LLM smoke** against `endor`
- [x] `get_relevant_context` tool — symbolic structured retrieval (topic via the coherent entity index, else list-all); MCP-verified; test `tests/context.rs`
- [ ] CategoryMappings (L0 → fixed BFO/CCO IRIs) — deferred to M2 (needs the generated ontology's CCO roots)
- [x] Key E2E **verified live**: record → NLQ → synthesized answer + 10-stage trace (LLM fired only as the extraction *sensor*; symbolic pipeline did the reasoning). Persistence proven (M0).
- [x] **Record → restart → read-back** verified with the real binary across two separate processes sharing a data dir.

**M1 complete** ✅ — tools live: `ping`, `record_important_decision`, `query` (+trace), `get_relevant_context`, `get_provenance`. 6 tests green, clippy clean.
- [x] **Edit provenance** (bonus): every write emits PROV-O (`prov:Activity` + agent from MCP `clientInfo` + timestamp) into a companion `…/kg/provenance` graph; `get_provenance` reads it back. Realizes auditability (#6); prototypes core Ask-2 scope-A. `src/provenance/mod.rs`, `tests/provenance.rs`.
- Note: rmcp dispatches tool calls concurrently; cross-call read-after-write relies on the client awaiting each response (true for real MCP clients). Not a concern for v1; revisit if batched/streamed calls are added.

## M2 — Alignment ✅
- [x] Ontology vector store (`ontology_vectors` SQLite) — **built at startup** from the shipped ontologies (`src/vectors/mod.rs` + `AppState::build_alignment_index`), not a precompute script; embeds classes + datatype props via `default_backbone().embed_document()`, opened with `VecStore::open(None, Some(db))`
- [x] `embedding_store` wired into `EngineConfig` (also sharpens query-time class disambiguation); startup build is **non-fatal** (model-load failure disables only the alignment tools)
- [x] `align_concepts` + `suggest_mappings` (MCP tools) via `suggest_parent` — L1 keyword + L2 embedding (LLM tier off; symbolic-first)
- [x] Verify: live — "Design Decision" → ArchitecturalDecision (L1 exact); ambiguous concepts → ranked candidates with cosine scores. Tests: `tests/vectors.rs`, `tests/alignment.rs`
- [ ] CategoryMappings (L0 → CCO roots from `trivyn:l0Category`) — **deferred refinement** that narrows L0; alignment works without it (also the M1-deferred item)

## M3 — SPARQL + Validation
- [ ] `sparql` tool wrapping `oxigraph::sparql::SparqlEvaluator`
- [ ] `validate_against_architecture` (SHACL over the durable KG)
- [ ] **Populate shape-required fields on capture** (P2): `hasAuthor` + `hasTimestamp` (typed `xsd:dateTime` — needs a `PropertyLiteral::Typed` on `RecordInput`) + default `hasLifecycleStatus`; reconcile with the PROV-O who/when (provenance-only vs. also domain-typed). Belongs here because nothing validates instances until this tool exists (`assert_instance` runs with `validator: None`, so shapes are schema-info today, not a write gate)
- [ ] Verify: SPARQL returns recorded instances; malformed decision fails validation

## M4 — Bootstrap + Focus
- [ ] `skills/bootstrap-existing-codebase.md`
- [ ] `get_focus_stack` via `SessionDb`
- [ ] Verify: bootstrap a sample repo → typed knowledge populated + queryable

## Stretch
- [ ] Read-only local web UI (focus stack + recorded decisions)

## Known limitations / deferred
- **Ontology-regeneration orphans existing records** (P1): instances carry the full class IRI as `rdf:type`; if a regenerated ontology changes class IRIs/namespace, prior durable records stop being listed/searched (`relevant_context`/`query` candidate sets come from the current `arch_vocab`). **Zero impact today** (no persistent data), but needs a deliberate **migration story** (re-type on ontology change via a mapping — *not* hardcoded old namespaces) before real data accrues. Lighter partial step: make list-all enumerate by actual `rdf:type` in the project graph rather than the current vocab.
- **Per-class title predicate**: capture binds the title to `hasTitle` (the `InformationRecord` label property every capture class inherits). The class-generic form (read each class's `trivyn:labelProperty`) is blocked until MOOSE surfaces that annotation (`VocabularyEntry.label_property`).

## Core-MOOSE coordination
- [x] Ask 1 — **landed in MOOSE**: `invalidate_graph`/`invalidate_all` now clear `label_sets`/`label_order` (+ global/per-graph epoch coherence)
- [x] Ask 2 (minimal) — **landed** as `moose::kg::assert_instance` (transactional write + epoch-based cache coherence + `AssertionValidator` hook); MOOSEDev wires to it
- [~] Ask 2 (scope A): **provenance prototyped in MOOSEDev** (`src/provenance/mod.rs` — PROV-O on write, agent from `clientInfo`) — informs the eventual core `ProvenanceWriter` generalization. Still deferred in core: retract/supersede lifecycle, the generalized hook, incremental index maintenance.
