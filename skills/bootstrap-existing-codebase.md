# Skill: Bootstrap an existing codebase into MOOSEDev

**Goal:** recover the *why* of an existing codebase — the architectural decisions,
constraints, lessons, and patterns that aren't obvious from the source — and record them as
**typed, LINKED, queryable knowledge** in MOOSEDev's durable project graph.

This is how a project that has accumulated **comprehension debt** (CLAUDE.md invariant #3) gets
an initial knowledge graph. The point is **not a pile of records** — it is a **traversable
graph**: a decision links to the requirement that motivated it, the constraint that shaped it,
the alternatives weighed, the consequences that resulted, and the components it touches. Flat
keyword retrieval finds a single record; only a *linked* graph lets you follow "why?" from a
decision to rationale that lives in a lexically-distant record. New work then *extends* that
graph instead of starting from an empty store (invariant #10).

> **Audience:** a coding agent (Claude Code, Codex, …) with the MOOSEDev MCP server attached.
> It is a workflow, not code. Follow the phases in order.

---

## When to use

- Onboarding MOOSEDev onto a project that already has substantial history.
- A codebase whose design rationale lives only in people's heads, scattered docs, or git
  history — and is at risk of being lost.

**When *not* to use:** a brand-new/empty project (capture decisions as you make them instead),
or to mass-import low-value notes.

## What "done" looks like

A handful to a few dozen **high-signal** typed records, **linked into clusters** (not a flat
bag):

- they `validate_against_architecture` with **0 violations**;
- the relationship **edge count is ≳ the node count** (the graph is connected, not flat);
- the **competency questions** (Phase 7) return real multi-hop answers; and
- `get_relevant_context` returns a seed record **plus its linked neighbors**.

Quality + connectivity over quantity — a small set of load-bearing, *interconnected* decisions
beats a flood of restated, disconnected code.

---

## The typed NODES you capture

Every node is a typed instance written with `record_important_decision`
(`kind`, `title`, `description`, optional `status`). It returns the new record's **IRI** — keep
it (you need it to draw edges in Phase 5).

| `kind`                 | Capture this                                                        |
|------------------------|---------------------------------------------------------------------|
| `ArchitecturalDecision`| A choice about structure/behavior and **why** (the cluster's hub). Default. |
| `Requirement`          | A goal/driver the decision exists to satisfy.                        |
| `Constraint`           | A hard limit/invariant the design must respect (platform cap, security boundary, perf budget). |
| `Alternative`          | A different option that was considered and rejected.                |
| `Consequence`          | An outcome / accepted trade-off of a decision.                      |
| `SystemComponent`      | A code module/element that decisions and constraints act upon.       |
| `Pattern`              | A recurring design approach used deliberately.                      |
| `AntiPattern`          | Something to avoid here, and why.                                   |
| `Lesson`               | A non-obvious learning / gotcha worth remembering.                  |

The loaded ontology is the source of truth for kinds. An unknown `kind` is rejected — if a term
doesn't obviously map, run `align_concepts` first (Phase 3). (`Rationale` nodes are created
*automatically* by supersede/retract — you don't mint them.)

## The EDGES you build (the heart of the graph)

After recording nodes, connect them with `relate(subject_iri, predicate, object_iri)` — the
`predicate` is an ontology object-property **local name**. Each edge is **validated against the
ontology's SHACL shapes**: a backwards or mistyped edge (wrong endpoint kinds, wrong direction)
is **rejected and nothing is written**. So every edge must match this map:

```
ArchitecturalDecision --isMotivatedBy--> Requirement | Constraint   (why)
ArchitecturalDecision --weighs-->        Alternative                (what was rejected)
ArchitecturalDecision --resultsIn-->     Consequence                (the trade-off)
ArchitecturalDecision --concerns-->      SystemComponent            (what it touches)
Constraint            --constrains-->     ArchitecturalDecision | SystemComponent
AntiPattern           --violates-->       Constraint
Lesson                --learnedFrom-->    ArchitecturalDecision | AntiPattern
```

`supersedes` and `hasRationale` are created **automatically** by `supersede_decision` /
`retract_decision` — never `relate()` those by hand.

> A rejected `relate()` means the kinds/direction don't match the map — fix the cluster, don't
> force it. Keep `isMotivatedBy` homogeneous per decision: if both a requirement and a
> constraint apply, use `isMotivatedBy`→Requirement and `constrains`→the decision.

---

## What to capture vs. skip

Think in **clusters**, not isolated facts. **Capture** durable *why* the code can't tell you —
a decision and the requirement/constraint/alternative/consequence/component around it; lessons
and anti-patterns that prevent repeating a mistake.

**Skip** what the code or git already record, and what won't matter next month:
- code *structure* (modules, signatures) — recoverable by reading the code (the exception:
  a `SystemComponent` node minted only as an *anchor* for `concerns`/`constrains` edges);
- line-level mechanics, restating what a function does; transient detail or chatter.

**Discipline (invariant #2 — structured over free text):**
- **One fact per node.** Don't pack three decisions into one.
- **Title = a short handle** (≤ ~80 chars / ~12 words): the concept's NAME — what you'd *say* to
  refer to it ("Adopt RocksDB for the durable store") — **not** the claim, no sentence-long titles
  with dashes/parentheticals/metrics. `rdfs:label` is weighted **2× in retrieval**, so a
  claim-as-title saturates lexical search (self-announcing records BM25 always finds) and bloats
  every list/graph view. **The description LEADS with the one-line claim**, then the *why* +
  **the evidence** (the file/section or commit it came from). `relate()` has no description field,
  so the edge's justification lives in the endpoint nodes' descriptions + your link plan.
- Prefer the typed `kind` and an explicit edge over vague prose.

## Where to mine rationale

Design docs / ADRs / RFCs, `CHANGELOG`, "why" comments (not "what"), **commit messages and PR
descriptions**, issue/discussion threads, naming conventions, `CONTRIBUTING`/`CLAUDE.md`/
`README`. Git history is often the richest source of *why* (and of supersessions).

**Specs are the richest source of `Requirement`s specifically.** Files clearly identifiable as
specifications — under a `spec/` (or `specs/`) directory, or that call themselves a
spec/specification — state intended behavior and constraints *directly*, so they map cleanly to
typed `Requirement` nodes. Mine them deliberately: a graph that under-captures Requirements
leaves its decisions with nothing to be `isMotivatedBy`, which is the most common reason a
bootstrapped graph ends up decision-heavy but motivation-sparse (a flat list of choices with no
recorded *why*). Each mined Requirement becomes a reusable **hub** (link-or-mint, Phase 4) that
many decisions point at — exactly the multi-hop structure traversal depends on.

---

## Workflow

### Phase 0 — Connect, recall, baseline (always first)

1. `ping` → `pong`. If it fails, the backend isn't attached — fix wiring before continuing.
2. **Confirm the store.** For a foreign repo, `get_relevant_context` with **no `topic`** should
   return "No recorded knowledge found." (proves you're on that project's own store, not
   another project's). On an existing store, this list-all is your dedup inventory.
3. **Baseline edge count** (run the Phase 6 histogram once) so you can later show N→M edges.

### Phase 1 — Survey for CLUSTERS (not isolated items)

**Use subagents** for the reading-heavy work (CLAUDE.md "Subagent Strategy"). Have each return
candidate **clusters** with **per-node AND per-edge evidence** — not file dumps. For every
candidate decision, answer the edge-surfacing questions:

- **Why was it chosen?** → a `Requirement` (`isMotivatedBy`)
- **What hard limit forced/shaped it?** → a `Constraint` (`constrains` → the decision)
- **What else was considered and rejected?** → an `Alternative` (`weighs`)
- **What did it cost / what trade-off was accepted?** → a `Consequence` (`resultsIn`)
- **What module does it touch?** → a `SystemComponent` (`concerns`)

And: **what bad pattern would break a constraint?** → `AntiPattern` (`violates`);
**where did a lesson come from?** → `Lesson` (`learnedFrom`).

Source cue-words (work on docs, comments, commits, PRs): "never / must not / inviolable" →
**Constraint**; "because / so that / in order to" → **Requirement**; "rejected / instead of /
we tried X but" → **Alternative**; "trade-off / at the cost of / limitation" → **Consequence**;
"lesson / gotcha / we learned" → **Lesson**; a commit/PR that **reverts or changes** a prior
choice → `supersede_decision`.

Good starting reads: `README`, `CLAUDE.md`/`CONTRIBUTING`, `spec/`/`docs/`/`adr/` (mine `spec/`
especially for `Requirement`s — see "Where to mine rationale"), build manifests, top-level
module layout, `git log --oneline` / notable PRs.

### Phase 2 — Shape each cluster (node table + link plan)

Consolidate the survey into, per cluster, two small tables. **Drop any node or edge that lacks
specific evidence.**
- **Node table:** `kind | title | description (why + evidence: file/section or commit) | status`
- **Link plan:** `subject-title | predicate | object-title | evidence-for-the-edge`

The link plan applies the predicate map above; the edge's evidence must justify the **direction
and the specific pair**, not merely that both nodes exist.

### Phase 3 — Align new TERMS to classes (before recording)

For any node whose `kind` isn't obviously a canonical class, `align_concepts(label, definition?)`
(or `suggest_mappings`) to resolve the class (invariant #4). This is **term → class** resolution
— distinct from **link-or-mint** (instance dedup, Phase 4). They compose: align picks the class;
link-or-mint decides reuse-or-create within it.

### Phase 4 — Capture nodes (link-or-mint) → node registry

Walk the node table. For each node apply **link-or-mint grounding** (below); when you mint:

```
record_important_decision(
  kind:        "Requirement",                          // or the aligned kind
  title:       "Byte-identical KG on unchanged input", // a NAME (≤~80 chars) — not the claim
  description: "A rebuild on unchanged input must emit a byte-identical KG — reproducible diffs for the review/audit workflow. Evidence: spec/SPEC.md §Ingestion.",
  status:      "proposed"                              // default; use "accepted" only when the source states it
)
```

Parse the returned IRI and store it in a **node registry** (`title → IRI`). To **correct** an
existing record, never silently duplicate: `supersede_decision(superseded_iri, title, rationale,
description?)` when there's a replacement (preserves the old, links new→old, records *why*), or
`retract_decision(iri, rationale)` when it should simply no longer apply.

### Phase 5 — LINK (the distinct edge step)

Walk the **link plan**; for each row:

```
relate(subject_iri, predicate, object_iri)   // IRIs from the registry; predicate from the map
```

`relate()` is idempotent and SHACL-validated. A rejected edge names the offending endpoint and
the expected class — that's a signal your cluster is mis-shaped, not a reason to force it.

### Phase 6 — Validate + lint + connectivity (prove it isn't flat)

1. `validate_against_architecture` → expect **0 violations**. **Necessary, not sufficient:** it
   checks required fields and edge *types-if-present* but **never requires edges**, so a *flat*
   graph also passes. Prove linkage with SPARQL.
2. **Edge histogram** (linkage proof):
   ```sparql
   SELECT ?pred (COUNT(*) AS ?n) WHERE {
     GRAPH <https://moosedev.dev/kg/project> {
       ?s ?p ?o . FILTER(isIRI(?o))
       BIND(REPLACE(STR(?p),"^.*[/#]","") AS ?pred)
       FILTER(?pred IN ("isMotivatedBy","weighs","resultsIn","concerns","constrains","violates","learnedFrom","supersedes"))
     }
   } GROUP BY ?pred ORDER BY DESC(?n)
   ```
3. **Edge-type matrix** (confabulation/mistype lint — every row must match the predicate map):
   ```sparql
   SELECT ?pred ?scl ?ocl (COUNT(*) AS ?n) WHERE {
     GRAPH <https://moosedev.dev/kg/project> {
       ?s ?p ?o . FILTER(isIRI(?o)) . ?s a ?sc . ?o a ?oc .
       BIND(REPLACE(STR(?p),"^.*[/#]","") AS ?pred)
       BIND(REPLACE(STR(?sc),"^.*[/#]","") AS ?scl)
       BIND(REPLACE(STR(?oc),"^.*[/#]","") AS ?ocl)
       FILTER(?pred IN ("isMotivatedBy","weighs","resultsIn","concerns","constrains","violates","learnedFrom"))
     }
   } GROUP BY ?pred ?scl ?ocl ORDER BY ?pred
   ```
4. **Orphan check** — records with no typed edge in or out should be ≈ 0.

Targets: ≥1 outbound edge for every `ArchitecturalDecision`; **edge:node ratio ≳ 1.0**.

### Phase 7 — Verify traversability (the acceptance criterion)

- Answer the **competency questions** with `sparql`/`query`, each should return bound rows:
  1. Which decision **supersedes** a previous one? 2. What constraints does an anti-pattern
  **violate**? 3. Which requirements **motivated** a decision? 4. What **consequences /
  alternatives** does a decision have? 5. Which components does a constraint/decision
  **constrain/concern**? 6. What lessons were **learned from** a decision/anti-pattern?
- **Multi-hop proof:** `get_relevant_context(topic: "<a seed record's words>")` should return
  the seed **plus** neighbors tagged `linkedVia: <predicate>` — the lexically-distant *why*
  reached by edge-following, not keyword match.
- **(Operator A/B):** re-running the same query with `MOOSEDEV_EXPAND_HOPS=0` on the serve
  returns **only** the seed (no neighbors). That difference is the traversal value the linked
  graph adds over flat retrieval — the whole point of bootstrapping structure.

---

## Link-or-mint grounding (instance dedup, invariant #4)

Before creating a node, check whether an equivalent **instance** already exists:
`get_relevant_context(topic: "<the node's concept in 3-6 words>")` and inspect returns **of the
same kind**.
- **Match** (a maintainer would call having both a duplicate): **reuse the existing IRI** — do
  not mint. Record the reuse in your registry.
- **No match** (only topically related, or nothing returned): **mint** and capture the new IRI.

Test: *"Would a maintainer say these are the same requirement/constraint, or two different
ones?"* Bias **toward reuse** for `Requirement`/`Constraint` — they recur across many decisions,
and reuse is what creates the **hub nodes** that make the graph multi-hop. Bias **toward mint**
for `Consequence`/`Alternative` — usually specific to one decision.

## Anti-confabulation discipline (invariant #6 — honesty over plausibility)

Edges are not SHACL-*required*, and `get_relevant_context` tells agents to **TRUST** recorded
knowledge — so a fabricated edge is a durable false claim. Therefore:
- **Evidence-or-skip, per edge.** Cite the doc line / code fact / commit that states the
  *relationship* (X drove / violates / resulted-in Y) — not merely that both nodes exist. If you
  can't point to it, **don't draw the edge.**
- **Prefer not-linking over guessing.** A missing edge is honest comprehension debt; a wrong one
  is served as truth forever. **Under-link rather than over-link.**
- **No transitive invention.** Assert only edges the source states or directly implies.
- **Lifecycle honesty.** Leave `status:"proposed"` when the source only *implies* a node/edge;
  reserve `"accepted"` for what the source states outright.

## Anti-goals (read before you start)

- **Symbolic-first.** Compose the capture/align/relate/validate/query tools (invariant #1);
  don't reach for free-text dumps or external "semantic search" over records.
- **No record floods, no edge floods.** A few load-bearing, well-linked decisions beat hundreds
  of restated facts or speculative edges. Noise is itself comprehension debt.
- **Align before coining; link-or-mint before duplicating.** Inconsistent one-off kinds or
  parallel duplicate instances defeat the graph (invariant #4).
- **Don't restate the code.** If a fact is recoverable by reading the source or `git log`, it
  doesn't belong here unless you're capturing the *why* behind it.

## Done checklist

- [ ] `ping` healthy; recalled existing context first (correct store; no duplicates).
- [ ] Candidates shaped into **clusters** (node table + link plan); new terms aligned.
- [ ] Nodes recorded with rationale + evidence; IRIs captured in a registry (link-or-mint applied).
- [ ] Edges drawn with `relate()` per the predicate map; each edge evidence-grounded.
- [ ] `validate_against_architecture` → 0 violations.
- [ ] Edge histogram non-empty; edge-type matrix clean; orphans ≈ 0; **edges ≳ nodes**.
- [ ] Competency questions return rows; `get_relevant_context` shows `linkedVia` neighbors.
