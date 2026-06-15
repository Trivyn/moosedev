# Architecture Ontology — Generation Brief

**Audience:** Trivyn's ontology generator (and whoever drives it).
**Consumer:** MOOSEDev, which loads the output and feeds it to the MOOSE engine's alignment + NLQ-query pipelines.
**Goal:** produce the v1 domain ontology MOOSEDev types its knowledge capture against. MOOSEDev only *consumes* the output; this brief is the input spec.

This brief fixes **scope, layering, the seed vocabulary, the detail posture, and the integration contract**. It deliberately leaves the professional modeling (BFO/CCO grounding, exact axioms, final taxonomy) to the generator.

---

## 1. Layering

```
BFO                              (fixed foundation)
 └─ CCO                          (fixed foundation)
     ├─ software-engineering     (v1 · keep THIN)   — the system being described
     │     └─ «v2» language-specific (deferred)     — specializes SE; NOT in v1
     └─ architecture-knowledge   (v1 · the focus)   — ICEs *about* the system
```

- **BFO → CCO are the fixed top two layers.** All domain classes subclass into CCO.
- Two **sibling** domain modules under CCO (not a stack): the *system* (software-engineering) and the *knowledge about it* (architecture-knowledge). They relate by **aboutness** (`concerns`, `constrains`, …), not subsumption.
- The **software-engineering module is intentionally thin** in v1. Its jobs: (a) be the **range** of the architecture relations, and (b) be the clean **extension seam** for the v2 language-specific ontology. Do not over-build it.
- Emit the two modules as **separate graphs/files** so they remain independently extensible.

## 2. Module: software-engineering (thin)

Seed classes (generator may refine names/grounding): `Component`, `Module` / `Package`, `Service`, `Interface`, `Dependency`. Optionally `System` / `Subsystem`.

- Ground these in CCO appropriately for **software artifacts** (designed/abstract entities). *This ICE-vs-artifact grounding is the generator's call* — it's the one modeling decision we explicitly defer.

## 3. Module: architecture-knowledge (the v1 focus)

Required classes — each ⊑ `cco:InformationContentEntity`:

- **`ArchitecturalDecision`** (central) — a recorded choice about system structure/behavior, with rationale.
- **`Rationale`** — the reasoning behind a decision.
- **`Alternative`** — an option considered but not chosen.
- **`Constraint`** — a restriction the system/design must satisfy.
- **`Lesson`** — a generalizable insight learned during development.
- **`AntiPattern`** — a recurring problematic structure/practice.

Candidate extensions the generator may add if natural: `Requirement`, `Assumption`, `Risk`, `TradeOff`. Keep optional.

## 4. Relations & attributes (seed)

**Object properties** (with `rdfs:domain`/`rdfs:range`) — these are the edges the NLQ query walks, so model them well:

| Property | Domain → Range |
|---|---|
| `concerns` | ArchitecturalDecision → (SE) Component/System element |
| `hasRationale` | ArchitecturalDecision → Rationale |
| `consideredAlternative` | ArchitecturalDecision → Alternative |
| `supersedes` | ArchitecturalDecision → ArchitecturalDecision |
| `constrains` | Constraint → SE element \| ArchitecturalDecision |
| `violates` | AntiPattern → Constraint |
| `mitigates` | ArchitecturalDecision \| Lesson → AntiPattern \| Risk |
| `learnedFrom` | Lesson → ArchitecturalDecision \| AntiPattern |
| `dependsOn` | (SE) Component → Component |

**Datatype properties** (reuse `rdfs`/`dcterms`/`skos` where idiomatic): a title/label, a description, a `status` for decisions (e.g. proposed/accepted/superseded/deprecated), a record date, an author/agent reference.

## 5. Detail posture

Spend effort where MOOSE's machinery actually reads it:

- **Lexical surface — HEAVY.** Every class/property gets `rdfs:label`, several `skos:altLabel` (+ `skos:hiddenLabel` for jargon), and a `skos:definition` (and `examples` where useful). *This is the alignment signal* (MOOSE's L1 keyword + L2 embedding tiers read exactly these).
- **Relations — HIGH.** Model the object properties above with proper domain/range; the graph's query value comes from edges.
- **Taxonomy — SHALLOW.** Do not deeply subclass. Alignment is designed to *attach* new concepts under existing classes at runtime; depth here just adds ambiguity. Let usage reveal subtypes.
- **Axioms / SHACL — SELECTIVE.** A few high-value items: domain/range, disjointness between the SE and knowledge branches, and SHACL cardinality on load-bearing properties (e.g. a Decision should have ≥1 Rationale). SHACL is optional but valued — MOOSE both validates against it *and* uses it to optimize queries (`schema_shape`).

## 6. Integration contract (must hold for MOOSE to consume it)

- **Turtle**, parseable by oxigraph; classes declared with `a owl:Class`, properties with `owl:ObjectProperty` / `owl:DatatypeProperty`.
- Domain classes **subclass into CCO** (so MOOSE's `CategoryMappings` — which binds the engine's L0 categories to fixed BFO/CCO IRIs — applies without per-regeneration changes).
- **Rich lexical surface** as in §5 (non-negotiable: it's the alignment signal).
- Object/datatype properties carry **`rdfs:domain` and `rdfs:range`**.
- Stable **namespace IRIs** (proposed: `https://moosedev.dev/ontologies/architecture#` and `…/software-engineering#` — adjust to the canonical Trivyn/MOOSEDev convention; MOOSEDev only needs them stable across regenerations).

## 7. Out of scope (v1)

- The **language-specific ontology** (v2) — it extends the SE module later; do not model it now.
- Exhaustive axiomatization / full OWL-DL completeness — not needed for v1.

## 8. The one deferred modeling decision

**How to ground software artifacts in CCO** (designed-artifact continuant vs. information/design entity, software being abstract). The knowledge side is settled (`⊑ cco:InformationContentEntity`); the system side is the genuinely subtle call, and it's the generator's to make.
