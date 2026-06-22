# GROWL enrichment integration — implementation plan

Source of truth: **AD `e0b7b528`** ("GROWL enrich co-locates inferred edges in kg/project"),
superseding `52a6b6cc`. Motivated by Requirement `b3ba8285` (NLQ must answer content-recall **and**
structural queries) + Constraint `72d7a908` (enrichment can't outrun the ontology axioms). This file
is the human-readable mirror (invariant #2).

## Status — IMPLEMENTED & VERIFIED

All stages landed on branch `hybrid_retrieval`. New: `src/reasoning/mod.rs`,
`examples/growl_enrich_probe.rs`, `tests/enrich_integration.rs`. Edited: `Cargo.toml`
(`growl = "0.5.1"`), `src/lib.rs`, `src/graph/mod.rs` (AppState `inferred_stale`/`enrich_lock`
+ `mark_inferred_stale`/`ensure_enriched`; trigger in `relevant_context`/`execute_query`),
`src/mcp/mod.rs` (4 write handlers mark stale; `sparql` ensures fresh), `src/provenance/mod.rs`
(reasoner activity + RDF-1.2 reification + `clear_reasoner_inferences`).

**Two refinements vs the original plan, both confirmed by the dogfood prototype:**
1. **Provenance encoding = RDF 1.2 reification**, not RDF-star `<< s p o >>`-as-subject. The
   oxigraph fork is RDF 1.2 (triple terms are objects only), so each inferred triple is tagged
   `R rdf:reifies «s p o» ; prov:wasGeneratedBy <activity>` — written via the ordinary
   Transaction API (no SPARQL UPDATE, which the codebase rejects). AD `e0b7b528`'s intent
   (per-triple provenance, drop-and-rerun) is preserved.
2. **`enrich_delta` is scoped to record→record object-property edges.** Feeding the full
   ontology T-box makes GROWL also emit CCO/BFO T-box closure, datatype-property liftings, and
   `rdf:type` completions (882 edges on the dogfood graph). Those are invisible to
   `record_neighbors` (it only follows record-object edges) — pure overhead. Scoping the delta
   to edges whose subject AND object are A-box records yields the clean **69 bidirectional-walk
   edges** (motivates 6, isRationaleFor 21, isSupersededBy 19, isConstrainedBy 2, BFO part-of 21).

**Verification:** 3 unit tests (`reasoning::`) + 1 integration test (`enrich_integration`, via
the real bootstrap→record→relate→trigger flow) prove materialization, co-location, RDF-1.2
reification, **0 new SHACL violations**, and drop-and-rerun idempotency. Full `cargo test` green,
clippy clean, UI-embedded release binary builds.

**To activate live:** restart the dogfood serve on the rebuilt `target/release/moosedev` (the
lazy trigger enriches on the first read after startup).

**Open observations (Trivyn-owned, not addressed here):** (a) the base graph is edge-sparse —
~48 asserted object-property edges over 153 records, so enrichment amplifies but the bigger
structural-NLQ win still needs the capture-gap fix; (b) the `BFO_0000176` lifts read "record is
part-of its Rationale", a possible `subPropertyOf` direction to review in the eng/arch ontology.

## Goal

Materialize inferred A-box edges (prp-inv inverses, prp-spo subproperty, prp-dom/rng types, cax-sco
subclass) **deterministically** by running GROWL in enrich mode over `kg/project` + the domain
ontology T-box — so the typed-expansion / graph-walk reads traverse **bidirectional** edges and the
structural half of NLQ starts answering. The inverse object properties just declared in the
ontologies are the unlock; "enrichment can't outrun the axioms" (`72d7a908`).

## Canonical reference to port

`trivyn/src/rdf/reasoning.rs` — Trivyn already marshals oxigraph ↔ GROWL and runs enrich. We lift it.
GROWL is an **app-layer** dep there (`trivyn/Cargo.toml: growl = "0.5.1"`), **not** in `moose` —
MOOSEDev sits at the same layer, so the dep goes in `moosedev/Cargo.toml` (invariant #11).

Key API facts confirmed from that file:
- Enrich flag: `ReasonerConfig::new().verbose(false).enrich(true).cancel_token(&tok)` →
  `reasoner.reason_with_config(&cfg)` (reasoning.rs:831).
- `Reasoner::with_capacity(bytes).filter_annotations(true)`; load via `add_triple_ref(&s,&p,&o)`
  with borrowed `growl::Term::{Iri(&str), Blank(i64), Literal{value,datatype,lang}}`.
- Result `OwnedReasonerResult::{Success{triples,inferred_count,iterations}, Inconsistent, Cancelled}`;
  `Success.triples` is the **full** materialized set → compute the delta ourselves (reasoning.rs:810).
- `growl = "0.5.1"` is a plain crates.io dep with a vendored C core (Trivyn builds against it on this
  box) — no system `libgrowl`, no open-source-build concern.

## Design (already decided — AD `e0b7b528`)

- **Placement:** inferred triples co-located **in `kg/project`** (NOT a separate `kg/inferred` graph).
  Confirmed rationale: every `get_relevant_context` expansion/walk read is graph-scoped to
  `PROJECT_KG_GRAPH_IRI` — `record_neighbors` (src/graph/mod.rs:1585), `build_context_item`
  (1760, 1810), `list_instances` (1728), `require_information_record` (764) — so co-location makes
  inferred edges first-class with **zero hot-path change**; a separate graph would need unioning into
  all 5–8 reads.
- **Provenance / drop-and-rerun mechanism:** per-triple RDF-star
  `<< s p o >> prov:wasGeneratedBy <reasoner-activity>` in the sibling `kg/provenance` graph. The tag
  is load-bearing (it's how we tell an inferred edge from an asserted one once co-located).
- **Dense index:** untouched — inferred edges carry no `rdfs:label`/`hasDescription`, so `index_record`
  has nothing to embed (the `f1f86296` footgun does not apply).
- **Deferred (user's call):** transitive / propertyChainAxiom. Integration is unchanged when they land
  later — just declare more axioms and the same enrich pass picks them up.

## Stages (checkable)

### Stage 0 — dependency + smoke
- [ ] Add `growl = "0.5.1"` to `moosedev/Cargo.toml` `[dependencies]`.
- [ ] `cargo build --release` green (confirms the vendored C core builds in this tree).
- [ ] Micro-smoke (a `#[test]`): 3-triple `IndexedGraph` with one `owl:inverseOf` axiom + one asserted
      edge → enrich → assert the inverse triple appears in the delta. (Model on
      `trivyn/src/tests/reasoning_tests.rs`.)

### Stage 1 — port marshalling + enrich core  → new `src/reasoning/mod.rs`
- [ ] Lift `BlankNodeMapper` (oxigraph bnode str ↔ growl i64).
- [ ] Lift the four converters: `subject_to_growl_term`, `object_to_growl_term`,
      `owned_term_to_subject`, `owned_term_to_term` (swap `TrivynError` → `anyhow`).
- [ ] Lift the reasoner-drive + `.enrich(true)` config + match on `OwnedReasonerResult`.
- [ ] Lift the input-set `(s,p,o)` string-keyed **delta-dedup** (keep only genuinely-derived triples).
- [ ] Wrap in `spawn_blocking` + `tokio::select!` timeout with `CancelToken` (don't block the MCP
      server on the solver). Keep arena sizing (32 MB floor, 1 KB/triple).
- [ ] Inputs: gather `PROJECT_KG_GRAPH_IRI` A-box + `ontology::SE_DOMAIN_GRAPH_IRI` +
      `ARCH_DOMAIN_GRAPH_IRI` T-box via `store.quads_for_pattern(None,None,None,Some(graph))`. The
      ontology MUST be in the input — enrich only materializes **declared** axioms.
- [ ] Single batch (drop Trivyn's batching/sandbox/core-ontology paths — moosedev's graph is hundreds
      of triples).

### Stage 2 — write-back: co-locate + RDF-star provenance
- [ ] Insert the delta quads into `kg/project` via `store.bulk_loader()` (idempotent put).
- [ ] Extend `src/provenance/mod.rs`: a reasoner-inference `prov:Activity` (agent = a stable GROWL
      agent IRI; `used` = project + ontology graphs; started/ended) + per-triple
      `<< s p o >> prov:wasGeneratedBy <activity>` via **SPARQL UPDATE `INSERT DATA`** into
      `kg/provenance` (oxigraph rdf-12 allows quoted-triple subjects only via UPDATE, not `Quad::new`
      — mirror reasoning.rs:970-1006).

### Stage 3 — drop-and-rerun + trigger
- [ ] `clear_inferred(store)`: find `kg/project` triples whose reified form is
      `prov:wasGeneratedBy` a reasoner activity in `kg/provenance`; DELETE those triples + their
      annotations. Idempotent; run before each enrich.
- [ ] `enrich(state)` = `clear_inferred` → materialize → annotate. The public entry point.
- [ ] **Lazy trigger:** `AtomicBool inferred_stale` on `AppState`; set in `record_important_decision`,
      `relate`, `supersede_decision`, `retract_decision`; an `ensure_enriched()` at the top of the
      edge-traversing read paths (`relevant_context` / `query` / `sparql`) runs `enrich()` once if
      stale, then clears the flag — so a capture burst enriches **once** before the first recall.
- [ ] Explicit enrich entry point (MCP tool or startup hook) for the bootstrap bulk-load case.

### Stage 4 — verify (no "done" without proof — practice #4)
- [ ] Unit: prp-inv mints `concerns`→`isConcernedBy` (and one eng inverse) on a fixture.
- [ ] Dogfood: run on `.moosedev`; report materialized-edge count; show `record_neighbors` now
      surfaces an inverse edge it didn't before (e.g. Requirement `b3ba8285` lists the decisions that
      `isMotivatedBy` it via the **forward** materialized edge).
- [ ] `validate_against_architecture` → 0 **new** violations (co-located prp-dom/rng types shift SHACL
      targeting).
- [ ] Dense index unchanged (instance-vectors.db embed count stable).
- [ ] Idempotency: enrich twice → second delta is 0 new (clean drop-and-rerun).
- [ ] Retrieval A/B: a structural query pre/post-enrich answers via bidirectional traversal.

## Fallback / follow-ups (not blocking)
- **Lighter provenance** if we defer RDF-star: a `kg/inferred` graph used purely as a drop-manifest
  (stores the delta as the delete-list, NOT read by retrieval) — same zero-hot-path property,
  coarser ("it's inferred") provenance. Faithful RDF-star is preferred (invariant #6, max Trivyn reuse).
- **Cleanup once inverses materialize:** `build_context_item`'s hand-rolled inverse-`supersedes`
  reverse lookup (src/graph/mod.rs:1799-1824) can read the materialized `isSupersededBy` forward edge.
- **`edge_priority`** (src/graph/mod.rs:1711, flagged `JAMES: ... FIX`) is coupled to ontology object
  properties; revisit so newly-materialized inverse predicates get sensible expansion priority.
