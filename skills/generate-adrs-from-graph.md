# Skill: Generate Architecture Decision Records (ADRs) from the graph

**Goal:** render the project knowledge graph's `ArchitecturalDecision` records — with the
requirements, constraints, alternatives, consequences, and supersession chain linked to each —
as a **standard, human-readable ADR set** under `docs/adr/`. The graph is the source of truth;
the ADR files are a **regenerable view** of it (CLAUDE.md invariant #2).

This is the **inverse of `bootstrap-existing-codebase.md`**: bootstrap *populates* the graph
from the codebase; this skill *reads it back out* as documentation. It is the first of a family
of **artifact-generating** skills — siblings will render constraint catalogs, lessons, and
architecture overviews. **This one is ADR-specific:** it renders decisions and the cluster
linked to each, and nothing else.

> **Audience:** a coding agent (Claude Code, Codex, …) with the MOOSEDev MCP server attached.
> It is a workflow, not code. Follow the phases in order. **It writes only files, never graph
> knowledge** — every MCP call here is read-only (`sparql`, `get_relevant_context`,
> `get_provenance`). Do not `record_important_decision`/`relate`/`supersede_decision` from this
> skill; the graph already holds the truth you are rendering.

**Preferred implementation:** run the checked-in generator instead of recreating the rendering
logic:

```bash
scripts/generate-adrs-from-graph.py --check
```

The script implements this skill's COUNT → enumerate → batched cluster SPARQL workflow against
the local MOOSEDev HTTP backend (`.moosedev/http.addr`) and writes `docs/adr/`. Use the phases
below as the contract for what the script must do, for reviewing its output, or as a manual
fallback if the script is unavailable. If the backend is not running, start it first
(`moosedev --serve`) or pass `--addr HOST:PORT`.

---

## When to use

- A MOOSEDev-backed project wants a conventional `docs/adr/` set (adr-tools / MADR / Log4brains
  layout) generated from its captured decisions — for onboarding, review, or publication.
- Refreshing an existing ADR set after new decisions were captured or old ones superseded.

**When *not* to use:**
- The graph holds few or no `ArchitecturalDecision`s — bootstrap or capture decisions first
  (`bootstrap-existing-codebase.md`), then render.
- You want to document constraints, patterns, anti-patterns, or lessons that aren't tied to a
  decision — those are **out of scope here**; a sibling artifact skill renders them. (A
  constraint/requirement that *motivates* a decision still appears, as that ADR's *Context*.)

## What "done" looks like

A `docs/adr/` directory that is a **faithful, auditable mirror** of the graph's decisions:

- **one `NNNN-<slug>.md` per `ArchitecturalDecision`** (every lifecycle status, including
  superseded/deprecated — the log's value *is* the evolution), plus a `0000-index.md` log;
- the **count of ADR files == the count of `ArchitecturalDecision`s** in the graph;
- every **"Superseded by ADR-MMMM"** resolves to a real file, with the reciprocal
  **"Supersedes ADR-KKKK"** on its partner;
- **every claim traces to a graph IRI**, and a section with no graph data reads
  "*not recorded*" — never invented (invariant #6).

---

## The graph → ADR mapping (what fills each section)

Each `ArchitecturalDecision` (the ADR) is rendered from its node literals plus its 1-hop cluster:

| ADR field | Graph source (predicate → node) |
|-----------|---------------------------------|
| Title | `rdfs:label` (the node's name) |
| Status | `hasLifecycleStatus` → mapped (table below) |
| Date | `hasTimestamp` (render as `YYYY-MM-DD`) |
| Author | `hasAuthor` (fallback: `get_provenance`) |
| **Context** (drivers) | `isMotivatedBy` → `Requirement`/`Constraint`; **inbound** `constrains` (a `Constraint` shaping this decision) |
| **Decision** (the why) | `hasRationale` → `Rationale` node's `hasDescription`; lead with the AD's own `hasDescription` |
| **Considered Options** | `weighs` → `Alternative` |
| **Consequences** | `resultsIn` → `Consequence` |
| **Affects** | `concerns` → `SystemComponent` |
| **Supersedes** ADR-KKKK | outbound `supersedes` → the older AD |
| **Superseded by** ADR-MMMM | outbound `isSupersededBy` (materialized inverse) → the newer AD |

> Why both `isMotivatedBy` *and* inbound `constrains` feed Context: the bootstrap discipline
> keeps `isMotivatedBy` homogeneous (→ `Requirement`) and models a shaping constraint as
> `Constraint --constrains--> Decision`. So a decision's drivers live on **both** edges.

**Status mapping** (`hasLifecycleStatus` → ADR `Status`):

| graph status | ADR Status line |
|--------------|-----------------|
| `proposed`   | `Proposed` |
| `accepted`   | `Accepted` |
| `deprecated` | `Deprecated` |
| `superseded` | `Superseded by [ADR-MMMM](MMMM-<slug>.md)` (resolve MMMM via `isSupersededBy`) |

## ADR file template

```markdown
# NNNN. <AD title>

- Status: <Proposed | Accepted | Deprecated | Superseded by [ADR-MMMM](MMMM-<slug>.md)>
- Date: <YYYY-MM-DD>
- Author: <hasAuthor>
- Supersedes: [ADR-KKKK](KKKK-<slug>.md)        <!-- only if this AD supersedes another -->

## Context
<One bullet per driver, from isMotivatedBy → Requirement/Constraint and inbound constrains:
 the driver's claim + `(IRI)`. If none recorded: "No motivating requirement or constraint recorded.">

## Decision
<The AD's hasDescription lead line, then the Rationale node's text (hasRationale).
 If no Rationale node exists: render hasDescription and add "No separate rationale recorded.">

## Considered Options
<One item per weighs → Alternative (label + description). If none: "No alternatives recorded.">

## Consequences
<One item per resultsIn → Consequence. If none: "No consequences recorded.">

## Affects
<concerns → SystemComponent labels. OMIT this whole section if there are none.>

---
Source: graph record `<AD IRI>`. Generated view — regenerate from the graph; do not hand-edit.
```

## Index file template (`0000-index.md`)

```markdown
# Architecture Decision Records

> **Generated view.** Rendered from the MOOSEDev knowledge graph on <YYYY-MM-DD>.
> The graph is the source of truth — **regenerate, do not hand-edit.** Scope: architectural
> decisions only; constraints, patterns, and lessons are rendered by sibling artifact skills.

| #    | Title | Status | Date |
|------|-------|--------|------|
| 0001 | <title> | Accepted | 2026-06-18 |
| 0002 | <title> | Superseded by 0007 | 2026-06-19 |
| …    | … | … | … |
```

---

## Workflow

### Fast path — checked-in generator

1. Confirm the MOOSEDev backend is running and has published `.moosedev/http.addr`.
2. Run:
   ```bash
   scripts/generate-adrs-from-graph.py --check
   ```
   Optional flags:
   - `--json` prints the coverage/lifecycle summary as JSON.
   - `--batch-size K` changes the bounded cluster query size.
   - `--addr HOST:PORT` targets a specific backend if `http.addr` is unavailable.
3. Report the script summary: ADR file count, index path, count check, supersede chains, and
   missing graph fields surfaced as comprehension debt.

Do not write a one-off generator unless you are modifying `scripts/generate-adrs-from-graph.py`
itself. Keeping the renderer checked in prevents each agent from re-implementing subtly
different markdown, status, and verification rules.

### Phase 0 — Connect & inventory

1. `ping` → `pong`. If it fails, the backend isn't attached — fix wiring before continuing.
2. **Count decisions across all lifecycle statuses** with `sparql`:
   ```sparql
   PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
   SELECT (COUNT(?ad) AS ?n) WHERE {
     GRAPH <https://moosedev.dev/kg/project> { ?ad a :ArchitecturalDecision . }
   }
   ```
   **Zero →** write only `0000-index.md` stating "No architectural decisions recorded yet."
   and stop. Do **not** fabricate ADRs (honest empty state — invariant #6). A **non-zero count
   sizes the batched render** in Phase 2 — pick a batch size K so one batch of decisions plus
   their clusters comfortably fits the context window.

### Phase 1 — Enumerate & number (the stable ordering)

```sparql
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?ad ?title ?status ?ts ?author WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    ?ad a :ArchitecturalDecision ;
        rdfs:label ?title ;
        :hasLifecycleStatus ?status ;
        :hasTimestamp ?ts .
    OPTIONAL { ?ad :hasAuthor ?author }
  }
} ORDER BY ?ts ?ad
```

Assign numbers **`0001`, `0002`, …** in this row order — sort by `hasTimestamp` ascending,
tiebroken by IRI (`?ad`), so the same graph always yields the same numbers. Build a registry
`AD-IRI → NNNN` (and a `slug` from the title: lowercase, non-alphanumerics → `-`). Numbers are
**derived fresh each run and never written back** to the graph — the generator stays read-only
and the doc stays a pure view. (Caveat: stable under normal append-only capture; a *backdated*
record can renumber later ADRs — regeneration is cheap and expected.)

### Phase 2 — Fetch clusters in bounded batches (sized from the count)

Do **not** pull every decision's cluster at once: on a large graph the descriptions alone can
overrun the context window — the same failure mode as dumping the whole graph. Phase 1 stayed
light on purpose (no descriptions), so the *enumeration* is cheap at any size; the heavy text
(rationale, alternatives, consequences) is fetched here, **a batch at a time**.

Use the Phase-0 count to choose a batch size **K** (default ~20; lower it if descriptions run
long). Walk the numbered list in batches of K; for each batch: fetch its clusters → render its
files (Phase 3) → discard, then move to the next. The working set stays bounded no matter how
big the graph is — a small project is a **single batch** (`ceil(N/K)=1`); a 600-record graph is
`ceil(N/K)` bounded reads. (Never substitute `export_graph` — see the tooling note below.)

Fetch one batch with a `VALUES`-scoped query (it also returns each decision's **own**
description as a `self` row — the Decision lead line):

```sparql
PREFIX : <https://trivyn.io/ontologies/software/architecture/domain/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?ad ?dir ?rel ?node ?nlabel ?ndesc WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    VALUES ?ad { <…the K AD IRIs in this batch…> }
    {
      ?ad ?p ?node . FILTER(isIRI(?node))
      BIND("out" AS ?dir)
      BIND(REPLACE(STR(?p), "^.*[/#]", "") AS ?rel)
      FILTER(?rel IN ("isMotivatedBy","weighs","resultsIn","concerns",
                      "hasRationale","supersedes","isSupersededBy"))
      OPTIONAL { ?node rdfs:label ?nlabel }
      OPTIONAL { ?node :hasDescription ?ndesc }
    } UNION {
      ?node :constrains ?ad .
      BIND("in" AS ?dir) BIND("constrains" AS ?rel)
      OPTIONAL { ?node rdfs:label ?nlabel }
      OPTIONAL { ?node :hasDescription ?ndesc }
    } UNION {
      ?ad :hasDescription ?ndesc .
      BIND("self" AS ?dir) BIND("hasDescription" AS ?rel) BIND(?ad AS ?node)
    }
  }
} ORDER BY ?ad ?dir ?rel
```

Group the batch's rows by `?ad` and route each into that decision's template: the `self` row →
the **Decision** lead line; `hasRationale` → the rest of **Decision**; `isMotivatedBy` + inbound
`constrains` → **Context**; `weighs` → **Considered Options**; `resultsIn` → **Consequences**;
`concerns` → **Affects**; `supersedes`/`isSupersededBy` → the **Status** and **Supersedes** lines
(resolve the partner's `NNNN` from the registry). **Phase 1's enumeration is the authoritative
AD list** — a decision with no cluster rows still gets an ADR file with empty sections; it is
never dropped.

### Phase 3 — Per-file rendering rules (applied to each batch)

For each decision in the batch, write `docs/adr/NNNN-<slug>.md` from the template — then move on
to the next batch, holding only one batch of clusters in memory at a time. Map the status; wire
supersede links to the partner's number. **Render only fields the graph actually holds** — an
empty relation becomes its "*…not recorded*" line (or, for **Affects**, omit the section). End
each file with the `Source: graph record <IRI>` provenance line.

### Phase 4 — Render the index

Write `docs/adr/0000-index.md` from the index template: the generation banner ("generated
view — regenerate, do not hand-edit"), the scope note (ADRs only; sibling skills cover other
artifacts), and the decision-log table (`# | Title | Status | Date`) in number order. For a
superseded row, render `Superseded by NNNN` in the Status cell.

### Phase 5 — Verify (the acceptance criteria)

1. **Coverage:** the Phase-0 `COUNT` == number of `NNNN-*.md` files == index table rows.
2. **Lifecycle integrity:** every "Superseded by ADR-MMMM" points to an existing file, and its
   partner carries the reciprocal "Supersedes ADR-KKKK". Re-run the cluster query for any
   superseded AD to confirm the link came from the graph, not a guess.
3. **No fabrication:** spot-check 2 ADRs field-by-field against their Phase-2 query output —
   every rendered claim has a backing row; every "*not recorded*" line truly has no row.
4. **Links & banner:** intra-set markdown links resolve; the index carries the banner + scope
   note.

**Tooling & read budget.** `sparql` is the only read primitive you need here — deterministic
structural reads (invariant #1); use `get_provenance` only if author/date is missing from a
node. The two query shapes above (light enumerate + batched cluster) are the entire data budget.
**Never `export_graph`:** it is the backup / version-control serialization primitive (it dumps
whole named graphs to N-Quads), *not* a read path — pulling the whole graph into context to
render a doc defeats the symbolic, token-efficient memory this skill renders from (invariants #1
and #5; Constraint `aa8b3fa3` — "a budgeted reach to answer-bearing records, never a
neighborhood dump"). This skill **writes only files** (the ADRs); it never calls a graph-write
tool.

---

## Anti-confabulation discipline (invariant #6 — honesty over plausibility)

`get_relevant_context` tells agents to **TRUST** recorded knowledge, so a rendered ADR is read
as truth. Therefore:

- **Render-or-mark, never invent.** Every Context/Decision/Option/Consequence line must trace
  to a graph row. No motivating requirement? The Context says so — you do **not** infer one
  from the code or the title.
- **No cross-decision links the graph doesn't assert.** Only render `supersedes`/`isSupersededBy`
  edges that the cluster query returns. Don't connect two ADRs because they "seem related."
- **Don't enrich from the source tree.** This skill renders the *graph*. If a decision's
  rationale is thin, that thinness is honest comprehension debt to surface — not a prompt to
  go read the code and write the rationale yourself. (Capturing missing rationale is a separate
  job for the bootstrap/capture skills, which *write* the graph.)
- **Include history honestly.** Superseded and deprecated decisions are rendered with their
  real status, not dropped — the evolution is the point of an ADR log.

## Report

End with a short summary: number of ADR files written, the index path, the count check
(graph decisions == files == index rows), supersede chains rendered (`KKKK → MMMM`), and any
decisions whose Context/Decision came back "*not recorded*" (surfaced as comprehension debt,
not padded). State plainly what you rendered and what the graph was missing.
