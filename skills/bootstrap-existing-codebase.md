# Skill: Bootstrap an existing codebase into MOOSEDev

**Goal:** recover the *why* of an existing codebase — the architectural decisions,
constraints, lessons, and patterns that aren't obvious from the source — and record them as
**typed, queryable knowledge** in MOOSEDev's durable project graph.

This is how a project that has accumulated **comprehension debt** (CLAUDE.md invariant #3)
gets an initial knowledge graph. New work then *extends* that graph instead of starting from
an empty store (invariant #10). It is also MOOSEDev's own end-to-end exercise of the
capture → align → validate → query loop.

> **Audience:** a coding agent (Claude Code, Codex, …) with the MOOSEDev MCP server
> attached. It is a workflow, not code. Follow the phases in order.

---

## When to use

- Onboarding MOOSEDev onto a project that already has substantial history.
- A codebase whose design rationale lives only in people's heads, scattered docs, or git
  history — and is at risk of being lost.

**When *not* to use:** a brand-new/empty project (there's no accumulated rationale to
recover yet — just capture decisions as you make them), or to mass-import low-value notes.

## What "done" looks like

A handful to a few dozen **high-signal** typed records exist in the graph, they
`validate_against_architecture` with **0 violations**, and they come back from `query` /
`get_relevant_context` / `sparql`. Quality over quantity — a small set of load-bearing
decisions beats a flood of restated code.

---

## The typed knowledge you capture

Every record is a typed instance written with `record_important_decision`
(`kind`, `title`, `description`, optional `status`). Pick the `kind` that fits:

| `kind`                 | Capture this                                              |
|------------------------|----------------------------------------------------------|
| `ArchitecturalDecision`| A choice about structure and **why** it was made (and what was rejected). The default. |
| `Constraint`           | A hard limit / invariant the system must respect (platform cap, security boundary, perf budget). |
| `Requirement`          | A goal or need the system exists to satisfy.             |
| `Pattern`              | A recurring design approach used deliberately across the code. |
| `AntiPattern`          | Something to avoid here, and why (often a lesson learned the hard way). |
| `Lesson`               | A non-obvious learning / gotcha worth remembering.       |

These are the canonical kinds; the loaded ontology is the source of truth. An unknown
`kind` is rejected — if a term doesn't obviously map, run `align_concepts` first (Phase 3)
to find the right class rather than guessing.

---

## What to capture vs. skip

**Capture** durable *why* — the reasoning that the code cannot tell you:

- the decision **and its rationale** (why this, why not the alternative);
- constraints that explain otherwise-puzzling code;
- lessons/anti-patterns that prevent repeating a past mistake.

**Skip** what the code or git already record, and what won't matter next month:

- code *structure* (modules, signatures) — that's recoverable by reading the code;
- line-level mechanics, restating what a function does;
- transient implementation detail or chatter.

**Discipline (invariant #2 — structured over free text):**
- **One fact per record.** Don't pack three decisions into one.
- Title = the claim in a line; description = the *why* + what was rejected + evidence
  (e.g. the file/commit it came from).
- Prefer the typed `kind` over a vague decision. Structured beats prose.

## Where to mine rationale

Rationale hides in: design docs / ADRs / RFCs, `CHANGELOG`, "why" code comments (not
"what"), **commit messages and PR descriptions**, issue/discussion threads, naming
conventions, and contributor docs (`CONTRIBUTING`, `CLAUDE.md`, `README`). Git history is
often the richest source of *why*.

---

## Workflow

### Phase 0 — Connect & recall (always first)

1. `ping` → expect `pong`. If it fails, the backend isn't attached — stop and fix wiring
   (see the README "Shared mode" section) before continuing.
2. **Recall first.** `get_relevant_context` with **no `topic`** (list-all) to see what's
   already recorded. You are *extending* the graph — don't duplicate existing records. If a
   record exists but is now wrong, plan to `supersede_decision` it (Phase 4), not duplicate.

### Phase 1 — Survey the codebase

Build a mental map of the project and where rationale lives. **Use subagents** for the
reading-heavy work so the main context stays clean (CLAUDE.md "Subagent Strategy") — e.g.
one agent summarizes docs/ADRs, one mines `git log`/PRs for decisions, one notes
constraints/patterns from the code and build config. Have each return a short list of
**candidate** items with evidence, not file dumps.

Good starting reads: `README`, `CLAUDE.md`/`CONTRIBUTING`, `spec/`/`docs/`/`adr/`, build
manifests (`Cargo.toml`, `package.json`, …), top-level module layout, and
`git log --oneline` / notable PRs.

### Phase 2 — Extract candidates

Consolidate the survey into a deduped list of candidate records. For each: the proposed
`kind`, a one-line title, the *why*, and the evidence (file/commit). Drop anything that
fails the "capture vs. skip" bar above.

### Phase 3 — Align new terms (before recording)

For any concept whose `kind` isn't obviously one of the canonical classes, **align it
first** so the graph doesn't drift (invariant #4):

- `align_concepts(label, definition?, surface_labels?)` → resolves the best-matching class
  (with the sensor + rationale), or returns ranked candidates if ambiguous.
- `suggest_mappings(label, definition?)` → just the ranked candidates, for review.

Record under the resolved class. Don't invent kinds.

### Phase 4 — Capture as typed instances

For each surviving candidate:

```
record_important_decision(
  kind:        "ArchitecturalDecision",          // or the aligned kind
  title:       "Single-writer RocksDB store; one shared backend per project",
  description: "RocksDB holds an exclusive lock, so per-client stdio servers can't share a
                project. Chose one --serve backend + thin --connect proxies. Rejected: N-Quads
                file store (loses transactional coherence). Evidence: tasks/todo.md M5.",
  status:      "accepted"                          // optional; defaults to "proposed"
)
```

- Capture the **rationale and the rejected alternative**, not just the conclusion.
- To **correct** an existing record, never silently duplicate — instead:
  - `supersede_decision(superseded_iri, title, rationale, description?)` when there is a
    **replacement** — preserves the old as history, links the new, and records *why* it
    changed (type-preserving).
  - `retract_decision(iri, rationale)` when the record should simply **no longer apply**
    (e.g. a duplicate, or a decision abandoned with no successor) — marks it `deprecated`,
    captures *why*, and preserves it as history.

### Phase 5 — Validate

Run `validate_against_architecture`. Resolve any reported violations (usually a missing
required field — re-record with the missing piece). Aim for **0 violations** before moving
on.

### Phase 6 — Verify queryability

Prove the knowledge is retrievable (this *is* the acceptance criterion):

- `query("<a question the records should answer>")` → expect a synthesized answer **plus a
  reasoning trace**.
- `get_relevant_context(topic: "<area>")` → expect the relevant records back.
- `sparql("SELECT ?s ?l WHERE { GRAPH <https://moosedev.dev/kg/project> { ?s
  <http://www.w3.org/2000/01/rdf-schema#label> ?l } } LIMIT 50")` → deterministic listing of
  what landed. (Optional: `get_provenance(iri)` to confirm who/when recorded it.)

---

## Anti-goals (read before you start capturing)

- **Symbolic-first.** This workflow composes deterministic capture/align/validate/query
  tools (invariant #1). Don't reach for free-text dumps or external "semantic search" over
  records — `query` (walk planning) and `sparql` are the retrieval path.
- **No record floods.** A few load-bearing decisions > hundreds of restated facts. Noise
  buries signal and is itself a form of comprehension debt.
- **Align before coining.** Introducing inconsistent one-off kinds defeats the graph
  (invariant #4).
- **Don't restate the code.** If a fact is recoverable by reading the source or `git log`,
  it doesn't belong here unless you're capturing the *why* behind it.

## Done checklist

- [ ] `ping` healthy; recalled existing context first (no duplicates).
- [ ] Candidates pass the capture-vs-skip bar; new terms aligned.
- [ ] Typed records written with rationale (+ rejected alternatives).
- [ ] `validate_against_architecture` → 0 violations.
- [ ] `query` / `get_relevant_context` / `sparql` return the recorded knowledge.
