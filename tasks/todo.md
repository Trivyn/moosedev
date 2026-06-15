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
- [ ] `get_relevant_context` tool ← **next (finishes M1)**
- [ ] CategoryMappings (L0 → fixed BFO/CCO IRIs) — deferred to M2 (needs the generated ontology's CCO roots)
- [x] Key E2E **verified live**: record → NLQ → synthesized answer + 10-stage trace (LLM fired only as the extraction *sensor*; symbolic pipeline did the reasoning). Persistence proven (M0).

## M2 — Alignment
- [ ] `scripts/precompute-vectors` → `ontology_vectors` SQLite for shipped ontologies
- [ ] Load `VecStore::open`; build `CompactVocabulary` + `CategoryMappings`
- [ ] `align_concepts` + `suggest_mappings` via `suggest_parent`/`align_batch`
- [ ] Verify: new concept aligns under existing class w/ rationale; ambiguous → ranked candidates

## M3 — SPARQL + Validation
- [ ] `sparql` tool wrapping `oxigraph::sparql::SparqlEvaluator`
- [ ] `validate_against_architecture` (SHACL over the durable KG)
- [ ] Verify: SPARQL returns recorded instances; malformed decision fails validation

## M4 — Bootstrap + Focus
- [ ] `skills/bootstrap-existing-codebase.md`
- [ ] `get_focus_stack` via `SessionDb`
- [ ] Verify: bootstrap a sample repo → typed knowledge populated + queryable

## Stretch
- [ ] Read-only local web UI (focus stack + recorded decisions)

## Core-MOOSE coordination
- [x] Ask 1 — **landed in MOOSE**: `invalidate_graph`/`invalidate_all` now clear `label_sets`/`label_order` (+ global/per-graph epoch coherence)
- [x] Ask 2 (minimal) — **landed** as `moose::kg::assert_instance` (transactional write + epoch-based cache coherence + `AssertionValidator` hook); MOOSEDev wires to it
- [ ] Ask 2 (scope A, deferred): retract/supersede lifecycle, `ProvenanceWriter` generalization, incremental index maintenance
