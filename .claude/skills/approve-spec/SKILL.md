---
name: approve-spec
description: Capture the Requirements, Constraints, and settled decisions a specification states into MOOSEDev as typed, linked records at the moment the spec is approved for implementation. Use when the user approves/ratifies a spec, or asks to implement/build a spec that has no records in the graph yet.
---

# Skill: Approve a spec (mint the Requirement hubs)

A spec becomes binding at **approval**. That is the moment its stated behaviour should enter the
graph as typed `Requirement` and `Constraint` records — because the implementation decisions
captured over the following days need those hubs to be `isMotivatedBy`. Capture them later and
the decisions are already recorded with nothing to point at.

This is the **ongoing-workflow half** of Pattern `a2bf1670` ("Mine spec files for Requirements
during bootstrap"), whose bootstrap half lives in
`.claude/skills/bootstrap-existing-codebase/SKILL.md:125-132`. That skill remains the
**authoritative discipline** — especially **Link-or-mint grounding** (:298-310) and
**anti-confabulation** (:312+). Everything below scopes it to one spec at approval time.

> **A graph that under-captures Requirements leaves its decisions with nothing to be
> `isMotivatedBy`.** That is the failure this skill exists to prevent, not a nice-to-have.

---

## Two rules that are not negotiable

**1. Nothing is written before the human says yes.** Phases 0-2 are read-only. You extract,
dedup, and *show* — then you stop and ask. This is the only human gate that exists (see rule 2),
so it cannot be skipped, batched, or inferred from enthusiasm.

Being *asked to implement* a spec is **not** approval. "Implement Spec A", "let's build the
X spec", "start on X" — these tell you the spec is **worth checking**, nothing more. Treat them
as a trigger to run Phase 0, and if the spec has no records, to offer capture. The user has not
seen what you would write, and Lesson `46a3589d` is explicit: any surface asking a human to
approve something must show the content being approved. If they decline, write nothing and get
on with the implementation.

**2. Records are minted `accepted`, so there is no safety net.** `proposed` records are excluded
from the working set (`src/graph/lifecycle.rs:31`) and therefore invisible to
`get_relevant_context` — a proposed Requirement is not a usable hub (AD `3b0c7d59`). These
records go straight into authoritative recall with no ratification inbox behind them. Mint
accordingly: fewer, better, evidence-backed.

---

## Workflow

### Phase 0 — Idempotency + inventory (read-only)

1. Resolve the spec's path. If the user named a spec loosely, confirm which file you mean before
   reading anything else.
2. **Has it already been approved?** Match the **approval marker**, never the bare path — many
   records legitimately mention a spec they were derived from or argue about, and matching the
   path alone reports "already approved" for any spec that has merely been *discussed*:
   ```sparql
   PREFIX a: <https://trivyn.io/ontologies/software/architecture#>
   SELECT ?s ?title WHERE { ?s a:hasTitle ?title ; a:hasDescription ?d .
                            FILTER(CONTAINS(?d, "spec-approval: <spec path>")) }
   ```
   Already approved and the spec is unchanged → say so and stop. Unchanged means unchanged; do
   not re-mint "just in case". If the spec has been **revised** since, go to **Re-approval**.
3. **Which way does this spec point?** A spec written *before* the decisions states intent, and
   its Requirements are genuinely new — that is the case this skill is for. A spec written
   *from* an already-accepted decision cluster is **downstream of the graph**, and mining it
   manufactures near-duplicates of records that already exist. Tell them apart by reading the
   spec's own preamble: citations of existing record IRIs, "operationalizes the accepted …
   cluster", or a precedence clause like *"where this document and the graph disagree, the graph
   wins"* all mean downstream. Say so and stop — offer capture only for the spec's **net-new**
   decisions, if it names any. Do not mine a spec that declares the graph authoritative over
   itself.

4. **Dedup inventory — the hub set, not everything.** Use `sparql`, not a 100-record list-all:
   dedup only needs existing `Requirement`/`Constraint` titles and IRIs, and the compact listing
   is both complete and cheap enough to hold while you work.
   ```sparql
   PREFIX a: <https://trivyn.io/ontologies/software/architecture#>
   SELECT ?kind ?title ?s WHERE {
     { ?s a a:Requirement . BIND("Requirement" AS ?kind) } UNION
     { ?s a a:Constraint  . BIND("Constraint"  AS ?kind) }
     ?s a:hasTitle ?title ; a:hasLifecycleStatus "accepted" .
   } ORDER BY ?kind ?title
   ```
   Note this is deliberately scoped to `accepted` — superseded and deprecated hubs must not be
   reused as link targets.

### Phase 1 — Extract (read-only)

Read the spec and pull candidates:

| kind | what qualifies |
|---|---|
| `Requirement` | intended behaviour, a goal the system must meet, a need the spec exists to serve |
| `Constraint` | a hard limit or invariant: perf budget, security boundary, compatibility rule, "must never" |
| `ArchitecturalDecision` | a choice the spec **already settles**, with its why — plus `alternatives_considered` / `consequences` when the spec states them |

- **Titles are handles, not claims** (≤ ~80 chars, no sentence-long titles). `rdfs:label` is
  weighted **2× in retrieval**, so packing the claim into the title makes lexical recall
  trivially saturate on self-announcing records. "Reversible schema migrations", not "All schema
  migrations must provide a reversible down-path before merge".
- **The claim leads the `description`**, then the evidence: the spec file and section it comes
  from.
- **Do not pad.** A vague or exploratory spec yields few records, and that is the honest
  outcome. Aspirational prose is not a Requirement; a sentence with no testable content is not a
  Constraint.
- Skip anything the spec merely *discusses*. Capture what it **states**.

### Phase 2 — Dedup before showing (read-only, highest-risk step)

For each candidate, `get_relevant_context(topic: "<the concept in 3-6 words>")` and inspect
returns **of the same kind**. Apply the maintainer test from bootstrap :307-310: *would a
maintainer say these are the same requirement, or two different ones?* Bias **toward reuse** for
`Requirement`/`Constraint` — they are hubs, and reuse is what makes the graph multi-hop.

Mark every candidate **MINT** or **REUSE(iri)**.

> A near-duplicate hub is worse than the gap you are filling: future `isMotivatedBy` edges split
> across two nodes that should have been one, so traversal from either returns a partial
> dependent set and neither node is right.

Run `align_concepts(label, definition)` before introducing a genuinely new term, so the model
graph does not drift (invariant #4).

### Phase 3 — Confirm (the gate)

Show the user, grouped by kind: **title + a one-line claim** each, with REUSE candidates marked
as already present and not to be written. State the total that will be written. Then ask for a
yes.

```
Spec <name> has no records in the graph. From <path> I can capture:

  Requirements (5 new, 2 already recorded)
    NEW    Editions split across sqlite and pg
           - each edition owns its store; no cross-edition joins
    REUSE  Annotation scan must not whitelist predicates  (Requirement/8f3a…)
    ...
  Constraints (2 new)
    NEW    Scan stays under 200ms at 100k quads
           - hard perf budget stated in §4.2
    ...

Approve the spec and record these 7 as accepted?
```

**No** → write nothing, say so plainly, continue with the task.

### Phase 4 — Mint, hubs first

`record_important_decision(kind, title, description, status: "accepted", relations)`.

Order matters: **Requirements → Constraints → ArchitecturalDecisions**, so inline `relations` on
later records can target IRIs minted earlier in the same pass.

- ADs the spec settles: `isMotivatedBy` the Requirement they serve; `alternatives_considered`
  and `consequences` in the SAME call when the spec states them.
- Constraints: `constrains` the record or component the spec binds them to.
- `concerns` the `SystemComponent` the spec touches, when one exists. If the store has no
  SystemComponents, skip it — do not invent one to satisfy the shape.
- **Evidence-or-skip, per edge.** Cite the spec line that states the *relationship*, not merely
  that both nodes exist. A fabricated edge is durable misinformation, and these records land in
  authoritative recall immediately.

Keep the returned IRIs — they are your link registry and your report.

### Phase 5 — Record the approval act

One `ArchitecturalDecision`, titled "Spec &lt;name&gt; approved for implementation", whose
description states who approved it, the approval date, and the IRIs minted. This is both the
audit trail (invariant #6) and the Phase 0 idempotency key — without it, the next session cannot
tell an approved spec from an unapproved one.

Its description **must contain the approval marker on its own line**, exactly:

```
spec-approval: <spec path as given in Phase 0>
```

That literal is what Phase 0 matches on. A description that merely mentions the path does not
count as an approval, which is the point — write the marker verbatim or the next run will
re-approve the spec and duplicate every hub.

### Phase 6 — Validate + report

`validate_against_architecture` → expect **0 violations**. Fix any before finishing.

Report every record written as **kind / title / IRI**, plus which candidates were REUSEd and
which were dropped for lack of evidence. No silent writes.

---

## Re-approval (the spec changed after it was approved)

Diff the spec against what is recorded, then per record:

- **Changed** → `supersede_decision(superseded_iri, title, rationale)`, the rationale citing the
  revision that changed it. Never edit by minting a second copy.
- **New** → mint as in Phase 4.
- **Unchanged** → leave it alone.
- **Dropped from the spec** → `retract_decision(iri, rationale)` only if the spec genuinely
  abandons it; a section moving is not a retraction.

`hasLifecycleStatus` has no in-place transition (Requirement `1ae3b79a`), so this skill cannot
mark a Requirement "implemented" — do not try to fake it by superseding, which records
"superseded", not "done".

---

## Report

End with the record list (kind / title / IRI), the reuse and skip counts, and the validation
result. If the user declined at Phase 3, say **"not approved — nothing recorded"** and nothing
else. State what you did, not what would look thorough.
