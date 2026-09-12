# Three-arm baseline: pre-registration

Mode `local-harness-evolution-stage2-baseline`. Governing records: the
three-arm decision recorded on 2026-09-12 (ArchitecturalDecision "Three-arm
baseline: native OpenCode capability alongside both harness policies"),
recovery decision `3c719e56`, terminal-cause Constraint `d2569270`, pilot
Constraint `83c18bb9`, token-semantics Lesson `e23cb3ec`. The graph wins on
disagreement. This document is hashed into the frozen design identity and is
not edited after the first scored cell starts.

## Question

Does the MOOSEDev harness cost task capability relative to the same model
running natively in OpenCode, at episode 1, per model and package? And does
the experimental purpose gate cost more than the current policy?

## Cells

Two models (Qwen3.8-27B, Gemma E4B) × three unchanged packages (ruleset cache,
retry ledger, display-label maintenance) × three arms:

| Arm | Backend | Condition | Policy | Knowledge surface |
|---|---|---|---|---|
| harness-current | harness | harness | current | seeded graph, dossiers, capture, reviewed links |
| harness-v2 | harness | harness | change-level-v2 | as above plus the pre-edit purpose gate |
| opencode-without | opencode | without | none | `PROJECT_NOTES.md` rendered from the same seed facts |

Eighteen cells, one run each, serial in the seeded schedule, `episode_limit` 1,
1200 s per episode, 32768-token client context, temperature zero, one frozen
build, one fresh store, one day. The task text, clarifications and visible
checks are byte-identical across arms; only the frozen per-condition guidance
sentence differs (`seed.GUIDANCE`, unchanged since the pilot). Every arm's
model traffic passes through the same recording proxy.

## Primary outcome

Per model and package, a run **passes** iff it completes within budget and the
hidden check passes. The primary comparison is harness-current versus
opencode-without on that pass/fail, six pairs. change-level-v2 is reported as
a third column of the same table. At n=1 per cell each pair is a single
observation; the table is diagnostic, and a per-model count of pairs where the
harness fails and OpenCode passes is the reported capability cost, with its
converse reported alongside.

## Secondary outcomes

- `task_requirements_complete` and `tests_complete` from independent review,
  all arms.
- `first_edit_seconds` (harness: first `task.edits` entry; OpenCode: first
  `edit`, `write`, `patch` or `multiedit` tool event) and elapsed seconds.
- Provider tokens and requests within model, from the proxy only. Native
  OpenCode `step_finish` totals are reported separately and never summed with
  provider totals.
- Harness arms only: terminal cause, purpose and association requests,
  reconciliation correctness, association relevance, the maintenance
  conjunction. Baseline runs have no knowledge or link outcomes; they are
  recorded as not applicable, never as zero or false.
- Knowledge coverage of `PROJECT_NOTES.md` for baseline runs is graded as
  claims (as in the pilot) and reported, but it is not part of the primary.

## Terminal causes

Harness cells use the recovery table plus the two guard classes above. Native
cells use a separate branch decided before the harness rows: `infrastructure`, `success`
(completion stop reason and success status), `native_no_completion` (process
exited without a stop reason and not at the deadline), `deadline_native`
(deadline), else `unknown`. Neither native class is controller class. An
`unknown` terminal invalidates that run for the primary and requires an
instrumentation fix under a new identity.

## Known asymmetries, stated before the run

- OpenCode recovers internally from tool errors; the frozen study reviewer
  terminates a harness task at its first `last_error`. A harness stop at a
  recoverable error is therefore partly a reviewer artefact. It is reported as
  such and not corrected mid-campaign.
- OpenCode reads notes when it chooses to; the harness pushes dossiers and
  requires plan approval, captures and reviews. Time and tokens include that
  work for the harness arms.
- The purpose gate has no completion path for obligation-only selections
  (Lesson "Obligation-only purpose selections need a completion path"), so
  change-level-v2 stops on the ledger and on obligation-only selections are
  expected and are not new evidence about the gate.
- Attempt v1 ran runner binaries whose Rust sources were identical to build
  `86dcd226…`. Attempt v2 carries the runner fix described below, so its
  binaries differ; the source diff against `86dcd226…` is limited to
  `capture_resolution.rs` and its tests and is recorded in the freeze.

## Attempt v1 (study `harness-evolution-stage2-baseline-v1`, build `5cdf8bd6…`)

Stopped after seven sealed cells when cell 7 (Gemma, harness-current, cache)
entered a reviewer-runner loop: the runner re-presented an oversized
human-required reuse card under a new operation id after every rejection, the
frozen reviewer rejected each new card, and the 43.5 GB event stream then
killed the driver at sealing. Retained unsealed and documented in
`target/harness-evolution-stage2-baseline-v1/`. The seven sealed v1 cells are
never pooled with v2.

Changes for v2, all listed as shared (both harness arms): rejecting a
human-required reuse card keeps the observation unresolved instead of
re-reconciling, and the oversized branch never re-presents a candidate the
human already rejected; evidence sealing streams the event sequence; the
driver ends an episode as `evidence_limit` when recorded evidence exceeds the
frozen byte cap and as `reviewer_reject_loop` when the scripted reviewer
rejects the same reuse candidate five consecutive times. `reviewer_reject_loop`
is controller class; `evidence_limit` is not attributed to an arm. The
reviewer's decision function is unchanged.

## Rules

No mid-campaign fixes. Retain every failure. A new implementation needs a new
build identity, study id and store. Grading pools nothing across `build_id`s.
Archive inputs, outputs, judgments and verified checksums outside `target`
before completion. `change-level-v2` stays default-off regardless of result.
