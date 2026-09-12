# Harness bounded self-recovery: change set for review

Draft for James, 2026-09-12 morning. Derived from the terminal causes actually
observed in the three-arm baseline (`target/harness-evolution-stage2-baseline-v2/`)
and the two earlier campaigns. Nothing here is implemented or captured in the
graph yet; it is a proposal. Governing context: v3 Requirement `350c7f2e`
(standalone agent), recovery decision `3c719e56`, three-arm decision
`f19ce43b`, Lessons `f34b953c` (obligation-only coercion) and `7f341f01`
(rejected human-required card).

## The claim the data supports

Every harness failure on a package the model can do natively is a workflow
state the runner cannot leave without a human, not a wrong edit. Gemma E4B:
two of three packages lost to the native agent this way; Qwen 27B: no
capability cost under `current`, all three packages lost under the purpose
gate. The scripted reviewer makes the halts visible; in real standalone use
nobody is there either. So the fix is bounded autonomous recovery inside the
runner, one transition per observed halt, each with a cap and a journal event.
Simulating the human in the study reviewer would hide the gap and is not
proposed.

## Transitions, one per observed halt

| # | Observed halt (cells) | Today | Proposed transition | Bound | Event |
|---|---|---|---|---|---|
| 1 | Purpose gate with obligation-only selections (v2: 0, 8, 9, 11, 12; recovery 1, 3, 4) | only `missing` legal; three rounds then guidance | offer `done` whenever the selection is non-empty; record the plan summary as the purpose when no purpose-role record was chosen; on an empty inventory, short-circuit to `done` with the plan summary instead of a forced `missing` call | one purpose cycle | `purpose_obligations_only` |
| 2 | Plan-scope escape, "return to Plan" (v2: 1, 7; recovery 6) | error, `last_error`, human must `/plan` | discard the pending edit, re-enter Plan mode with the escaped file appended to the reason, let the model replan | 2 escapes per task, then park | `scope_escape_replan` |
| 3 | Repair budget exhausted on a no-change edit (v2: 6) | park for guidance; the message itself says "or finish if the objective is already satisfied" | after the third no-change edit, inject that sentence as the next observation and continue; if the next action is still a no-op, run checks and finish if they pass | one continuation | `noop_edit_continuation` |
| 4 | Idle after a suppressed identical gate decision (v2: 4; recovery 9 read loop) | the run waits until the deadline | controller timeout on an unchanged `AwaitingReview`/`AwaitingPlan` state: after N seconds with no new input and no new snapshot difference, re-enter Plan mode with the reason | once per task | `idle_replan` |
| 5 | Rejected human-required reuse card (v1 cell 7) | fixed: observation kept unresolved | keep; also bound every path that mints a new operation id | done | `reuse_unresolved` |
| 6 | Model repair exhausted generally (recovery: purpose selection) | park | one "proceed with best judgment" continuation before parking, since that is the frozen clarification text anyway | one per task | `guidance_autocontinue` |

## What does not change

Enforcement and evidence stay: source CAS, sandbox, plan approval before edits,
capture checkpoints, reviewed links, durable recovery across restart. The
proposal changes what the runner does at its own dead ends, not what it
demands as evidence. Each transition writes an intent event so the study can
count autonomous recoveries per run and grade whether they were sound.

## Also on the table, separately

- Association candidate filtering by kind and test path at enumeration
  (Lesson `10d76078`): reduces the 43-of-57 request share on Qwen/cache and
  the parameter links that fail every maintenance conjunction.
- A per-edit budget for capture, reconciliation and association work under
  `current`, since Gemma/current's failures are request-volume failures.
- A diagnostic study arm with a minimally nudging reviewer, run once, to bound
  how much of the Gemma gap the six transitions can close before they are
  built. Not a scoring arm.

## Order

1 (purpose gate) and 2 (scope escape) account for eight of the nine harness
failures on natively passable packages across the three campaigns and are the
smallest changes. 3, 4 and 6 are each a single guarded transition. Then a new
identity, the same eighteen cells, and the same pre-registered capability
table.
