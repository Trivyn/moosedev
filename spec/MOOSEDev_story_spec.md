# Spec — Story: Human-facing subsystem comprehension

> Satisfies `Requirement/52721e84-313e-4fa2-a26e-d6a7685e793c` and implements
> `ArchitecturalDecision/768665cb-8f3a-4a9b-b89b-b83903050dea`, constrained by
> `Constraint/66d15437-2afd-4459-9707-d7e5c5423fa6` and the existing one-brain
> `Constraint/2ba76439-e146-425d-b18a-4d46f7418cb8`.
>
> **Status:** accepted for implementation. If this document and the project knowledge graph
> disagree, the graph wins and this document has a bug.

## Goal

Story helps an experienced developer quickly recover a working mental model of an unfamiliar or
forgotten subsystem. It turns current, accepted project knowledge into a concise guided explanation
of what the subsystem does, where its boundaries are, which code matters, why it is shaped that way,
and what to be careful about when changing it.

Story is a projection over existing knowledge, not a new knowledge type or a second source of truth.

## Reader experience

The workbench has a **Stories** page with a “Tell me the story of…” prompt and a list of draft and
published recipes. A reader may name a topic or choose a component; an ambiguous topic requires an
explicit component choice.

A Story contains three to five ordered beats:

1. purpose,
2. boundary,
3. core code,
4. governing decisions or constraints, and
5. risks or extension points.

Only beats supported by current accepted evidence are shown. Missing evidence is an explicit
knowledge gap, never invented connective tissue. Each beat exposes its source records, lifecycle
state, and resolvable code anchors. When the graph contains enough distinct authoritative evidence,
the Story ends with one or two structured questions whose answers are graded symbolically from
accepted graph relationships. If a nontrivial question cannot be formed without inventing a
distractor, Story exposes that limitation as an explicit gap instead. Question options are ordered
independently for each run. Client-visible option IDs and check handles are opaque: the designated
answer exists only in a bounded, expiring daemon grant and is revalidated, together with the subject
and every offered option, against the current graph when answered.

Every run has a visible trust state:

- **Generated** — assembled on demand from current graph state.
- **Draft** — a generated route saved for maintainer curation.
- **Published** — a route reviewed and shared by a maintainer.

A matching published recipe is preferred, but a reader can always request a fresh generated Story.

## Generation and narration

One deterministic backend planner serves every surface. It resolves the component, selects the
bounded route, orders accepted evidence, resolves code anchors, and derives comprehension checks
before any prose generation occurs.

Story remains usable in pure symbolic mode through templates and extractive record text. A configured
LLM is **strongly recommended** for concise human-readable narration. The LLM receives only the
planner-selected evidence and acts as a presentation sensor: its prose is never accepted project
knowledge. Missing, invalid, or failed narration falls back to the symbolic rendering.

The workbench renders the symbolic Story first, then may apply assisted prose only when the second
response has the same structural and evidence fingerprint. The presentation-only request does not
mint a second set of comprehension-check grants. Only narration mode and beat narrative text change;
curator notes remain visible exactly once, and the symbolic run, evidence, gaps, and issued
comprehension checks remain authoritative.
A mismatched or late narration response cannot replace the Story, reset quiz progress, or restore a
Story after the reader navigates away or begins another request.

Generating, reading, or completing a Story never writes the project graph. A discovered gap enters
the existing proposal and ratification workflow only through an explicit user action.

## Curated recipes

Generated runs are ephemeral unless saved. Draft and published recipes are typed, version-controlled
JSON files loaded, validated, and written by the daemon. They live outside the project knowledge graph
and contain only instructional metadata and references:

```ts
interface StoryRecipe {
  id: string;
  title: string;
  subject_component_iri: string;
  goal: string;
  audience: "reboarding";
  beats: StoryBeatRecipe[];
  status: "draft" | "published";
  curator: string;
  updated_at?: string;
}

interface StoryBeatRecipe {
  id: string;
  title: string;
  intent: "purpose" | "boundary" | "core-code" | "governance" | "risk";
  record_iris: string[];
  code_symbols: string[];
  curator_note?: string;
}
```

Recipes never copy authoritative claims, descriptions, lifecycle state, or code locations. Those are
resolved at run time. A superseded record anchor follows its current successor and marks the recipe as
changed since curation. A missing record or code symbol remains visible as drift; Story never chooses
a lexical substitute silently.

Published recipes use each beat intent at most once and keep the canonical order: purpose, boundary,
core code, governance, then risk. A recipe may select any three to five of those intents. The exact
subject `SystemComponent` is intrinsic authoritative evidence for a boundary beat, so that beat does
not duplicate the component IRI in `record_iris`; every other published beat requires at least one
grounded record or code anchor.

`updated_at` is the daemon-issued recipe revision token. Saves and publication use it for optimistic
concurrency: a request based on an older revision is rejected instead of overwriting newer curation.

## HTTP and UI contract

The workbench is a thin client of daemon-owned planning, validation, persistence, and grading. The
HTTP surface supports:

- listing draft and published recipes;
- generating a Story from a prompt, component, or recipe;
- reading and saving a recipe;
- publishing a validated draft; and
- grading a structured relationship check.

Reader progress and check results are local session state, not project knowledge.

## Honest degradation

- Without an LLM, the complete symbolic Story remains available with less polished prose.
- Without a usable code substrate, the evidence narrative remains available and code navigation
  reports reduced coverage.
- Proposed records may be shown only as labeled gaps and never as established claims.
- Superseded and missing evidence is called out; it is not silently hidden or replaced.
- Story introduces no new classes or properties into the architecture or code ontologies.

## Acceptance criteria

- A component produces a three-to-five-beat Story without an LLM.
- LLM-assisted narration is evidence-bounded and safely falls back to symbolic rendering.
- Every factual beat exposes accepted, ontology-validated record evidence and code anchors.
- Ambiguous subjects require explicit resolution.
- A generated route can be saved, curated, published, reloaded, and shared through version control.
- At most one published recipe is authoritative for a component at a time.
- Published beat intents are unique and remain in canonical route order.
- Curated evidence and code anchors are shown only when linked to the recipe's exact subject.
- Generated boundary evidence survives save and reload without being misclassified as a record anchor.
- Recipe files contain references and presentation metadata, not copied project claims.
- Recipe reads and writes stay inside the project `stories/` directory and reject symlink escapes.
- One or two nontrivial structured questions are graded deterministically against current graph
  relationships when distinct options exist; otherwise the missing assessment is an explicit gap.
- Question order does not reveal the answer, and no client-visible identifier encodes it.
- A presentation-only narration upgrade does not issue or evict comprehension-check grants.
- Narration upgrades preserve issued checks and in-progress or completed answers.
- Missing, stale, proposed, and superseded evidence is represented honestly.
- Story activity does not mutate the project knowledge graph.

## Deferred

- PR- or diff-specific Stories.
- Decision-archaeology Stories.
- Adaptive tutoring and free-text answer grading.
- Team-wide learning analytics or competency scoring.
- Story ontology classes.
- Automatic conversion of Story narration into accepted project knowledge.
