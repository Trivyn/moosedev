# Lessons

## Comment New Functions During Implementation

Pattern: When adding new modules or helper functions, add concise comments as part of the first implementation pass, not as a cleanup step after review.

Rule: Public functions need doc comments. Private helpers need comments when they encode a non-obvious policy, query shape, graph assumption, or format choice.

## A Cache-Invalidation Test Fixture Must Introduce a *Real* Change

Pattern: While testing the ontology-vector cache, the "rebuild on change" test mutated the Lesson class by adding the altLabel "Takeaway" — which the shipped ontology already had. RDF set semantics made the insert a no-op, so the fingerprint correctly didn't change, and the test failed against working code.

Rule: When a test asserts that some change invalidates a cache/fingerprint, first confirm the mutation actually alters the input. For RDF, the new triple must not already exist (check the data, or use an obviously novel value). A failing invalidation test is the cache working *or* the fixture being a no-op — distinguish them before touching the implementation.

## MCP Read-After-Write Smoke Tests Must Serialize Requests

Pattern: A binary MCP smoke piped write + read requests into the server in one batch (`printf 'req1\nreq2\n' | moosedev`). The reads returned the *pre-write* state, looking like a supersede/write bug — but the integration tests (sequential Rust API) all passed.

Rule: rmcp dispatches tool calls **concurrently**; cross-call read-after-write only holds if the client awaits each response before sending the next (noted in tasks/todo.md). A single piped batch races them. For a shell smoke, drive **separate sequential process invocations** against a persistent `MOOSEDEV_DATA_DIR` (each commits on exit before the next reads) instead of one piped stream. When a binary smoke "fails" but unit/integration tests pass, suspect the harness (ordering/concurrency), not the code.

## Lifecycle Tests Must Exercise the Actual Signal Boundary

Pattern: The initial `--connect` auto-spawn test killed only the proxy PID and asserted the daemon survived. That proved ordinary child reparenting, but it did not exercise the load-bearing `process_group(0)` isolation that protects the daemon when an MCP-client shell tears down the proxy's whole process group.

Rule: When process-group/session isolation is the design requirement, the test must send a signal to the parent/proxy process group and assert the detached child still serves. Killing one PID is insufficient because it passes even when the isolation call is removed.

## Don't Index Instance Data — MOOSE Uses Walk Planning for Precision

Pattern: Proposed embedding-ranked A-box recall and embedding-based instance dedup — building a vector index over instance content. This contradicts a deliberate MOOSE design choice: MOOSE does not index instance data; walk planning was devised to avoid the vector-index/RAG trap and deliver more precise, auditable retrieval.

Rule: When recall/dedup/contradiction seems to need "semantic search" over recorded instances, reach for the symbolic path first — `query` (walk planning) and `sparql` for retrieval precision; LLM-as-sensor + typed conflict for contradiction — not a nearest-neighbor index over instances. Embeddings in MOOSEDev stay confined to T-box term alignment (`suggest_parent`) over the static ontology (class placement, not instance retrieval). `get_relevant_context` is a shallow lexical anchor/browse tool by construction; fix its imprecision with an honesty floor + tool-selection guidance, not vector search.

## Canonicalized Path Derivation Needs Precreated Directories

Pattern: `socket_path_for` canonicalizes the data dir when it exists, then applies the Unix-socket length guard. `--serve` created the data dir before deriving the socket, but `--connect` did not, so first-run and later-run derivation could diverge for paths near the macOS socket length limit.

Rule: If a derived path depends on canonicalization and existence, create the directory before deriving it in every mode that participates in the rendezvous. Tests using short local temp paths will not expose length-boundary flips.

## Recall-First Means List-All — a Topic-Scoped Empty Result ≠ Empty Graph

Pattern: During the M4 bootstrap-skill dry-run I ran `get_relevant_context` with a *topic*, got "No recorded knowledge relevant to …", concluded the graph was empty, and recorded new decisions without a list-all recall. The graph was actually well-populated (~28 records); the topic result was empty only because nothing cleared the relevance floor (`TOPIC_RELEVANCE_FLOOR`) for that phrasing. Two of the three records I added duplicated existing knowledge (the M5 shared-backend decision; the "don't index instance data" lesson, already covered by a Lesson + two Constraints). This is the exact failure the bootstrap skill's own Phase 0 is designed to prevent — and I wrote the skill, then skipped its first step.

Rule: "Recall-first" means **list-all** (`get_relevant_context` with NO topic) before recording — not a topic-scoped probe. A topic-scoped empty result means nothing cleared the relevance floor for that wording; it is NOT evidence the graph is empty. When a write tool offers no retract (MOOSEDev has none yet — it's an open Requirement), a careless duplicate is permanent, so the bar before `record_important_decision` is: list-all, confirm it's genuinely new, then write. For a verification that only needs a round-trip, reuse an existing (or the single just-recorded) item instead of minting new permanent records.
