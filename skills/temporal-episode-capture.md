# Skill: Temporal episode capture (git-walk bootstrap)

You are invoked **once per commit** by the temporal-bootstrap driver, which replays a repo's
trunk history **oldest → newest**. Your job: capture the durable engineering decisions in **this
one episode** (a single commit or a merge to trunk) into the project knowledge graph — **exactly
as you would during normal work** (recall → align → record → link → supersede → validate) — with
two temporal twists:

1. Records are **automatically stamped with this commit's date + author** (the driver injects
   them server-side) — you record normally and never pass timestamps/authors yourself.
2. You see the graph **only as it stood BEFORE this commit** (the driver runs episodes in order,
   one at a time). This is **look-back, never forward** — it is what lets you create honest
   `supersedes` edges when this commit reverses an earlier decision.

This is the per-episode adaptation of `bootstrap-existing-codebase.md` (decision-cluster capture).
That skill is the authoritative discipline; everything below scopes it to one commit.

> **Inputs the driver gives you:** the commit SHA, **author**, **date** (RFC3339), the commit
> message, and the diff. A `moosedev` MCP server is attached to the shared, in-progress store.

---

## Workflow (one episode)

### 0. Recall first — see the graph-so-far
`get_relevant_context(topic: "<the concepts this commit touches>")` (and a second focused call if
the commit spans areas). The store contains **only decisions from earlier commits** — this is your
link-or-mint candidate set **and** your supersede candidate set. Note the IRIs of any record this
commit might extend, link to, or reverse.

### 1. Is this commit decision-bearing?
Capture only **durable why**: a choice + its rationale, a constraint/invariant introduced, a
requirement a change satisfies, an alternative rejected, a consequence/trade-off accepted, a
lesson/gotcha, an anti-pattern, or a **reversal/refinement of an earlier decision**. Routine
mechanics (formatting, dependency bumps, typos, pure refactors with no rationale change) →
**record nothing** and report "non-bearing." Honesty over completeness: a sparse/unclear commit
yields few or zero records — do not pad (invariant #6).

### 2. Align new terms (invariant #4)
For any concept whose `kind` isn't an obvious ontology class, `align_concepts(label, definition?)`
before recording, so the graph doesn't drift. (Class resolution — distinct from link-or-mint.)

### 3. Record nodes
`record_important_decision(kind, title, description, status)`:
- `title` = a short **handle** (≤ ~80 chars / ~12 words): the concept's NAME — what you'd *say* to
  refer to it ("Adopt RocksDB for the durable store"), **not** the claim. No sentence-long titles,
  no dashes/parentheticals/metrics. `rdfs:label` is weighted **2× in retrieval**, so a sentence-long
  title packs the entire claim into the top-weighted field and makes lexical retrieval trivially
  saturate (self-announcing records BM25 always finds) — keep it a name.
- `description` = **lead with the one-line claim** (the assertion the record makes), THEN the
  **why + evidence**: cite this commit and the diff hunk / message line. The claim lives here, not
  in the title.
- `status`: `"accepted"` if the commit puts the decision in effect; else `"proposed"`.
- The record's **timestamp + author are set automatically to this commit's values** by the driver
  — record normally; do **not** pass timestamp/author yourself.
Capture the IRI returned for each node (your link registry).

### 4. Link — `relate(subject_iri, predicate, object_iri)`
Per the predicate map (`isMotivatedBy`, `weighs`, `resultsIn`, `concerns`, `constrains`,
`violates`, `learnedFrom`). **Link-or-mint:** reuse an existing record's IRI from step 0 rather
than minting a duplicate (bias Requirement/Constraint toward reuse — they're hubs). Edges are
SHACL-validated; a rejected edge means the kinds/direction are wrong — fix, don't force.

### 5. Supersede — only what already exists
If this commit **reverses or replaces** a decision you found in step 0:
`supersede_decision(superseded_iri=<the existing IRI from step 0>, title, rationale=<why it
changed, from THIS commit>, description)` — timestamp + author are injected automatically.
- You may supersede **only an IRI you actually saw in recall** — never invent one.
- **Prefer mint over speculative supersede.** A refactor or extension is not a reversal. Only
  supersede when the commit clearly retires the earlier choice, and cite the evidence.

### 6. Validate
`validate_against_architecture` → expect **0 violations**. Fix any before finishing.

### 7. Anti-confabulation (invariant #6)
Evidence-or-skip **per edge** (cite the diff hunk / message line that states the relationship);
prefer not-linking / not-superseding over guessing; assert only what the diff or message states or
directly implies. A fabricated edge or supersession is durable misinformation.

---

## Report
End with a short summary: nodes minted (kind + title + IRI), edges drawn, supersessions
(old → new), or **"non-bearing — nothing recorded"** with a one-line reason. State plainly what
you recorded, not what would look thorough.
