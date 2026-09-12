# Harness evolution campaigns

The accepted decision and requirements cited in
[`spec/harness_evolution.md`](../../spec/harness_evolution.md) govern this protocol.
The graph wins on disagreement. These campaigns have separate identities from
the original intent pilot described in [INTENT.md](INTENT.md).

Stage 1 uses `local-harness-evolution-stage1`: six exploratory cells with the
current policy. Stage 2 uses `local-harness-evolution-stage2`: twelve matched cells,
Qwen3.8-27B and Gemma E4B, current and change-level-v2 policies, and the three
unchanged scenarios. Both Stage 2 arms receive deterministic post-edit discovery
and reviewed associations. Only the treatment has the pre-edit purpose/scope gate.

Freeze binaries with `build --indexer-manifest PATH`. Preflight binds the actual
repository-built executables, owned source snapshots, indexer, model identities,
generation settings, approved rubric and scenario hashes. It also executes the
frozen session binary's neutral `--probe-intent-contracts --model ID --endpoint URL`
mode through the recording model proxy. This runs the actual production schemas
for association, no association, purpose selection, purpose completion and missing
purpose with an empty candidate inventory. No coding action, scenario, graph or
project is exposed to those probes. A generic successful JSON response alone is
insufficient: every response must satisfy the supplied production contract.
Preparation, transport and format failures block scored runs. Choosing another
allowed branch is retained as a semantic diagnostic, not a compatibility failure;
the pilot measures model capability rather than selecting models that already
pass a semantic test. Prompts use complete neutral fixtures and receive one
request each, without retries or a narrower substitute schema.

Native probe inputs, schemas, output, provider response bytes, executable hashes
and errors are retained under `target/harness-study/native-contract-probes/`.
Preflight failures include the evidence location. Preserve and archive this setup
evidence with the campaign; probe usage is setup cost, separate from episode cost.

Run serially in the frozen schedule, with one attempt per cell and dependent
episodes unattempted after failure. Do not patch a scored campaign. A persistent
contract incompatibility may stop future launches: retain the sealed attempts,
explicitly identify cells never started, archive the aborted identity, and freeze
a new identity after correction. Never fabricate unstarted runs or pool recovery
results with the failed build. The original Stage 2 build was stopped this way
after MLX rejected `uniqueItems` on a nonempty association request.

The recovery stage uses `local-harness-evolution-stage2-recovery`: the same
twelve matched cells on a build that also fixes the defects found after the
sealed campaign, with `episode_limit` 1 so every run attempts only its first
episode. Its primary outcome is decided from typed journal fields
(`first_edit_reached` and `terminal_cause`, see `cause.py`) rather than from
error text, and every fix shared by both arms is named in
[RECOVERY.md](RECOVERY.md) so no shared correction is credited to the policy.
`init-evolution --stage local-harness-evolution-stage2-recovery` accepts a
Stage 2 or pilot parent preflight and refuses the two sealed Stage 2 builds.

The baseline stage uses `local-harness-evolution-stage2-baseline`: the same two
models and three packages with three arms each, harness `current`, harness
`change-level-v2`, and OpenCode with no MOOSEDev (condition `without`,
`PROJECT_NOTES.md` from the seed facts), eighteen cells, `episode_limit` 1.
The pre-registered question is whether the harness costs task capability
relative to the native agent at episode 1; see [BASELINE.md](BASELINE.md).
Baseline runs have no knowledge or link outcomes. Native cells classify with
their own terminal causes (`success`, `native_no_completion`,
`deadline_native`) and mark first edit from OpenCode edit tool events.

Grade sealed evidence independently. Retain coding correctness, within-budget
workflow completion, knowledge coverage, semantic reconciliation, association
quality and the registered maintenance conjunction separately. Report physical
requests and observed token coverage by purpose; missing usage remains unknown.
Audit both directions between reviewed daemon operations and journal dispositions,
excluding exact pre-episode setup dispositions using retained baseline evidence.
Keep affected-scope requirements, approvals, input interactions and individual
record/link/reuse dispositions separate; success events alone are only a lower
bound on inputs. Scope-only simulated approval and n=1 per cell do not establish
semantic protection or statistical benefit.

Archive all inputs, outputs, raw failures, judgments and explicit corrections
privately outside `target`, with per-file hashes and a verified archive hash.
Reconstructed convenience output must be labeled as derived; it never substitutes
for missing raw output without an explicit provenance gap.
