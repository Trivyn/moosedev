# Harness evolution

> Superseded by S8 (2026-09-12): the `change-level-v2` policy and the
> reconciliation and post-edit candidate facilities it specifies were removed;
> the symbolic policy is the only harness (see `docs/harness.md`, "How the
> harness decides"). Kept as the record of the sealed campaigns.

Approved 2026-09-10. This spec is a view of existing graph decisions; the graph
wins on disagreement. Requirement
`https://moosedev.dev/kg/Requirement/4ff3ef62-c7a9-4494-89d3-a91ea110c52a`
supersedes the original experimental mapping Requirement. Implementation decision:
`https://moosedev.dev/kg/ArchitecturalDecision/395c4e78-0413-4cb6-90a8-901f44d062f2`.

## Approved replacement Requirement

Provide an experimental, default-off `change-level-v2` harness policy requiring
approved change-level purpose, scope, and verification before source mutations.
Persist purpose and obligation roles as task-journal metadata referencing current
typed knowledge. MOOSEDev resolves existing entities and represents proposed
additions by file/container and proposed-diff scope, without requiring predicted
entity names. After editing, use the current source index to discover entity
candidates; semantic association recommendations require human review before graph
links are written. Reuse existing validated predicates and their meanings;
introduce no ontology vocabulary. Related files, tests, and helpers may share a
journal rationale without exemption from applicable Constraints. Genuinely missing
governing knowledge requires grounded capture and acceptance before the dependent
edit. Source, knowledge, scope, or candidate changes invalidate affected approvals.
Preserve durable recovery and existing tasks’ policy contracts. Structural validity
and retrieval similarity do not establish semantic equivalence or relevance.

## Stage 1: capture reconciliation

The daemon nominates possible existing records using exact title and the existing
bounded record retrieval. It reports claims, lifecycle, relationships, operation
ownership and differences without asserting equivalence. Candidate snapshots bind
both accepted revision and per-record content/lifecycle state, including proposed
knowledge. Source proofs remain mandatory for any eventual code association.

Exact replay of the same persisted operation and unchanged request remains
idempotent. A different operation needs a reconciliation disposition even when its
title is identical. The model chooses reuse unchanged, revise proposal, or distinct
knowledge; the controller owns IDs, lifecycle mechanics and operation construction.
New observations and their differences remain journal evidence and are never
silently merged into the reused graph record. A revised pending proposal needs
explicit resolution of its existing review before replacement; supersession applies
only to eligible current accepted knowledge. External pending operations remain
outside this task's review authority.

Non-exact reuse recommendations join the existing human review batch. Governing
decisions require approval before dependent work, and completion requires all
dispositions. Durable page assessment and an outstanding review are separate state:
acknowledging the evidence page does not accept the record or erase the review.
Malformed/unresolved resolution attempts are bounded independently of service
errors. Old task contracts, exact request equality and journal recovery remain
compatible. Model relationship choices derive from the same existing SHACL
catalogue as daemon validation; no ontology change is needed for an illegal
Constraint-to-isMotivatedBy request.

## Stage 2: system-owned intent metadata

`change-level-v2` is a new persisted, default-off policy. Incremental model choices
identify semantic purpose; the controller retains valid choices while resolving
ambiguity. Plans still contain summary, files and checks. No predicted helper
identity or compound entity-to-record form is required from the model.

Approved files, proposed diff and current index determine existing entities and
planned source scopes. Newly implicated governing knowledge is delivered before
mutation, and changed source/knowledge/scope invalidates affected approval. Files,
tests and helpers can share journal justification without inheriting graph claims
or exempting constraints. After edits, refreshed definitions and diff yield bounded
entity candidates. Enclosing ranges, when present, narrow candidates; a broader
file candidate must be labeled conservative rather than claimed as a proven change.

Existing associations are reused. Candidate new associations need semantic
recommendation and human review. Constraints use constrains and other supported
records use concerns; neither association establishes implementation correctness.
Unsupported or stale indexing leaves an explicit unresolved obligation, never a
guessed CodeEntity. Only supported frozen producers refresh automatically.

## Evaluation and rollout

Stage 1 is six exploratory cells (two models, three scenarios). Stage 2 is twelve
matched cells (two models, current versus change-level-v2, three scenarios) from one
new frozen build. Both Stage 2 arms receive reconciliation and the same post-edit
candidate/association facilities; only the experimental arm requires pre-edit
approved purpose and scope and resolution of those obligations. The models are
Qwen3.8-27B and Gemma E4B. Reuse the previously approved cache, ledger and maintenance
packages and rubric; preserve their hashes. Freeze native repo builds, producers,
settings and runtime inputs. Retain all failures under new campaign identities.

Maintenance retains its conjunctive primary outcome: within-budget completion,
passing behavior/extraction, meaningful helper links to both seed records and no
redundant or unsupported accepted knowledge. Report constituents separately and
retain semantic reconciliation judgments alongside record/link judgments. Count
record, attached-link and reuse dispositions by stable event identity, separately
from UI actions and approval cycles. Capture physical token/request usage by
purpose, including probes and repair. Early failure is not token efficiency.

The scope-only simulated reviewer does not prove semantic protective value, and
n=1 per cell is diagnostic. No frozen run is patched or silently replaced. A new
implementation requires a new freeze and new retained results. Archive complete
inputs, outputs, snapshots, judgments and verified checksums outside target before
completion. Keep the new policy off by default; no commit, merge or publication is
part of this task.
