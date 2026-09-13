# Symbolic two-arm baseline: pre-registration

Mode `local-harness-symbolic-baseline`, study `harness-symbolic-baseline-v2` (attempt v1
is described below).
Governing records: ArchitecturalDecision `9dcaddeb` (S8: the symbolic policy is
the only harness) and its consequence `b5a313eb` (the next matched campaign is
one harness arm beside opencode-without under a new identity), Requirement
`9ae68a19` (the coding model answers only actions and one capture note),
Constraints `40d414ce` (frozen repository-built binaries), `83c18bb9` (isolated
inputs, E4B helper, scripted reviewer, 20-minute episodes), `d2569270` (typed
exhaustive terminal causes) and `9936d96d` (frozen reconciliation thresholds),
Lessons `c60cc811` (harness failures are unattended halts), `7f341f01`
(evidence cap and reject-loop guard), `e23cb3ec` (native and provider tokens
are never summed), `10d76078` (report association candidates by kind) and
`861c54c7` (probe the real contracts before freezing). The graph wins on
disagreement. This document is hashed into the frozen design identity and is
not edited after the first scored cell starts.

## Question

After S8, the coding model answers two questions: `harness_action` while
working and one plain-prose `harness_capture_note` at the final checkpoint;
obligations, scope, associations, capture typing and reconciliation are derived
by the daemon. Does that harness cost task capability relative to the same
model running natively in OpenCode, at episode 1, per model and package? And
how often does it end in an unattended halt, the failure class every earlier
campaign attributed to a model-facing structured decision?

## Rigour

This is an exploratory campaign. One frozen build, a fresh store, twelve serial
cells, no mid-campaign fixes and an automated report are kept because without
them the cells do not measure one thing. Hand-written semantic grading is
performed only if a result is interesting. The result feeds a Lesson, not a
confirmatory claim.

## Cells

Two models (Qwen3.8-27B, Gemma E4B) × three unchanged packages (ruleset cache,
retry ledger, display-label maintenance) × two arms:

| Arm | Backend | Condition | Policy | Knowledge surface |
|---|---|---|---|---|
| harness-symbolic | harness | harness | symbolic | seeded graph, dossiers, derived associations, typed final note |
| opencode-without | opencode | without | none | `PROJECT_NOTES.md` rendered from the same seed facts |

Twelve cells, one run each, serial in the seeded schedule, `episode_limit` 1,
1200 s per episode, 32768-token client context, temperature zero, one frozen
build, one fresh store. The task text, clarifications and visible checks are
byte-identical across arms; only the frozen per-condition guidance sentence
differs (`seed.GUIDANCE`, unchanged since the pilot). Every arm's model traffic
passes through the same recording proxy.

## Primary outcome, stated as bounds

1. Per model and package, a run **passes** iff it completes within budget and
   the hidden check passes. Six harness-symbolic versus opencode-without pairs.
   Reported per model: the number of pairs where the harness fails and OpenCode
   passes (the capability cost) and its converse.
2. The **halt count**: the number of symbolic runs whose terminal cause is in
   the unattended halt class {`model_repair_exhausted`,
   `scope_escape_exhausted`, `capture_retype_exhausted`,
   `reviewer_idle_deadline`}, per model, split by whether the native pair
   passed. Zero is the S8 claim under test.

At n=1 per cell each pair and each halt is a single observation. Direction is
reported; counts bound nothing. Outcomes flip run to run at temperature zero
when a model's plan choice flips (Lesson `c60cc811`).

## Halt classes and the reviewer

The symbolic runner parks itself in `AwaitingInput` after its autonomous
bounds: three plan-scope escapes (`scope_escape_exhausted`) or three capture
retypes (`capture_retype_exhausted`). No `recovery` or `last_error` is set. The
frozen reviewer previously answered every `AwaitingInput` with the clarification
text, which would replan the task and re-park it until the clarification cap:
the reviewer supplying the missing human. In this campaign a park is a reviewer
terminal with the park's own cause, exactly as `awaiting_guidance` already is.
This is the only reviewer change; every decision the reviewer made before this
campaign is unchanged, and no pre-existing state is decided differently.

`clarification_cap` is a reviewer budget, reported separately and not a halt.
`unknown` invalidates the run for the primary and requires an instrumentation
fix under a new identity (Constraint `d2569270`). Native cells keep the
three-arm baseline's native branch: `success`, `native_no_completion`,
`deadline_native`; neither native class is controller class.

## Secondary outcomes

- `symbolic_*` metrics from the final journal snapshot: obligations derived,
  scope-escape replans and exhaustion, no-op continuations, associations
  derived/none/skipped/unresolved, capture deferred/note/typed, reconciled
  restates/refines/distinct, capture notes, and `structured_model_decisions`,
  which must be zero in every symbolic run (the negative proof), plus
  `plan_check_rejected` and `check_unrunnable` from the check guard.
- `evolution_*` review counts (record and link dispositions, review
  interactions, approval cycles): one batch review is one human decision.
- `first_edit_seconds` (harness: first `task.edits` entry; OpenCode: first
  `edit`, `write`, `patch` or `multiedit` tool event) and elapsed seconds.
- Provider tokens and requests within model, from the proxy only, attributed by
  purpose (`harness_action`, `harness_capture_note`, helper, probe). Native
  OpenCode `step_finish` totals are reported separately and never summed with
  provider totals.
- Harness arm only: association candidates by kind, skipped scopes by reason,
  typing mode and dispositions. Native runs have no knowledge or link outcomes;
  they are recorded as not applicable, never as zero or false.
- The maintenance conjunction and `tests_complete` stay unknown unless graded.

## Known asymmetries, stated before the run

- The S8 bounds are expected runner behaviour, not defects: up to three
  autonomous replans on a scope escape, the first no-op edit running the checks,
  up to three retypes of a rejected typed capture.
- A read-only conversation never reaches the final checkpoint and captures
  nothing; `capture_typed = 0` on a completed task is legal.
- OpenCode recovers internally from tool errors; the frozen reviewer terminates
  a harness task at its first `last_error`. A harness stop at a recoverable
  error is partly a reviewer artefact and is reported as such.
- Harness time and tokens include dossier push, plan approval, derived link
  review and note typing. OpenCode reads notes when it chooses to.
- Capture typing uses the daemon's LLM sensor (Gemma E4B helper) in both
  models' harness cells; helper requests are logged and attributed separately.
  Gemma cells serve agent and helper from one loaded instance.

## Pinned settings

Reconciliation thresholds are the frozen defaults: `MOOSEDEV_RECONCILE_RESTATES`
0.80, `MOOSEDEV_RECONCILE_REFINES` 0.55, `MOOSEDEV_RECONCILE_REFINES_CONTAINMENT`
0.60, `MOOSEDEV_RECONCILE_TIEBREAK_BAND` 0.08; every receipt records them.
`MOOSEDEV_LLM_ASSIST_LEVEL` is unset, which the daemon resolves to Sensor once
the helper endpoint is configured. Evidence cap 8 GiB (`evidence_limit`),
reviewer reject-loop cap 5 (`reviewer_reject_loop`), both retained from the
three-arm baseline although the reuse card they guarded no longer exists.

## Named runner changes since build `abe95d92…`

All in the single harness arm; none is credited as a fix against the three-arm
baseline, whose arms can no longer be run from this tree and whose results are
never pooled with this identity:

- the symbolic policy is the only harness; task journals are schema 2;
- a plan-scope escape replans autonomously, three per task, then parks;
- the first no-op edit runs the required checks instead of spending the repair
  budget;
- an abandoned link review resets the derived association for re-derivation;
- a daemon-rejected or colliding typed capture is retyped under fresh ids,
  three per note, then parks;
- typing is invalidated on a source or knowledge change without a model call;
- plan checks must be runnable commands: plan validation rejects a check whose
  first word is not a shell builtin, an installed program or a project file
  (one repair attempt, `plan_check_rejected`), and a required check that exits
  126 or 127 is reported as an invalid check rather than a failed test
  (`check_unrunnable`).

## Smoke run

At most one harness cell may be run into a discarded store before cell 0 to
exercise `harness_action`, `harness_capture_note`, `POST
/api/v1/harness/capture/type` and the contract-2 advertisement on the frozen
build. It is never scored and never pooled. A defect found there means a new
identity, not a patched one.

## Block order

The twelve cells run in two blocks, each in frozen schedule order: first the
six Gemma E4B cells, then the six Qwen3.8-27B cells. Gemma is first because the
model-size question lives there. Between the blocks the operator reads every
harness journal of the first block. A harness run whose terminal path is not
explained (what the model did and why the run ended; Lesson `1f6233e3`) stops
the campaign before the second block, and the attempt is reported as stopped.
A failure explained by the model's own output or code does not stop it. Blocks
change the order in which cells run, not the cells, and the summary is
computed over all twelve.

## Attempt v1 (study `harness-symbolic-baseline-v1`, build `9aa4f75c…`)

The pre-registered smoke cell (Gemma, harness, retry ledger) and the first
Gemma harness campaign cell both ended at the deadline after dozens of approved
plans. The journals showed the model writing its plan checks as English
sentences; the shell failed each with exit 127 and the failure message sent the
model back to replan the same checks. The maintainer stopped the campaign after
cell 1; cells 2 and 4 were interrupted and cell 3 sealed before the driver was
killed. All five runs and the smoke run are retained unscored and never pooled.
One instrumentation defect was fixed before v1's cell 0 (the deadline snapshot
was overwritten by the post-interrupt Cancelled state). Changes for v2: the
check guard listed above and the block order.

## Rules

No mid-campaign fixes. Retain every failure. A provider stall may be replaced
under the same identity only as an `infrastructure_failure` attempt recorded
with `--replacement-for`. A new implementation needs a new build identity,
study id and store. Grading pools nothing across `build_id`s. Archive inputs,
outputs, judgments and verified checksums outside `target` before closure.
