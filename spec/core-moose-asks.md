# Core MOOSE Asks (surfaced by MOOSEDev)

MOOSEDev is the first host application built on MOOSE and the proving ground for the planned family
of MOOSE-based tools. This document records capabilities that are (a) needed by MOOSEDev, (b) likely
needed by every future MOOSE host, and (c) engine-shaped enough to belong in **core MOOSE** rather
than being re-implemented per tool. Each ask cites source evidence so the MOOSE team can evaluate it
directly. (Home: kept here in MOOSEDev as the proving-ground's record; items can be copied into
MOOSE's own backlog.)

**Status:** 🔴 immediate / small · 🟡 eventual / larger · ⚪ deferred (not needed for MOOSEDev v1)

## Background — MOOSE is read-optimized; the family is write-heavy

MOOSE loads a graph, builds derived caches (vocabulary, entity index, ontology vectors), and answers
queries. It has **no general public write path**: the only store-mutating public API is the
clarification accept/review flow (`clarification::accept` → private `write_overlay_quads`,
`clarification/mod.rs:1263`), which writes only `skos:altLabel`/`hiddenLabel`/`definition` with
**string-literal objects** and is welded to the chat/session lifecycle. Every tool in the family
exists to *accumulate* structured knowledge at runtime, so each needs a typed write path.
Re-implementing it per tool invites exactly the drift MOOSE's alignment subsystem exists to prevent.

## Ask 1 — Complete cache invalidation for `label_sets` 🔴

**Problem.** `EntityIndexCache` is build-once and reused; the read path returns the cached index
without consulting the store (`entity_index.rs:774`), keyed on `class_iri` + graph-set with **no
content version**. The cache is *designed* to be "invalidated on data changes" (`entity_index.rs:7`),
and two public methods exist — `invalidate_graph(g)` and `invalidate_all()` (`entity_index.rs:1192`,
`1203`). **But both clear only `indexes`/`access_order`; neither touches `label_sets`/`label_order`** —
the per-graph label gazetteer (`entity_index.rs:37`) that the keyword matcher's positive-signal gate
consults. No public API evicts that gazetteer.

**Impact.** A host that writes to a graph and calls `invalidate_graph` still serves a **stale
gazetteer**: a newly written entity's label can fail the matcher gate until LRU eviction or restart.
Full read-after-write coherence is not achievable from the public API today.

**Proposed fix.** Extend `invalidate_graph(g)` and `invalidate_all()` to also evict matching entries
from `label_sets`/`label_order` (and any future per-graph cache). ~5–10 lines; pure cache eviction,
low risk.

**Why core / why now.** Any MOOSE-hosted tool that mutates the store needs complete coherence. This
unblocks MOOSEDev to ship correct writes via the existing public API in the interim, independent of
Ask 2.

## Ask 2 — A cache-coherent knowledge-assertion / curation primitive 🟡

**Goal.** A public, engine-owned write/curation API — the write-side counterpart to
`execute_graph_walk_nlq` — so hosts build and curate the knowledge graph at runtime without
re-implementing IRI minting, validation, provenance, and cache coherence.

**Shape (sketch).**
- `assert(store, graph, instance, opts) -> AssertOutcome` — write a typed instance: `rdf:type` +
  datatype properties + **object properties with IRI objects** (the capability `write_overlay_quads`
  structurally lacks). Mint IRIs per a documented convention (generalize the existing `urn:moose:…`
  patterns).
- `retract` / `supersede` — generalize the curation substrate already present: the
  `moose:ProvisionalTriple` reification + `activityId`, plus `review::validate` (promote
  provisional→canonical) and `review::reject` (retract).
- **Reuse the existing seams** (verified cleanly decoupled):
  - `StructuralValidator` (`clarification/validator.rs:34`) — widen `ProposedAddition` from one label
    predicate to a full instance bundle (+ optional SHACL shape check).
  - `ProvenanceWriter` (`clarification/writer.rs:35`) — generalize its event from
    `ClarificationAcceptEvent` to a neutral `AssertionEvent`/`MutationEvent` so PROV-O serves any
    mutation.
- **Cache-coherent maintenance** — the part only core can do: splice new instances' labels into the
  live `ClassEntityIndex`/`label_sets` incrementally, instead of full-graph invalidation. The index
  internals (`exact_map`/`token_map`/`label_sets`) are private; a host's only lever is the O(graph)
  sledgehammer.

**Boundary.** Core owns *"assert/curate any typed triple safely, keep indexes + provenance
coherent."* Hosts own domain typing (e.g. what an `ArchitecturalDecision` is) and their tool/transport
surface.

**Sequencing.** MOOSEDev prototypes the typed write path host-side first (`store.insert` +
`invalidate_graph`/`invalidate_all`, leaning on Ask 1 for correctness) to discover the right API shape
under real use, then proposes promotion to core with incremental coherence. This keeps MOOSE's own
evolution faithful to "leverage existing capabilities" — generalize proven machinery, don't greenfield.

## Secondary candidates (lower priority)

- **Public SPARQL surface** 🟡 — SPARQL execution is `pub(crate)` (`supplemental/helpers.rs` wraps
  `oxigraph::sparql::SparqlEvaluator` + an internal optimizer + prefix conventions). Every host will
  want structured queries beside NLQ. MOOSEDev wraps oxigraph directly for v1; exposing MOOSE's
  evaluator+optimizer+prefixes would standardize it for the family.
- **Default trait impls** 🟡 — ship a feature-gated OpenAI-compatible `LlmClient` and a simple
  RDF/table `OntologyResolver`. MOOSE already optionally depends on `openai-api-rs`. Removes
  boilerplate every host rewrites.
- **Public structured execution-trace API** 🟡 — `materialize_execution_trace` is `pub(crate)`;
  traces are reachable only by SPARQL-ing the session graph or reading `PipelineTimings` off the
  single-shot path. A first-class public trace object would serve the auditability every host wants.
- **`index_ontology` / `VecStore::build_from_vocabulary`** ⚪ — *Deferred.* Because MOOSEDev ships
  precomputed ontology vectors, runtime ontology indexing isn't needed for v1; it matters only for a
  future bring-your-own-ontology path. If built, it belongs in core: only MOOSE knows the exact
  embedding recipe its ranker expects (query-side `embed_query` is `pub(crate)`; a host using
  `backbone.embed()` may not match it).
