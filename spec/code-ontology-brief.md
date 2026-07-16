# Code Ontology — Generation Brief

**Audience:** Trivyn's ontology generator (and whoever drives it).
**Consumer:** MOOSEDev v2 — the code layer (spec §3) types its entities, judgments, and
observations against this module.
**Goal:** produce the v2 **software-code** module: the source-code layer beneath the
architectural component layer. It is the seam the v1 brief reserved ("language-specific
(deferred) — specializes SE"), generalized: it is **language-agnostic** — language specifics
live in the substrate identity scheme (SCIP symbols), never in the ontology.

Settled inputs this brief encodes (do not re-litigate): three strata AD `b0d3e4d9`; role
taxonomy AD `b5b4762b` (ratified 2026-07-06); entity identity AD `136dbf24`; intent links AD
`7079634f`; alignment pass 2026-07-03 (all 8 new terms cleared, cosines 0.46–0.54 < 0.55 bar).

---

## 1. Layering

```
BFO → CCO                         (fixed foundation)
 ├─ software-engineering          (v1, thin)  — components, modules, interfaces
 │     └─ software-code           (THIS BRIEF) — code entities realizing them
 └─ software-architecture         (v1)        — ICEs about the system
```

- Emit as a **separate module/graph** (`…/software/code#`) that **imports and specializes the
  software-engineering module** — attach under its Component/Module lineage (the alignment
  pass put CodeEntity's nearest neighbor at SystemComponent, 0.488). Do not remodel SE.
- **Reuse, never re-mint:** `Interface`, `InterfaceDefinition`, `Module`, `DataStore`,
  `Constraint`, the lifecycle/supersession machinery, and the properties
  `dependsOn` / `implements` / `uses` / `accesses` / `exposes`.

## 2. Classes to mint (the 8 aligned terms)

**Stratum 1 — structure** (derived from source, regenerated per commit, never hand-asserted):

- **`CodeEntity`** (central) — a continuant identified by symbol path + kind, persisting
  through edits. Kinds: function, method, type, trait, macro, const, test, migration, config
  (a `kind` designator or shallow subclasses — generator's call, but keep it flat).
- **`CodeFile`** — physical containment node (directory/file hierarchy).
- **`CodeSnapshot`** — a commit-anchored state of an entity carrying the volatile facts
  (content hash, line span, metrics). Observations attach **here**, never to the entity.

**Stratum 2 — judgments** (proposed from evidence, human-ratified, lifecycle-managed):

- **`CodeRole`** — anti-rigid role an entity *plays*. Fixed individuals (ratified):
  `core-algorithm`, `domain-logic`, `boundary`, `glue`, `boilerplate`, `generated`.
- **`Criticality`** — an axis **orthogonal** to role (a payments CRUD handler is boilerplate
  by role, critical by cost-of-being-wrong).
- **`AttentionPolicy`** — f(role, criticality, observations); never f(role) alone.

**Stratum 3 — observations** (measured, instrument-provenanced, append-only, commit-anchored):

- **`Observation`** — with subtypes for: performance/profile, change dynamics (churn, age,
  authorship concentration), defect attribution, verification state.
- **`AuthorshipProvenance`** — human vs. which agent/session authored a region; first-class.

OntoClean discipline: **rigid kind vs anti-rigid role**. An entity *is* a Function and
*plays* CoreAlgorithm via `playsRole` — never role-as-subclass.

## 3. Relations (seed)

| Property | Domain → Range | Notes |
|---|---|---|
| `playsRole` | CodeEntity → CodeRole | carries lifecycle status + provenance (reified or RDF-star — generator's call) |
| `hasCriticality` | CodeEntity → Criticality | same discipline as playsRole |
| `declaredIn` | CodeEntity → CodeFile | **crosses** the two containment hierarchies |
| logical containment | CodeEntity → Module/CodeEntity | crate/module/item — keep distinct from physical |
| physical containment | CodeFile → directory | |
| `calls` | CodeEntity → CodeEntity | topology; `uses`/`implements`/`accesses` reused from SE |
| exposure | CodeEntity → (public API \| FFI \| network entry) | designator or class — generator's call |
| `renamedFrom` / `movedFrom` / `splitInto` / `mergedInto` | CodeEntity → CodeEntity | refactor continuity = code-level supersession |
| `hasSnapshot` | CodeEntity → CodeSnapshot | snapshot carries commit anchor |
| `observedIn` (or inverse) | Observation → CodeSnapshot | never Observation → CodeEntity |
| `realizes` | CodeEntity → SystemComponent | intent link into architecture |
| `satisfies` | CodeEntity → Requirement | |
| `embodies` | CodeEntity → Pattern | |
| `constrains` / `violates` | **widen** existing domains to include CodeEntity | contracts reuse `Constraint` |

Datatype properties: symbol identity string, kind, commit id + content hash + line span on
snapshots (line numbers live **only** on snapshots — nothing in the graph is anchored by line
number), timestamps, author/agent reference.

## 4. Stratum discipline → SHACL

Shapes differ per stratum (Consequences `44229453`, `9c452470`):

- **Structure**: no ratification provenance permitted — it is re-derived; hand-assertion is a
  violation.
- **Judgments**: require lifecycle status + ratification provenance on every `playsRole` /
  `hasCriticality` assertion.
- **Observations**: append-only; require instrument provenance + commit anchor; must attach to
  a CodeSnapshot.

Instance-layer minting policy (context, not ontology content): modules + public surface always
minted; other entities lazily on first attachment; statements/expressions **never** — do not
model statement/expression classes.

## 5. Detail posture

Same as v1: **lexical surface HEAVY** (labels, altLabels, hiddenLabels, skos:definition on
every term — it is the alignment signal), **relations HIGH** (domain/range everywhere),
**taxonomy SHALLOW**, **axioms/SHACL SELECTIVE** (the stratum shapes above are the high-value
set).

## 6. Integration contract

- Turtle, oxigraph-parseable; `a owl:Class` / `owl:ObjectProperty` / `owl:DatatypeProperty`.
- Subclass into CCO via the software-engineering lineage.
- **Hash-style namespaces** (AD `f68ef43e`): `https://trivyn.io/ontologies/software/code#`,
  shapes at `…/software/code/shapes#`. Stable across regenerations.
- Emit as its own graph/file; import the engineering module rather than duplicating terms.

## 7. Out of scope

- Language-specific classes (RustTrait, PyClass, …) — identity is the substrate's job.
- Statements/expressions. AST detail of any kind (Alternative `a5360220`).
- The active-layer vocabulary beyond AttentionPolicy (push/gate/capture verbs are tool
  behavior, not ontology).

## 8. Deferred modeling decisions (the generator's calls)

1. **BFO grounding of CodeEntity vs CodeSnapshot** — continuant vs. generically-dependent /
   ICE treatment of a commit-anchored state (the v1 §8 question, now for code).
2. **Status-carrying assertions** — reification vs RDF-star for `playsRole` provenance.
3. Kind and exposure as designators vs. shallow subclasses.

---

## 9. Paste-ready generation prompt (domain hint)

> The source-code layer of a software system — the code entities beneath the architectural
> component layer, and the typed knowledge attached to them. Extends the existing
> software-engineering ontology (system, component, module, interface, data store); reuse its
> terms, do not remodel them. Language-agnostic: language specifics live in the code-index
> identity scheme, not here.
>
> Core concepts: code entity (a continuant identified by symbol path plus kind — function,
> method, type, trait, macro, const, test, migration, config — persisting through edits); code
> file; code snapshot (a commit-anchored state of an entity carrying volatile facts: content
> hash, line span, metrics); code role (an anti-rigid role an entity plays — core algorithm,
> domain logic, boundary, glue, boilerplate, generated); criticality (an axis orthogonal to
> role); attention policy (a function of role, criticality and observations together);
> observation (a measured, instrument-provenanced, append-only occurrent anchored to a
> snapshot — performance profile, change churn and authorship concentration, defect
> attribution, verification state); authorship provenance (whether a human or a specific agent
> authored a region).
>
> Core relationships: an entity is declared in a file (physical containment:
> directory/file) and sits in a logical module/item hierarchy — two distinct containment
> hierarchies crossed by declaredIn, never merged. Entities call, use, implement and access
> one another, and expose public API, FFI, or network entry points. An entity plays a role via
> a playing-relation that carries lifecycle status and ratification provenance — never
> role-as-subclass: an entity IS a function and PLAYS core algorithm. Refactor continuity is
> typed edges: renamedFrom, movedFrom, splitInto, mergedInto. Observations attach to
> snapshots, never directly to entities; nothing is anchored by line number except snapshot
> data. Code links upward into architecture and knowledge: an entity realizes a system
> component, satisfies a requirement, embodies a pattern; constraints constrain code entities
> and anti-patterns are violated by them.
>
> Discipline: three strata with distinct change semantics — structure (derived from source,
> regenerated per commit, never hand-asserted), judgments (proposed from evidence,
> human-ratified, lifecycle-managed), observations (measured, append-only). Shape validation
> should differ per stratum. Keep the taxonomy shallow and the lexical surface rich.
