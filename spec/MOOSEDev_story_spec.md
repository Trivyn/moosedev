# Spec — Story: Cohesive, evidence-backed project narratives

> Satisfies `Requirement/9327d3b1-d868-4f37-9d0d-eb7ee3f054de` and implements
> `ArchitecturalDecision/53720fee-d7ff-43d3-943f-64acff2fe112`, constrained by
> `Constraint/c1b8a8db-6904-4c33-a7d0-7734ad9de0ed` and informed by
> `Lesson/f0c35d76-67ef-472e-a74b-d44f084b37f9`.
>
> **Status:** accepted for implementation. If this document and the project knowledge graph
> disagree, the graph wins and this document has a bug.

## Goal

Story helps an experienced developer recover a working mental model of a project subject. It turns
the subject's connected project knowledge, lifecycle history, and code evidence into one cohesive,
causal, chronological account: what the subject is, why it exists, how it evolved, how it works now,
and what remains uncertain.

Story is a read-only projection over existing knowledge. It is not a knowledge type, capture path,
or second source of truth. This feature requires no ontology changes.

## Reader experience

The workbench has a **Stories** page with separate **Entity** and **Topic** entry modes plus a library
of draft and published recipes.

- Entity mode offers the complete current catalog of `SystemComponent`, `InformationRecord`, and
  `CodeEntity` subjects and submits canonical IRIs.
- Topic mode uses the existing bounded project-context retriever. A topic is a query over existing
  entities and never mints a Topic ontology node.

An opened Story is a single article rather than a stack of evidence cards. It contains:

1. a brief orientation;
2. one flowing narrative with only the applicable light headings: **Orientation**, **Evolution**,
   **Current state**, **Implementation**, and **Implications**;
3. a deterministic chronological timeline, including explicit supersession paths;
4. linked evidence and code references;
5. a separate, prominent **Knowledge gaps** section; and
6. one or two symbolic comprehension checks when the graph supports nontrivial questions.

The prose may cite evidence inline, but evidence inspection is not allowed to fragment the reading
experience. The evidence appendix is collapsible and grouped by entity type. It exposes full
descriptions, lifecycle status, timestamps, relationships, and code anchors. Links navigate to the
typed ADR, Requirement, Lesson, Constraint, component, or code surface rather than a generic record
page when a typed route exists.

Every run has a visible trust state:

- **Generated** — assembled on demand from the current graph.
- **Draft** — generated focus and presentation metadata saved for maintainer curation.
- **Published** — a recipe reviewed and shared by a maintainer.

A matching published recipe is preferred, but a reader can always request a fresh generated Story.
The page remains text-led and does not add a decorative Story illustration.

### Entry points from code

A Story is most often wanted while reading code, so two entry points lead into it from there.

- **The editor.** An LSP hover that already carries knowledge also carries a `[Tell me the Story]`
  link near the top, addressed at the exact entity under the cursor. The link is built from the
  daemon's live published HTTP address and is omitted entirely when this daemon run is not serving
  the workbench, so it never offers a dead port. It changes no silence policy: an entity with no
  direct records and no judgments still produces no hover, so the link never manufactures a reason
  to interrupt. No editor-client-specific code is involved.
- **The workbench.** `#/stories/entity/{uuid}` is the canonical Story deep link. It carries a record
  UUID like every other workbench route; on load or hash navigation the UUID is resolved through the
  record API and the Story is generated for the exact entity IRI returned. The route is refreshable
  and linkable, and it honors the same unsaved-curation guard as record routes.

A `CodeEntity` is its own Story subject. "Tell this Story" on a code entity's record page launches
the Story for that entity, not for its containing component — redirecting would answer a different
question than the one the reader asked.

### The CodeEntity record page

`#/record/{uuid}` remains the canonical destination for every record, including code entities, but a
`CodeEntity` renders a source-aware section above the relationship graph: kind, signature, substrate
symbol, repo-relative path, and a line-numbered preview of the definition with its lines marked and
a **Show full file** expansion. The viewer is plain monospace text — no syntax-highlighting
dependency — and source is always rendered escaped, never as markup. Non-`CodeEntity` records keep
their existing presentation.

Source is served only when it can be trusted:

- Definitions are located through one substrate helper covering SCIP and the tree-sitter fallback.
  No surface locates a definition by searching a file for its name; a miss stays a miss.
- Bytes are admitted only through the generation-proven read (`indexed_started_at` plus a stable
  stat/read/stat), so an edited working tree or an unverifiable baseline yields metadata plus a
  reindex explanation and **no preview**. The unit of proof is the FILE, not `HEAD`: a moved `HEAD`
  whose file is provably unchanged is still served, flagged `substrate_stale`, because its line
  numbers are still true and withholding would blind the workbench whenever any unrelated file
  changed. A line number is published for a Story code anchor on the same terms.
- Containment is checked after canonicalization, not by rejecting `..`. An indexed path can itself
  be a symlink, or sit under one, and reads follow symlinks — only paths that still resolve inside
  the repository root are servable.
- Source locations are runtime substrate projections. They are never persisted into the project
  knowledge graph, and reading them never writes it.
- A definition that does not lie inside the file actually read is refused rather than clamped: a
  clamped window would render an arbitrary slice under a highlight pointing at nothing.

## Deterministic Story dossier

One backend planner constructs a typed `StoryDossier` before any prose generation. It starts from the
exact entity subject, or from at most 64 topic matches above the existing relevance floor, and
collects the deterministic closure needed to explain that subject:

- all subject literal properties, lifecycle metadata, authorship, timestamp, and incoming and
  outgoing typed relationships;
- every directly connected project entity;
- the typed knowledge cluster around connected records: rationales, motivating requirements,
  alternatives, consequences, constraints and violations, lessons and their sources, components,
  and code relationships;
- code anchors and their architectural relationships, including `realizes`, `satisfies`, and
  `embodies`; and
- complete supersession history in both directions, including branches, with cycle protection.

This is a typed closure, not an unrestricted recursive graph crawl. Neighbors unrelated to one of
the listed explanatory relationships are not expanded recursively.

The planner records why each entity entered the dossier and produces a stable evidence order and
fingerprint. It derives the applicable narrative outline, timeline, gaps, and comprehension checks
before narration. Current working-set knowledge supports present-tense claims. Superseded and
deprecated records support explicitly historical claims. Rejected records appear only as rejected
events. Proposed records appear only as knowledge gaps, never as established evidence.

The closure is bounded to 512 entities or 4 MiB of serialized dossier data. Reaching either ceiling
stops expansion deterministically, records a `closure-truncated` gap, and prevents any claim that the
Story is complete.

## Cohesive narration

Pure symbolic mode renders the outline as one continuous article using deterministic extracts,
relationships, and chronology. It remains a complete, usable fallback when no LLM is configured.

Assisted narration receives only the deterministic dossier and outline. It may make the prose more
cohesive and readable, explain necessary terminology on first use, and connect evidence to the
consequences of changing the subject. It may not alter the subject, evidence, chronology, lifecycle
classification, gaps, checks, or trust state. Curator context is displayed separately and is not
evidence or model input.

Complete selected record descriptions remain retained in the public dossier. For assisted prose,
the server builds one deterministic narration packet of at most twelve provenance groups. The total
prompt budget is one quarter of `MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS` (default 32,768), capped at
32,768 estimated input tokens. Allocation first reserves subject identity, a subject-focused
chronological spine, and one semantically appropriate source for every rendered section. The
chronological spine preserves subject-named records, their complete supersession chains, and
deterministically sampled early, middle, and latest milestones before admitting incidental features
that merely share a broad component relationship. Curated inclusions, implementation anchors,
current decisions, requirements, constraints, and remaining dossier evidence follow. Field values
are atomic: the packet includes a complete value or omits it. Larger configured windows therefore
admit more complete evidence without changing the public dossier.

The default interactive path makes one synthesis request. Short source IDs keep the provider contract
small; the daemon owns their exact mapping to evidence IRIs. JSON-schema structured output is used
when supported, followed by independent typed and citation validation. Auto capability detection may
retry the first explicitly unsupported schema request once with validated plain JSON, then remembers
the provider capability for the daemon lifetime. JSON repair may repair syntax, never semantics.
Every packet source ID must be represented exactly, and the daemon expands and revalidates the exact
IRI union before accepting prose. A failed, invalid, mismatched, or 60-second timed-out synthesis
leaves the complete symbolic article in place.

Successful narration is memoized in a bounded process-local cache keyed by graph generation, model,
effective budget, prompt-contract version, and packet fingerprint. Concurrent identical requests are
single-flight. Failures are not cached, and narration is never written to the project graph or recipe.

Narrative length adapts to evidence density, normally about 700–2,000 words. The workbench renders
the symbolic Story immediately and applies assisted prose only when the returned subject and
evidence fingerprint match the current run. A late response cannot replace another Story, reset quiz
progress, or restore a Story after navigation.

The narration outcome identifies whether the reader is seeing symbolic prose, successful single-pass
assistance, or symbolic fallback, without using the internal term “beat.”

## Chronology, evidence, gaps, and checks

The timeline is a deterministic projection, not LLM output. Events sort by timestamp ascending, use
a stable graph-derived tie-breaker, and group undated events last. Each event carries its lifecycle
state and linked entity. Supersession events show predecessor, successor, and replacement rationale
when present; branches are preserved rather than collapsed to a guessed canonical path.

Narrative paragraphs cite evidence IRIs. The client resolves those citations through server-provided
typed evidence details and code anchors; the model cannot create URLs or unseen citations.

Missing expected relationships, proposed knowledge, unresolved recipe anchors, absent code
substrate, and closure truncation are separate typed gaps. Gaps are never converted into invented
connective prose.

When the graph contains enough distinct authoritative evidence, the Story ends with one or two
structured questions graded from current graph relationships. Question options are independently
ordered for every run. Client-visible option IDs and check handles are opaque: the designated answer
exists only in a bounded, expiring daemon grant and is revalidated, together with the subject and
every offered option, when answered. If no nontrivial distractor exists, the missing assessment is a
gap rather than a trivial question.

Questions prefer the relationships the Story exists to explain — which record replaced a retired one,
which approach a decision rejected — over which end of an edge something sits on, and fall back to
the membership forms when the graph supports nothing richer. Wrong answers are drawn from evidence
the Story actually showed, then from the answer's own kind, so that answering correctly requires
having followed the account rather than matching words in the question against an unfamiliar title.
Selection is deterministic and never derives from label order; only presentation order is randomized.
A candidate whose label is not shaped like a code identifier is never offered as a wrong answer.

## HTTP response contract

Existing Story routes remain, but Story generation returns schema version 3:

```ts
interface StoryRun {
  schema_version: 3;
  recipe_id?: string;
  trust_state: "generated" | "draft" | "published";
  narration_mode: "symbolic" | "llm";
  narration_strategy: "symbolic" | "single_pass";
  narration_outcome: NarrationOutcome;
  narration_failure_reason?: NarrationFailureReason;
  narration_coverage?: StoryNarrationCoverage;
  title: string;
  subject: StorySubject;
  goal: string;
  brief: StoryParagraph;
  narrative: StoryNarrativeSection[];
  timeline: StoryTimelineEvent[];
  evidence: StoryEvidenceDetail[];
  code_anchors: StoryCodeAnchor[];
  coverage: StoryCoverage;
  gaps: StoryGap[];
  checks: StoryCheck[];
}

type StorySectionKind =
  | "orientation"
  | "evolution"
  | "current_state"
  | "implementation"
  | "implications";

type NarrationOutcome =
  | "not_requested"
  | "succeeded"
  | "unconfigured"
  | "ineligible"
  | "timeout"
  | "provider_error"
  | "invalid_response";

type NarrationFailureReason =
  | "packet_too_large"
  | "invalid_json"
  | "schema_mismatch"
  | "citation_mismatch"
  | "structured_output_unsupported";

interface StoryNarrationCoverage {
  eligible_entities: number;
  included_entities: number;
  source_groups: number;
  truncated: boolean;
}
```

`StoryParagraph` contains prose plus `citation_iris`. `StoryNarrativeSection` has a stable section ID,
one of the five outline kinds, and cited paragraphs. `StoryEvidenceDetail` contains its canonical IRI,
typed route information, full description, lifecycle status, timestamp, author, and typed
relationships. `StoryTimelineEvent` contains an optional timestamp, lifecycle state, relation type,
linked entity, predecessor and successor links, and replacement rationale. `StoryCoverage` reports
dossier entity and byte counts, whether expansion was truncated, and which subject families and
outline sections are represented.

Reader progress and check results remain local session state. Checks refer readers back with
`revisit_section_id`; the removed beat IDs are not part of the v3 response.

`StoryCodeAnchor.line` is populated from the current substrate definition lookup when one can be
proven, and is absent otherwise.

### CodeEntity detail and source

`GET /api/v1/records/{uuid}` gains an optional `code` block, present only for a `CodeEntity`:

```ts
interface RecordCodeDetail {
  symbol?: string;              // substrate symbol recorded in the graph
  name?: string;
  entity_kind?: string;         // e.g. "Function"
  logical_path?: string;
  defined_in_path?: string;     // as recorded in the graph
  source_path?: string;         // where the CURRENT substrate defines it
  signature?: string;
  definition?: SourceSpan;      // 1-based, UTF-8 byte coordinates
  source_available: boolean;
  source_unavailable_reason?: string;
  substrate_stale: boolean;
}
```

`GET /api/v1/records/{uuid}/source?scope=context|full` serves the trusted text:

- `context` (the default) returns the definition plus 12 lines on each side, capped at 400 lines and
  256 KiB; `full` returns the whole indexed file, capped at 20,000 lines and 1 MiB.
- Caps drop whole lines, so `start_line`/`end_line` always describe the text actually returned, and
  `truncated` says when a cap applied. A single line too large to fit alone is cut at a character
  boundary, since whole-line clipping bottoms out at one line.
- Peak memory follows the window, not the file. The file is streamed and only the lines a scope can
  serve are retained, so previewing 25 lines of a multi-megabyte file costs 25 lines. The hard read
  ceiling bounds the streaming; the scope caps bound what is held. Both bounds apply to the read
  itself, never to a preceding size check — a file can grow between a `stat` and the read that
  follows it, which is exactly when a concurrent writer makes the limit matter.
- Availability is decided from file metadata alone, so describing an indexed record costs no file
  read, and a file past a hard read ceiling is declined rather than loaded. A syntactic-fallback
  entity is the exception, and unavoidably so: its declaration range exists nowhere but the file, so
  locating it costs one parse, bounded by its own ceiling and cached by mtime. Record detail is a
  single-record route, so that parse never fans out across a listing.
- The route answers `503` with the reindex explanation when source cannot be trusted, `400` for an
  unknown scope or a non-`CodeEntity` record, and `404` for an unknown UUID.
- It answers `403` unless the request is same-origin AND names this machine by address. The
  knowledge API's CORS policy is permissive, but source text is the one payload where that would let
  any page a developer visits read their working tree from a localhost daemon. Comparing `Origin` to
  `Host` is not sufficient on its own: both are caller-controlled, so a page whose DNS is rebound to
  loopback sends matching attacker-chosen values — and because that fetch is same-origin to the
  browser, it carries no `Origin` at all. A present `Host` must therefore be `localhost` or an
  address literal in every case; a DNS name is refused. Requests with no `Host` are not browser
  requests and are served.

`SourceSpan` mirrors the substrate: the start is inclusive and the end is EXCLUSIVE. A declaration
ending at column 1 of a line contains none of that line, so a renderer highlighting an inclusive
range would mark one line too many.

## Curated recipes and migration

Generated runs are ephemeral unless saved. Draft and published recipes are version-controlled JSON
files outside the project knowledge graph. They contain only instructional metadata and references:

```ts
interface StoryRecipe {
  schema_version: 3;
  id: string;
  title: string;
  subject: StoryRecipeSubject;
  goal: string;
  audience: "reboarding";
  focus: {
    include_record_iris: string[];
    exclude_record_iris: string[];
    include_code_symbols: string[];
    exclude_code_symbols: string[];
    emphasis: StorySectionKind[];
  };
  curator_context?: string;
  status: "draft" | "published";
  curator: string;
  updated_at?: string;
}
```

Include and exclude collections are unique, disjoint, and limited to 128 entries each. Included
records must belong to the subject's typed closure and receive priority in evidence allocation.
Exclusions suppress prose, but cannot remove the minimal lifecycle data needed to keep chronology
honest. `emphasis` is a unique subset of the five section kinds; it changes prose allocation, never
truth, chronology, or evidence eligibility. `curator_context` is limited to 2,000 characters and is
rendered as non-authoritative maintainer guidance.

Published recipes require a resolvable subject and resolvable, subject-connected references.
Historical anchors are allowed when visibly lifecycle-labelled. Proposed anchors remain gaps. At
most one published recipe is authoritative for a normalized subject. `updated_at` is a daemon-issued
optimistic-concurrency token; stale saves and publication attempts are rejected.

Readers accept schema v1 and v2 recipes without rewriting files. The next successful save migrates
them to v3 by:

1. unioning their ordered record and code anchors into the corresponding include lists;
2. mapping route intents to section emphasis as `purpose` → `orientation`, `boundary` →
   `current_state`, `core-code` → `implementation`, `governance` → `evolution`, and `risk` →
   `implications`, then preserving first-occurrence order;
3. combining curator notes in original order, prefixed by their former titles, into
   `curator_context`; if the result exceeds 2,000 characters, rejecting the save with an actionable
   validation error rather than truncating it; and
4. creating empty exclusion lists.

Only schema v3 is written after migration. Recipe summaries no longer expose a beat count.

## Honest degradation and invariants

- Without an LLM, the complete symbolic article, chronology, evidence, gaps, and checks remain
  available.
- Without a usable code substrate, the knowledge narrative remains available and code coverage is
  reported as reduced.
- Source that cannot be proven current is withheld and explained. Degrading to a warning is correct;
  degrading to approximately-right source is not.
- Missing, stale, proposed, rejected, superseded, and deprecated knowledge is represented according
  to its lifecycle state and never silently promoted.
- Generating, reading, narrating, saving, publishing, or completing a Story never writes the project
  knowledge graph.
- A discovered gap enters capture and ratification only through an explicit user action in the
  existing workflow.
- Story introduces no ontology classes or properties and does not modify capture behavior.

## Acceptance criteria

- Every eligible component, information record, or code entity produces a cohesive grounded article
  without an LLM; a topic does so only when bounded retrieval clears the relevance floor.
- Direct relationships, typed record clusters, code relationships, and complete bidirectional
  supersession histories are included deterministically with cycle protection.
- Full selected record descriptions remain inspectable and can inform assisted narration.
- The narrative, timeline, citations, evidence appendix, gaps, and checks agree on one evidence
  fingerprint and lifecycle classification.
- Timeline ordering is stable; dated evolution and supersession are visible, and undated events are
  clearly separated.
- A bounded narration packet retains subject-focused early, middle, latest, and supersession
  milestones before incidental recent feature records, so truncation cannot silently turn recency
  into project history.
- Assisted narration validates every short source ID and its expanded evidence union, uses one
  adaptive bounded synthesis request, and falls back without losing evidence on any failure.
- The reader presents one article surface, correct typed evidence links, a separate timeline and gaps
  section, and a collapsible evidence appendix; it does not render one card per former beat.
- A narration upgrade preserves issued checks and all in-progress or completed answers.
- Recipes save, curate, publish, reload, migrate from v1/v2, and enforce grounding, uniqueness,
  optimistic concurrency, and filesystem containment.
- Question order and handles do not reveal answers, and grading revalidates current graph state.
- Story activity leaves the project graph byte-for-byte unchanged and adds no ontology or capture
  behavior.
- A knowledge-bearing hover carries a Story link for the exact entity under the cursor, omits it when
  no workbench is serving, and leaves the hover-silence policy unchanged.
- `#/stories/entity/{uuid}` survives a refresh, resolves through the record API, and tells the Story
  of the exact returned entity.
- A `CodeEntity` record page shows its kind, signature, path, and a line-numbered definition preview
  with a full-file expansion; a stale or unverifiable index yields metadata and an explanation with
  no preview; and source is never readable cross-origin.

## Deferred

- PR- or diff-specific Stories.
- Adaptive tutoring and free-text answer grading.
- Team-wide learning analytics or competency scoring.
- Story ontology classes.
- Automatic conversion of Story narration into accepted project knowledge.
