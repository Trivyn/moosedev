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
- [ ] Minimal `ontologies/architecture.ttl` (ArchitecturalDecision, Lesson, Constraint, AntiPattern)
- [ ] `graph/` writer: validate → mint IRI → insert typed quads → `invalidate_graph`/`invalidate_all`
- [ ] `record_important_decision` tool
- [ ] `LlmClient` (OpenAI-compatible, env-config) + `OntologyResolver` impls
- [ ] `query` tool → `execute_graph_walk_nlq` (answer + trace)
- [ ] `get_relevant_context` tool
- [ ] Verify (key E2E): record a decision → restart → query it back with a trace

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
- [ ] File Ask 1 (invalidate `label_sets`) — unblocks complete write coherence
- [ ] Track Ask 2 (cache-coherent assert/curate primitive) against M1 write-path learnings
