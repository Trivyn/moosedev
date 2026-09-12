# Stage 2 recovery: pre-registration

Mode `local-harness-evolution-stage2-recovery`. Governing records: decision
`https://moosedev.dev/kg/ArchitecturalDecision/3c719e56-7707-457d-8c6e-a9616ad6b436`
and Constraint
`https://moosedev.dev/kg/Constraint/d2569270-2688-4afd-aeda-deae69b6b8c3`,
under Requirements `59378083` and `4ff3ef62`. The graph wins on disagreement.
This document is hashed into the frozen design identity; it is written before
the build is frozen and is not edited after the first scored cell starts.

## Why a recovery stage

The sealed Stage 2 campaign (build `2f108e95…`) measured a `change-level-v2`
workflow with three controller defects. Those were repaired after sealing
(build `364140e4…`), but review found three further defects able to wedge the
treatment arm on intended paths, two shared reconciliation defects, two driver
confounds, and no typed way to tell a controller wedge from a model failure.
This stage measures the repaired workflow with those corrected and with an
outcome the journal can decide by itself.

## Cells

Two models (Qwen3.8-27B, Gemma E4B) × two policies (`current`,
`change-level-v2`) × three unchanged scenario packages (ruleset cache, retry
ledger, display-label maintenance), one run per cell, serial, in the seeded
schedule order recorded at preflight. `episode_limit` is 1: only the first
episode of each package is attempted; the remaining episodes are recorded
`unattempted` with reason `episode_limit`. Same 32768-token context, temperature
zero, 1200 s per episode, frozen indexers, frozen model identities, one fresh
study store. Both arms run the same frozen build.

## Primary outcome (treatment arm only)

A treatment run passes iff both hold:

1. `first_edit_reached` is true: the final journal snapshot has a non-empty
   `task.edits` (the runner also emits intent event `edit_applied`).
2. `terminal_cause` is not in the controller class
   `{controller_invariant, daemon_rejection, reviewer_idle_deadline,
   purpose_missing_exhausted}`.

Control runs have no purpose gate, so outcome 1 is degenerate for them. They do
not contribute to the primary; they baseline the secondary outcomes and the
shared fixes.

`terminal_cause` is derived by `cause.classify` from typed fields only
(`task.last_error_kind`, `task.recovery.status`, intent event kinds, the
reviewer's typed `cause`, the driver's `suppressed_gate_repeats` and
`timed_out`). Never from `last_error` text. `unknown` is reachable only when a
needed field is absent. An `unknown` terminal invalidates that run for the
primary and requires an instrumentation fix and a new frozen identity; it is
never rerun under this identity and never counts for or against an arm.

`purpose_missing_exhausted` means the model was offered candidates and still
reported missing three consecutive times; it is controller class. An empty
accepted inventory classifies separately as `purpose_inventory_empty`, a
scenario/knowledge outcome that is reported but not charged to the controller.

## Secondary outcomes, per run, both arms

| # | Outcome | Field |
|---|---|---|
| 2 | Completion within budget | episode `status == success` |
| 3 | Source and hidden-test correctness | `checks[].passed` and independent grading |
| 4 | Purpose/reuse reconciliation correct | independent `capture_reconciliation_correct` |
| 5 | Required links with zero unsupported accepted links | independent `associations_relevant`, `intent_assessment` |
| 6 | Requests and tokens by purpose | `request_usage.by_purpose`, `harness_recovery.model_requests_by_purpose` |
| 7 | Candidates and association calls by entity kind | post-edit candidate pages in the journal; `harness_association_selection` requests |
| 8 | Exact repeated plans and file reads | `intent_activity.source_rereads_unchanged`, byte-identical adjacent plan count |
| 9 | Review gates, cycles and seconds before the first edit | `intent_gates`, `evolution_reviews`, `first_edit_seconds` |

`first_edit_seconds` is driver monotonic time from episode start to the first
snapshot with a non-empty `task.edits`, the same clock as the deadline.

## Shared fixes (both arms; never credited to the policy)

- Association prompt sends the record-choice payload once per request.
- Review blocking limited to governing, reconciliation and missing-purpose work.
- A rejected reuse refetches candidates at the current revision.
- A persisted reconciliation disposition is replayed after an interrupted daemon call.
- The driver computes journal metrics once from the final snapshot.

Control-arm deltas versus the sealed campaign are the shared-fix baseline.
Treatment-only fixes: state-masked purpose schemas, decision-first ordering,
missing-purpose scheduling and recovery, parked-edit invalidation after a
governing detour, purpose-selection revision refresh after a post-edit link
review.

## Acceptance for continuing change-level-v2 work

- No controller-class terminal cause in any treatment run.
- Both models traverse `select`, `done` and, where applicable, `missing`.
- At least one treatment maintenance run reaches code execution and grading.
- Disposition accounting matches daemon operations with no gaps.
- Association overhead is reported, not hidden by early failure.

Six treatment runs at n=1 bound a per-run controller-failure rate; they do not
clear it. If the true rate were 20 percent, all six pass with probability about
0.26. A pass permits the next experiment (deterministic candidate filtering or
batching); it does not enable `change-level-v2` by default. If both models still
stop before editing with the gate as the cause, narrowing or removing the purpose
gate is the next direction.

## Known residual confounds

- The runner emits full task snapshots; raw snapshot bytes still grow with
  journal length. Metric CPU no longer does.
- Gemma serves agent and helper traffic from one instance; Qwen does not.
- Model instances stay loaded across cells; order is seeded-random, recorded.
- The scope-only simulated reviewer measures friction and resulting link
  quality, not semantic protective value.
- n=1 per cell is diagnostic, not statistical.

## Rules

No mid-campaign fixes. Retain every failure. A new implementation needs a new
build identity, new study id and new store. Grading pools nothing across
`build_id`s. Archive inputs, outputs, snapshots, judgments and verified
checksums outside `target` before completion.
