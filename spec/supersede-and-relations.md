# Spec — Object-property capture + the supersede-with-rationale lifecycle

> Satisfies `Requirement/b2f8240c` (expose supersede + rationale; preserve superseded decisions as
> history) and is the *resolution* half of `Requirement/b5b101dd` (contradiction handling).
> Status: **proposed** (not yet implemented).

## Goal

When an architectural decision changes, keep the old one as an immutable historical record, link the
new one to it, and capture **why** it changed — using ontology terms that already exist. Nothing is
ever overwritten or deleted; "what we used to think, what we think now, and why" becomes one walkable
subgraph. This is the comprehension-debt payload MOOSEDev exists to preserve.

## What already exists (build on it — invariant #8)

- **Write primitive:** `moose::kg::assert_instance` already writes IRI-valued edges via
  `InstanceAssertion.object_props: &[ObjectAssertion { predicate_iri, object_iri }]`
  (moose/src/kg.rs:42, :118). It commits transactionally and invalidates the entity-index cache.
- **Ontology terms** (software-architecture.ttl, all present):
  - `supersedes` (ObjectProperty) — *"Relates an architectural decision to the one it replaces."*
  - `hasRationale` (ObjectProperty) → `Rationale` (a class, `rdfs:subClassOf InformationRecord`).
  - `hasLifecycleStatus` (DatatypeProperty) ∈ { proposed, accepted, superseded, deprecated }.
  - Competency question baked in: *"Which architectural decision supersedes a previous one?"*
- **Vocabulary lookup:** `CompactVocabulary.object_properties` (moose/src/types.rs) — resolve a relation
  IRI by local name, exactly like `datatype_property_iri` does for literals today.
- **Provenance:** `provenance::record_provenance_at(store, iri, agent, when)` (who/when).
- **Cache coherence:** `EntityIndexCache::invalidate_graph(graph_iri)` (moose/src/entity_index.rs:1277).

## The gap

1. `graph::RecordInput` carries only literal `properties: Vec<(String, String)>`; `record_instance`
   passes `object_props: &[]` (src/graph/mod.rs:273). **No relation can be written.**
2. No primitive mutates an existing instance, so an old decision's status can't be flipped to
   `superseded` (MOOSE intentionally exposes only `assert_instance`, no update/retract).
3. No tool/read surface for the supersede chain.

## Design

### Phase 1 — generic object-property plumbing (enabling, low-risk)

Make the existing write path able to assert relations. Benefits every relation
(`supersedes`, `hasRationale`, `isMotivatedBy`, `concerns`, …), not just supersede.

- `RecordInput` gains `object_properties: Vec<(String /*predicate_iri*/, String /*object_iri*/)>`
  (default empty — every current caller is unaffected).
- `record_instance` maps them into `ObjectAssertion`s and passes them through to `assert_instance`
  (replace the hardcoded `&[]`).
- Add `object_property_iri(vocab, local) -> Result<String>` mirroring `datatype_property_iri`, plus a
  public `resolve_object_property(&self, local)` on `AppState` (parallel to `resolve_class`), so the
  volatile namespace stays out of the code (the `[[decouple-code-from-ontology-ttl]]` invariant).

### Phase 2 — `supersede_decision` (the lifecycle operation)

A **new** domain function and a **new** MCP tool (kept separate from `record_important_decision`,
whose contract is "record a *fresh* item"; supersede mutates an existing record and that side effect
should be explicit and discoverable).

```rust
// src/graph/mod.rs
pub struct SupersedeInput {
    pub superseded_iri: String,   // the decision being replaced (must exist)
    pub new: RecordInput,         // the replacement (class defaults to ArchitecturalDecision)
    pub rationale: String,        // WHY it changed — required; this is the point
}

/// Atomically: mint the new decision + a Rationale node, link
/// new -supersedes-> old and new -hasRationale-> rationale, and flip the OLD
/// decision's lifecycle status to "superseded" (the old record is otherwise
/// untouched). One transaction; cache invalidated once on success.
pub fn supersede_decision(state, input: &SupersedeInput, author, when) -> Result<SupersedeOutcome>;
// SupersedeOutcome { new_iri, rationale_iri, superseded_iri }
```

Behavior:

1. **Precondition.** `superseded_iri` must exist in the project graph and be an `ArchitecturalDecision`
   (or a subclass). Reject a dangling/mistyped target with a clear error — `supersedes`' range is a
   decision, so don't create a broken edge.
2. **Mint** `new_iri = mint_instance_iri("ArchitecturalDecision")` and
   `rationale_iri = mint_instance_iri("Rationale")`.
3. **Build quads** (project graph):
   - new decision: `rdf:type`, `rdfs:label`, `hasTitle`, `hasDescription?`, default
     `hasLifecycleStatus "accepted"`, `hasAuthor`, typed `hasTimestamp` (reuse the existing
     literal-defaulting logic).
   - rationale: `rdf:type Rationale`, `rdfs:label "<short>"`, `hasDescription <rationale text>`.
   - edges: `new hasRationale rationale`, `new supersedes old`.
4. **Status flip** (the new mutation): remove **all** existing `(old, hasLifecycleStatus, *)` quads in
   the project graph and insert `(old, hasLifecycleStatus, "superseded")`. Flip regardless of prior
   value (it may have been `proposed`, never explicitly `accepted`). **Nothing else on the old
   instance is changed; it is never deleted.**
5. **Atomicity.** Do steps 3–4 in **one** `store.start_transaction()`: `transaction.extend(insert
   quads)` + `transaction.remove(old status quads)` → `commit()` → `entity_index.invalidate_graph(
   PROJECT_KG_GRAPH_IRI)`. (Mirror `assert_instance`'s shape; verify `Transaction::remove` on the
   oxigraph-trivyn fork — if unavailable, fall back to store-level remove+insert then invalidate,
   accepting a non-atomic window and documenting it.)
6. **Provenance.** After commit, `record_provenance_at` for `new_iri` and `rationale_iri` (best-effort,
   never fails the write — current contract). The supersession is auditable: the `supersedes` edge =
   *what*, the `Rationale` = *why*, provenance = *who/when*.

**Modeling note (why = a node, not a string).** `hasRationale`'s range is the `Rationale` class, so the
why is a first-class instance (invariant #2) — later linkable (`isMotivatedBy` a new `Requirement`,
`learnedFrom` a `Lesson`) and independently queryable. We attach `hasRationale` to the *new* decision
(its domain is the decision; RDF can't put properties on the `supersedes` edge without reification —
noted as a future option if edge-level rationale is ever needed).

MCP tool:

```
supersede_decision(superseded_iri: string, title: string, rationale: string,
                   description?: string, kind?: string="ArchitecturalDecision")
```
Thin handler: validate non-empty `title`/`rationale`/`superseded_iri`, build `SupersedeInput`, call the
domain fn, return `Superseded <old> → <new> (rationale <rationale_iri>)`.

### Phase 3 — read path reflects lifecycle

- `relevant_context` / `list_instances` (src/graph/mod.rs:379, :416) **default to current**: exclude
  instances whose `hasLifecycleStatus` ∈ { superseded, deprecated }. Keeps the working set clean.
- Surface the chain on items that have it: `supersedes <iri>` / `superseded by <iri>` and the
  `hasRationale` text, so history is one hop away — never gone.
- Optional `include_history: bool` (default false) on `get_relevant_context` to list superseded items
  too. The existing `sparql` tool already answers the competency question directly:

  ```sparql
  SELECT ?new ?old ?why WHERE {
    GRAPH <https://moosedev.dev/kg/project> {
      ?new <…/domain/supersedes> ?old ;
           <…/domain/hasRationale> ?r .
      ?r <…/domain/hasDescription> ?why .
    } }
  ```

## Tests (mirror existing integration-test style)

- **Phase 1:** `record_instance` with an `object_properties` entry writes the edge; SPARQL reads it back.
- **Phase 2:** after `supersede_decision`: new decision exists (`accepted`); `new supersedes old` and
  `new hasRationale ?r` present; `?r` is a `Rationale` with the why text; **old still exists** with
  status exactly `superseded` and all its other triples intact; provenance present for the new IRIs.
- **Precondition:** superseding a non-existent / wrong-class IRI errors and writes nothing
  (transaction rolled back — assert via no partial quads).
- **Phase 3:** default `relevant_context` omits the superseded decision; `include_history` includes it
  with the supersedes link rendered.
- Verify: `cargo test`, `cargo clippy`, and a real-binary smoke (record → supersede → query the chain).

## Out of scope / follow-ons

- **Dedup** (`Requirement/5565038e`) and **contradiction *detection*** (`Requirement/b5b101dd`) — this
  spec is the *resolution* mechanism; detection that proposes a supersede is separate.
- A generic `link`/relation tool (e.g. `isMotivatedBy`) beyond supersede — Phase 1 enables it; the tool
  surface is a later pass.
- A SHACL shape asserting "a superseded decision must be the object of some `supersedes`" — nice
  integrity check once data accrues.
- Folding provenance into `record_instance` so every write is provenanced by construction (already
  flagged in src/mcp/mod.rs).
```
