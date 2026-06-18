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
- [x] `sparql` tool wrapping `oxigraph::sparql::SparqlEvaluator`
- [x] `validate_against_architecture` (focused shape-driven validation over the durable KG: `sh:minCount 1` + `sh:datatype`; unsupported constraints counted)
- [x] **Populate shape-required fields on capture** (P2): `record_instance` stamps `hasAuthor`, typed `hasTimestamp`, and default `hasLifecycleStatus = "proposed"` when absent; MCP write path threads one timestamp to both domain field and PROV-O.
- [x] Verify: SPARQL returns recorded instances; normal captures validate; malformed raw decisions fail validation (`tests/sparql.rs`, `tests/validation.rs`)

**M3 complete** ✅ — tools live: `sparql`, `validate_against_architecture`; recorded instances conform to required `InformationRecord` fields by construction. Verification: `cargo check`, `cargo test`.

## M4 — Bootstrap + Focus
- [ ] `skills/bootstrap-existing-codebase.md`
- [ ] `get_focus_stack` via `SessionDb`
- [ ] Verify: bootstrap a sample repo → typed knowledge populated + queryable

## M5 — Concurrent multi-client access (shared backend) — CORE
> Goal: multiple MCP clients (Claude Code, Codex) share one live KG **concurrently** by
> talking to a single backend process that owns the RocksDB store. Build the core first
> (manual lifecycle, explicit `--serve`/`--connect`); auto-spawn daemon polish is deferred.
> Replaces the per-client stdio-subprocess model for shared use (see line-62 limitation).

- [x] CLI dispatch in `main.rs` (hand-rolled, no new dep): default = **stdio** (unchanged — backward compatible); `--serve [SOCKET]` = backend; `--connect [SOCKET]` = stdio↔socket proxy; `--help`.
- [x] Socket path derivation (`runtime::socket_path_for`): default `<MOOSEDEV_DATA_DIR>/moosedev.sock`; length-guard fallback to a hashed path under the temp dir (macOS `sun_path` ~104-char cap). **One socket per data dir → multi-project isolation by construction** (verified by the isolation test).
- [x] Refactor: extracted `runtime::build_server(data_dir, ontology_dir) -> MooseDevServer` (bootstrap + alignment index) shared by stdio + serve modes.
- [x] Serve mode (`runtime::serve_unix`): remove stale socket → `UnixListener::bind` → `select!`(accept → per-conn `server.clone().serve(stream)` task | `ctrl_c` → remove socket + exit). Shared `Arc<AppState>`; writes serialized by RocksDB txn (`EntityIndexCache` is `Send+Sync`). **Probe moved to `main`** (`ensure_no_live_backend`) — it must run *before* `build_server`, else a same-data-dir conflict dies on the RocksDB lock (after a wasted model load) instead of giving the friendly "already listening" message.
- [x] Connect mode (`runtime::connect_unix`): `UnixStream::connect` → transparent **bidirectional byte relay** via `tokio::io::copy_bidirectional(join(stdin,stdout), socket)` (no MCP parsing); half-closes on EOF. Diagnostics to stderr only. Never opens the store / loads the model.
- [x] Tests (`tests/concurrent_clients.rs`, rmcp `client` dev-dep): in-process serve on a temp socket + **two concurrent rmcp clients** → both handshake, both `record` concurrently, each cross-reads the other's write. Multi-project isolation (two data dirs → two distinct sockets, no cross-talk). Full suite green, clippy clean.
- [x] Verify (real binary smoke, 7/7): stdio regression (initialize→ping); two concurrent `--connect` clients; second `--serve` refuses with the friendly message; cross-process read-back via a fresh `--connect`. **Remaining for the user:** wire Claude + Codex configs to `--connect` against one manual `--serve` (see README "Shared mode").
- [x] Docs: README "Shared mode (multiple clients / agents)" + config env vars; line-62 limitation note updated.

Deferred to a later infra pass (auto-spawn daemon): detached spawn, idle-shutdown refcount, version handshake to restart a stale/outdated daemon, `--status`/`--stop`, daemon log file, and socket cleanup on `SIGTERM` (today only `SIGINT`/ctrl_c removes the socket; a stale file is cleared on next `--serve`).

**M5 core complete** ✅ — multiple MCP clients share one live KG concurrently via a single Unix-socket backend (`moosedev --serve`) + thin `--connect` proxies. Decision recorded in the graph (`ArchitecturalDecision/0d81e237…`).

## Vector store startup caching
> Problem: `build_alignment_index` → `vectors::build_and_open` **unconditionally** drops and
> re-embeds the whole ontology vector store on every backend startup (loads the embedding backbone
> + N inferences), even though the shipped ontology only changes on a version bump.

- [x] Make the rebuild conditional on a freshness key, reusing a persisted store when nothing changed:
  - [x] Collect the embed inputs once (`(iri, element_type, label, embed-text)`) and use them for both the build and the fingerprint, so the cache key can't drift from what's embedded.
  - [x] Persist an ontology **content fingerprint** sidecar next to the DB; fast-path reuse when it matches AND `VecStore::open` succeeds (`open` validates the model stamp against the compiled-in `model_id::ACTIVE` — model drift falls through to a rebuild). No backbone load on a cache hit.
  - [x] Clear the sidecar on rebuild; write it only after a successful stamp (crash-safe).
- [x] Tests (`tests/vectors.rs`): unchanged ontology ⇒ DB file untouched (cache hit); changed ontology ⇒ class re-embedded (rebuild).
- [x] Verify: `cargo test` (full suite green), `cargo clippy` (clean), and **real-binary smoke**: two startups on one data dir → cold "built … 36 vectors" (~10s) then warm "reusing cached … (ontology + model unchanged)" (~16ms).

> Future need (your point b): a **cache hit only proves the T-box index is fresh** — it does nothing
> for existing A-box instances. A breaking ontology change (renamed/removed class, namespace shift)
> still orphans recorded data. That migration story is tracked under "Known limitations" P1 below; this
> caching is forward-compatible with it (an ontology change flips the fingerprint ⇒ rebuild).

**Vector store caching complete** ✅ — startup reuses `ontology-vectors.db` unless the ontology content
fingerprint or embedding model changes. Decisions recorded in the graph (`ArchitecturalDecision/43446e69…`,
future-need `Requirement/74423463…`); graph validates (0 violations).

## Supersede-with-rationale lifecycle + object-property capture
> Spec: `spec/supersede-and-relations.md`. Plan: `~/.claude/plans/cryptic-forging-finch.md`.
> When a decision changes, keep the old one as history, link the new one, and capture WHY — using
> ontology terms that already exist (`supersedes`, `hasRationale`→`Rationale`, lifecycle statuses).
> **No ontology changes** (confirmed with the user's "pause if ontology changes" condition).

- [x] **Phase 1 — object-property plumbing:** non-breaking `graph::record_instance_with_relations`
  writes IRI-valued relations via `moose::kg::ObjectAssertion` (was always `&[]`); `record_instance`
  delegates with no relations. `object_property_iri` + `AppState::resolve_object_property` resolve
  relations by local name. (RecordInput left unchanged → zero churn to the 8 literal call sites.)
- [x] **Phase 2 — supersede:** `graph::supersede_decision` (atomic — one oxigraph transaction inserts
  the new decision + a `Rationale` node + `supersedes`/`hasRationale` edges, removes the old status
  quads, inserts `superseded`, then `invalidate_graph`). Precondition: target must be an
  `ArchitecturalDecision` (else errors, writes nothing). New MCP tool `supersede_decision`. Old record
  preserved; only its lifecycle status flips.
- [x] **Phase 3 — read path:** `relevant_context` gains `include_history` (default false → hides
  superseded/deprecated); items surface the `supersedes` link, the dereferenced rationale **text**, and
  a `supersededBy` back-link. `get_relevant_context` MCP arg `include_history`.
- [x] Tests `tests/supersede.rs` (4): links+preserve+conforms; precondition rejects + no partial write;
  read-path filtering + chain rendering; `record_instance_with_relations` writes edges. Clippy clean.
- [x] Real-binary MCP smoke (record → supersede → context default/​history): all checks pass —
  tool registered; default view hides the superseded decision and surfaces the new one with its
  rationale text + supersedes link; history view includes the old with a `supersededBy` back-link.
  (Drive sequential **process** invocations, not piped concurrent calls — rmcp dispatches tool calls
  concurrently, so a single piped batch races the write; see lessons.md.)

**Supersede feature complete** ✅ — full suite green (incl. `tests/supersede.rs` 4/4), clippy clean,
real-binary MCP smoke passes. Decisions recorded: `Requirement/b2f8240c` (need),
`ArchitecturalDecision/f6ac8e23` (implementation). No ontology changes.

## Stretch
- [ ] Read-only local web UI (focus stack + recorded decisions)

## Repo-local agent MCP config
> Move the MOOSEDev dogfood MCP wiring out of global client config and into this
> repository so Codex and Claude attach to this repo's shared backend only here.

- [x] Add project-scoped Codex MCP config at `.codex/config.toml`.
- [x] Add project-scoped Claude MCP config at `.mcp.json`.
- [x] Remove the global Codex `mcp_servers.moosedev` block after local config is in place.
- [x] Remove Claude's user-level project `mcpServers.moosedev` entry from `~/.claude.json`.
- [x] Verify config files parse and the user-level Codex/Claude configs no longer contain MOOSEDev MCP wiring.

Review: repo-local config now owns the MOOSEDev MCP wiring for both clients. `claude mcp list`
shows `moosedev` from `./target/release/moosedev --connect` as pending repo approval, and
`codex mcp list` shows `moosedev` from the same relative command. The shared backend remains
manual lifecycle: start `MOOSEDEV_DATA_DIR=./.moosedev ./target/release/moosedev --serve`.

## Root backend start script
> Provide a repo-root helper that starts the shared backend with the repository-local
> data/ontology paths and explicit LLM configuration.

- [x] Add `start-moosedev.sh`.
- [x] Default `MOOSEDEV_LLM_BASE_URL` to `http://localhost:1234/v1` and `MOOSEDEV_LLM_API_KEY` to `lm-studio`.
- [x] Require `MOOSEDEV_LLM_MODEL` so the script cannot silently use the stale built-in default.

Review: run with `MOOSEDEV_LLM_MODEL=<loaded-model-id> ./start-moosedev.sh`. The script resolves paths
from its own location, exports the environment expected by MOOSEDev, validates the release binary,
and execs `target/release/moosedev --serve`.

## --connect auto-spawn backend
> Make per-client `--connect` proxies auto-start the matching detached `--serve`
> backend when no daemon is listening on the resolved per-data-dir socket.

- [x] Add runtime helpers for serve log path, pidfile path, detached backend spawn,
  connect retry, and `MOOSEDEV_NO_AUTOSPAWN`.
- [x] Make `connect_unix` use auto-spawn with the resolved data dir while preserving
  the byte relay and stdout cleanliness.
- [x] Add SIGTERM handling to `serve_unix` so detached backends remove their socket on
  clean shutdown.
- [x] Wire `main.rs` for connect data-dir passing, serve pidfile lifecycle, startup
  logging, and updated usage text.
- [x] Add integration tests for default-on auto-spawn and opt-out behavior.
- [x] Verify with formatting, targeted autospawn tests, full tests/checks as feasible.

Review: `--connect` creates the data dir before deriving the socket, then first tries the
resolved socket, auto-spawns the same binary as `--serve <resolved-socket>` when the socket is
absent/stale, redirects daemon stdio to `<data_dir>/moosedev-serve.log`, and retries for up to
30s. `--serve` writes `<data_dir>/moosedev-serve.pid`, logs resolved paths at startup, removes
the pidfile after clean shutdown, and handles SIGTERM through the same socket cleanup path as
ctrl-c. `MOOSEDEV_NO_AUTOSPAWN=1` preserves the old fail-fast behavior. The autospawn test now
signals the proxy process group and verifies the detached backend survives, so it covers the
load-bearing `.process_group(0)` isolation. Verification passed: `cargo fmt`,
`cargo test --test autospawn`, `cargo check`, `cargo clippy --all-targets --all-features`, and
full `cargo test`.

## Known limitations / deferred
- **Ontology-regeneration orphans existing records** (P1): instances carry the full class IRI as `rdf:type`; if a regenerated ontology changes class IRIs/namespace, prior durable records stop being listed/searched (`relevant_context`/`query` candidate sets come from the current `arch_vocab`). **Zero impact today** (no persistent data), but needs a deliberate **migration story** (re-type on ontology change via a mapping — *not* hardcoded old namespaces) before real data accrues. Lighter partial step: make list-all enumerate by actual `rdf:type` in the project graph rather than the current vocab.
- **Per-class title predicate**: capture binds the title to `hasTitle` (the `InformationRecord` label property every capture class inherits). The class-generic form (read each class's `trivyn:labelProperty`) is blocked until MOOSE surfaces that annotation (`VocabularyEntry.label_property`).
- **Concurrent multi-agent access** — ✅ **addressed by M5 core** (was: a future goal). The durable KG is RocksDB-backed (single-writer exclusive lock); in the default per-client stdio model each MCP client spawned its own server, so the second (e.g. codex beside Claude Code) failed to open the locked store on startup ("handshake fail"). Resolved by a single shared backend (`moosedev --serve`, Unix-socket MCP) that owns the store, with clients as thin `--connect` proxies — multiple agents now read+write one live graph **concurrently** (per-data-dir socket keeps projects isolated). **Remaining (deferred infra):** transparent auto-spawn daemon so users don't run `--serve` manually (detached spawn, idle shutdown, version handshake, `--status`/`--stop`). Originally surfaced while wiring the Claude Code + codex dogfood against a shared `.moosedev/`.

## Core-MOOSE coordination
- [x] Ask 1 — **landed in MOOSE**: `invalidate_graph`/`invalidate_all` now clear `label_sets`/`label_order` (+ global/per-graph epoch coherence)
- [x] Ask 2 (minimal) — **landed** as `moose::kg::assert_instance` (transactional write + epoch-based cache coherence + `AssertionValidator` hook); MOOSEDev wires to it
- [~] Ask 2 (scope A): **provenance prototyped in MOOSEDev** (`src/provenance/mod.rs` — PROV-O on write, agent from `clientInfo`) — informs the eventual core `ProvenanceWriter` generalization. Still deferred in core: retract/supersede lifecycle, the generalized hook, incremental index maintenance.
