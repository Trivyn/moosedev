# Floor study: pre-registration

**Status: draft, not sealed, not approved.** Written 2026-09-15 for James
Adam's review. No mode, design identity or approval exists for this study yet.
Once sealed, this document is hashed into the design identity and is not edited
after the first scored cell starts. The graph wins on disagreement.

Governing records: Requirement `e9166711` (harness levers are classified as
grounding or control and tuned per model capability), Requirement `d2a54c9c`
(long-horizon, multi-turn work is where MOOSEDev's value is judged), Requirement
`9ae68a19` (the coding model answers only actions and one capture note), AD
`84dfd153` (native tool calls are the harness action contract), Constraints
`40d414ce` (frozen repository-built binaries), `83c18bb9` (isolated inputs, E4B
helper, scripted reviewer, 20-minute episodes) and `d2569270` (typed exhaustive
terminal causes). This study's goal is Requirement `f4b3e3d6` (a definitive
generic-harness comparison and model-size floor); AD `3f450d51` makes OpenCode the
generic-harness comparator at every tier, frontier included.

## Question

James's wording: "MOOSEDev harness improves small model results (vs a generic
harness like OpenCode) in these ways, it doesn't in these ways. The floor for a
useful model is X size." And, with a frontier anchor, "where the benefits stop
(and when/if the harness actually begins to do harm against a frontier model)."

Scope: the generic harness is OpenCode for every tier, the frontier anchor
included. James's working assumption is that Sonnet-class and larger models are
best served by MOOSEDev's MCP tools inside their native harness (Claude Code or
Codex). This study does not test that; it tests the MOOSEDev harness against
OpenCode on the same model.

## Rigour

Confirmatory, unlike every earlier campaign. Field checks (`FIELD_CHECK.md`)
are exploratory, one run per cell, and the grading code refuses to score or
pool them (`grading.py` refuses field-check reviews and mixed reports), so none
of them is a result here. This study needs its own mode and design identity, a
single frozen build for every cell, repetitions, pre-registered thresholds and
no harness change after sealing.

## Hypotheses

| ID | Hypothesis | Unit | Decided by |
|---|---|---|---|
| H1 | The harness arm's pass rate is at least the native arm's, per tier | run | hidden checks |
| H2 | The harness applies delivered project rules (deciding facts delivered before the first edit) more often than native | probe | retention and task probes that name a deciding fact |
| H3 | The harness spends fewer tokens and less wall time per passed task | passed run | provider tokens, elapsed seconds |
| H4 | Over episodes 2+, the harness keeps retention and currency probes passing more often than native | probe | `retention` (correctness) and `currency` probes |
| H5 | Floor: the smallest tier whose harness pass rate is at least **T** and not below native by more than margin **M** | tier | H1 table |
| H6 | Frontier direction: at the frontier tier OpenCode native does at least as well as the harness, and better on efficiency (tokens and wall time per passed task) | tier | frontier headline comparisons, below |

### H6, the frontier hypothesis

James (2026-09-15): "Open to being surprised if Sonnet works better in
moosedev-harness, but the hypothesis is it does better without it."

- **Pre-registered direction.** At the frontier tier (Sonnet via OpenRouter),
  OpenCode native is expected to do at least as well as the MOOSEDev harness,
  and better on efficiency: tokens and wall time per passed task.
- **Headline frontier comparisons**, all harness versus native at TF:
  1. run pass rate;
  2. knowledge-use probes (deciding facts applied: correctness retention and
     task probes that name a deciding fact);
  3. tokens per passed task;
  4. wall time per passed task.
- **Both directions are reported.** The analysis is two-sided with exact
  intervals. A harness advantage at the frontier is a reportable surprise
  result, not something to explain away.
- **If the harness is worse on any headline comparison**, a pre-registered
  ablation follows on the frontier tier only. It removes one control lever per
  arm and keeps the grounding levers: one action per step, plan approval,
  mode-specific action sets (AD `909145d3`), plan-coverage returns. The
  ablation needs its own sealed identity before it runs.
- **If the harness is better on any headline comparison**, the report names
  which grounding levers fired in those runs (from `process.SYMBOLIC_EVENT_KINDS`
  and `crowding-report` delivery sections), so the surprise is attributable.

## Tiers

| Tier | Model (table key) | Size | Thinking | Runtime |
|---|---|---|---|---|
| T1 | `qwen/qwen3.5-9b` | dense 9B | off | LM Studio MLX, 4-bit |
| T2 | `google/gemma-4-26b-a4b` | MoE 26B, about 4B active | off | LM Studio MLX, 5-bit |
| T3 | `qwen/qwen3.8-27b` | dense 27B | off | LM Studio MLX, 5-bit |
| T4 | `gemma-4-31b-it` | dense 31B | off | LM Studio MLX, 5-bit |
| T5 | NVIDIA Nemotron-3-Super-120B-A12B (**qualification pending**) | MoE 121B, 12B active | hybrid; both settings (`<think>` in its template) | LM Studio MLX, 4-bit, 68 GB, runs alone |
| TF | Sonnet via OpenRouter (**decision**: exact model ID) | frontier | provider default, matched across arms | hosted |

Dropped by the maintainer (2026-09-15): `gemma-4-e4b-it-mlx` as a tested tier (it stays the
daemon helper in every harness cell, Constraint `83c18bb9`), and both 70B models.
`llama-3.3-70b-instruct` and `nousresearch/hermes-4-70b` are excluded on evidence, not
preference: Llama paid about 55 s of JSON-schema setup per harness request and LM Studio did
not parse its tool calls (Lessons `9ef26b02`, `30a39e45`); Hermes made no tool-driven progress
in four cells, thinking off or on (Lesson `ddbe810c`). Their `model_table.py` rows stay so that
approved designs and existing evidence keep verifying.

Gap: nothing separates total size from active size between T2 (26B/4B) and T5 (121B/12B).
Qwen3.6-35B-A3B (35B total, 3B active, 21.6 GB MLX) would fill it in the Qwen tool format LM
Studio already parses; not downloaded (**decision**).

Pinned local rows, fingerprints and runtime contexts are in `model_table.py`.
Gemma E4B remains the daemon helper in every harness cell (Constraint `83c18bb9`).

A tier enters the study only after its preflight tool-call and response probes pass on the
sealed build AND it completes one qualifying harness episode with a plan and an edit; a clean
short probe is not sufficient (Lesson `ddbe810c`).

## Arms

| Arm | Backend | Knowledge surface | Contract |
|---|---|---|---|
| harness | `harness` | seeded graph, Project rules, dossiers, grounding, typed capture | native tool calls (AD `84dfd153`) on the sealed build |
| native | `opencode` | `PROJECT_NOTES.md` rendered from the same seed facts | OpenCode's own tools |

Same model, same task prompts, clarifications and visible checks; the
knowledge surface differs by design (table above). Thinking is
matched by tier. The harness arm's response policy is `reasoning-off` for
thinking-off tiers and `provider-default` for T5 when run with thinking on, and for TF, which lets both arms
inherit the runtime's setting (`field_check.RESPONSE_POLICIES`).

## Scenarios

From `scenarios/*/scenario.json`:

| Package | Track | Episodes | Probes (task / retained / retention / currency) | Negatives | Note |
|---|---|---|---|---|---|
| `display_labels_maintenance` | inherited | 1 | none declared; hidden test `e1.py` | 5 | intent pilot package; `review_status` draft pending approval |
| `retry_ledger` | accumulation | 3 | none declared; hidden tests `e1-e3.py` | 3 | intent pilot package |
| `ruleset_cache` | inherited | 3 | none declared; hidden tests `e1-e3.py` | 3 | intent pilot package |
| `supplier_quotes` | accumulation | 5 | 13 / 4 / 8 / 1 | 9 | long-horizon (`long_horizon.SCENARIOS`) |
| `entity_outbox` | accumulation | 5 | 13 / 4 / 9 / 1 | 10 | long-horizon |
| `late_fees` | inherited (2 seeds) | 4 | 7 / 3 / 7 / 1 | 8 | long-horizon |
| `late_fees_crowded` | inherited (57 seed facts) | 4 | 7 / 3 / 7 / 1 | 8 | exploratory only (`long_horizon.EXPLORATORY`) |

Probe kinds (`scenario.PROBE_KINDS`, `scenarios/LONG_HORIZON_GOLD_REVIEW.md`):
`task` is decided by this episode's prompt; `retained` is restated by the
current prompt (regression); `retention` is decided by an earlier reason or a
seed record and measures `correctness` or `cost`; `currency` is behaviour under
a superseding rule.

Gold status: the long-horizon review says no approval file exists and no
campaign may start until its decisions are settled. `late_fees_crowded` is
declared exploratory and never enters the long-horizon campaign; including it
here requires James to promote it.

Proposed subset (**decision**): `late_fees_crowded` or `late_fees` (all 4
episodes; rule delivery, retention, currency), `supplier_quotes` (5 episodes;
accumulation track, cost and correctness retention), `retry_ledger` (3
episodes; the task used in earlier field checks) and
`display_labels_maintenance` (1 episode; maintenance).

## Cells and repetitions

A cell is (tier, arm, scenario). A run is one cell repetition with a fresh store
and a new run ID. Proposed **n = 3** runs per cell, **n = 5** for tiers within
one tier of the provisional floor, T1 and T2 on current evidence (**decision**).
Schedule: grouped by tier to keep one model loaded (T5 runs alone: 68 GB plus the 7 GB helper on 96 GiB),
repetitions interleaved across arms within a tier so warm caches do not favour
one arm.

## Episode policy

Per the maintainer's long-horizon decisions (2026-09-13): continue after a
hidden-check failure; stop only when an episode fails to complete. An episode
passes when it completes, the probes it introduces pass, and nothing that passed
in the previous attempted episode regresses. Horizon reached is the run of
leading passing episodes. Today's driver stops on any hidden-check failure; the
new mode must implement this policy.

## Primary outcomes

1. **Run pass**: all attempted episodes pass. For 1-episode packages, the
   hidden check passes within 1200 s.
2. **Horizon reached** per run (multi-episode packages).
3. **Floor tier**: the H5 rule over run pass rates pooled across the scenario
   subset.

## Secondary outcomes

- Probe outcomes by kind, from per-test hidden results (`crowding.probe_results`
  pattern): correctness retention and currency pass rates, both conditional
  (episode completed and its task probes passed) and unconditional.
- Knowledge delivery before the first edit: whether each deciding fact's claim
  reached the harness prompt, and in which section (`crowding-report`:
  `claim_before_first_edit`, `rules_claim_before_first_edit`). Not applicable to
  the native arm; recorded as such, never as zero.
- Cost: provider requests and tokens (input, output, reasoning) by purpose
  (`harness_action`, `harness_capture_note`, helper, probe); reads and searches
  before the first edit; elapsed and first-edit seconds. Native OpenCode totals
  are reported separately and never summed with provider totals (Lesson
  `e23cb3ec`).
- Harness lever events from `process.SYMBOLIC_EVENT_KINDS`, e.g.
  `constraint_coverage`, `edit_grounding`, `plan_grounding`, `replan_noop`,
  `replace_text_repair`, reported per tier as grounding versus control.
- Halts and terminal causes (`cause.py`): the unattended halt class and native
  `native_no_completion` / `deadline_native`.
- Capture accuracy, graded only if a primary result is interesting: evidence-bound
  reviewer judgments (`grading.record_review`) against each episode's
  `expected_fact_ids`, with stale and forbidden-claim rules from the long-horizon
  review.

## Analysis plan

- Per tier and arm: run pass rate pooled over the scenario subset, with an exact
  (Clopper-Pearson) 95% interval. Per tier and scenario: the paired difference
  harness minus native.
- H5 floor rule (**decision** for T and M): the smallest tier where the harness
  pooled pass rate is at least **T = 0.8** and the harness rate is at least the
  native rate minus **M = 0.1**. Report the interval beside the point estimate;
  with n = 3 over 4 scenarios (12 runs) an 11/12 rate has a lower bound near
  0.62, so the floor is stated as a tier, not a parameter count.
- H6 is tested two-sided on the four headline frontier comparisons. Rates get
  exact (Clopper-Pearson) intervals on the harness-minus-native difference.
  Tokens and wall time per passed task are reported with their per-run
  distributions. The pre-registered direction is supported when native is not
  below the harness by more than M on pass rate and knowledge-use probes, and is
  lower on tokens and time per passed task. Any comparison in the opposite
  direction beyond M, or with a lower harness cost, is reported as a result
  against the hypothesis.
- Infrastructure failures (`cause.py` class `infrastructure`: preflight or
  infrastructure failure, fatal native error, killed return codes, service
  errors) are excluded and the run is repeated with `--replacement-for`, at most
  twice. After that the run is reported as lost, never as a model failure.
- `unknown` terminal causes invalidate the run and require an instrumentation
  fix under a new identity (Constraint `d2569270`).
- No post-hoc changes: no lever, prompt, threshold or scenario change after the
  first scored cell. A change is a new identity.

## Compute budget

Observed cell times (exploratory field checks, crowded late-fees episode 1):
passing 26-27B harness cells took 100-483 s (builds B-F); failing cells ran to
the 1200 s deadline; native Qwen3.5-9B took 62 s; the Llama native cell was
killed at 1205 s. The budget below assumes 10 minutes per attempted episode
(passes near 4-8 minutes, failures at 20).

| Matrix | Tiers (local) | Arms | Episodes per run | n | Episodes | Hours at 10 min |
|---|---|---|---|---|---|---|
| Full: all 7 packages | 5 | 2 | 25 | 3 | 750 | about 125 |
| Proposed subset (4 packages, above) | 5 | 2 | 13 | 3, and 5 for T1-T2 | 494 | about 82 |
| Lean: `late_fees_crowded` (4) + `retry_ledger` e1 + `display_labels_maintenance` | 5 | 2 | 6 | 3 | 180 | about 30 |

The frontier tier adds the same episode counts on hosted time and cost, capped
by the budget decision.

## Frontier anchor

- Provider: OpenRouter, an OpenAI-compatible endpoint; model ID pinned in the
  design (**decision**).
- Key handling: read from an environment variable at run time, never written
  to configs, receipts, journals or logs.
- Egress: only the pinned hosted origin is allowed; the hosted proxy already
  enforces exact-host HTTPS allowlists (`hosted_proxy.DomainProxy`).
- Data: scenario packages are synthetic fixtures (`SOURCE_MAPPING.md`), but
  prompts, seeded knowledge and code leave the machine. James's call.
- Cost cap per campaign (**decision**), enforced by stopping the schedule.
- Native arm: OpenCode with an OpenRouter provider, not Claude Code.

## What must be built before sealing

| Need | Files | Current state |
|---|---|---|
| Confirmatory mode with its own design identity, schedule with repetitions, pooled scoring allowed | new `floor_study.py`; `config.py` (derivation, `verify_config`, approval payload); `__main__.py` (`init-floor-study`); `grading.py` (report per tier with intervals, pooling only within this identity) | modes are pilot, intent, evolution stages and field check; field check is episode 1 only and never scored |
| Long-horizon episode policy (continue after hidden-check failure) | `run.py`, `cause.py` | driver stops on any hidden-check failure |
| Hosted tier rows (provider, model ID, no weights fingerprint) | `model_table.py`, `config.py` preflight | every row requires weights and a fingerprint |
| Hosted model traffic for the harness and OpenCode arms | `proxy.py` or a new recorder, `run.py`, `adapters.py` | `ModelProxy` accepts only `http://127.0.0.1:PORT/v1`; `DomainProxy` is CONNECT-only (TLS opaque) and used only for Codex backends; `run.py` requires the cell model to be loaded in LM Studio for non-Codex backends; OpenCode's provider is `@ai-sdk/openai-compatible` pointed at the local endpoint with a placeholder key |
| Tool-call compatibility probe per tier on the sealed build | `native_contracts.py`, `src/harness/response.rs` | the probe exercises the JSON response contract; build G adds a tool variant |
| Gold approval of the selected long-horizon packages | `scenarios/*`, `approve-gold` | no long-horizon approval exists |

Sealing steps:
1. James settles the decisions below.
2. Freeze the final build (build G after its regression cells) and qualify it.
3. Implement the mode and tests.
4. Approve the selected packages' gold.
5. Hash this document into the design identity.
6. James runs `approve-gold` for the study configuration.
7. Preflight every tier, including tool-call probes, then run the schedule without code changes.

## Known threats to validity

- One machine (M3 Ultra, 96 GiB) and one runtime (LM Studio MLX) for every
  local tier; quantization differs by tier.
- Nondeterminism at temperature 0: Qwen3.5-9B passed the crowded probe on build
  E and failed on build F with the same levers (Lessons `de463373`,
  `cb5cbfb0`). Repetitions are the only defence.
- Model and runtime format incompatibilities (JSON schema, tool-call parsing)
  can decide results; the per-tier preflight probe is a gate, not a result.
- Synthetic scenarios, and a scripted reviewer that approves plans and
  answers clarifications (`reviewer.py`).
- Knowledge surfaces differ by design: graph and dossiers versus rendered
  `PROJECT_NOTES.md`.
- The native cell of each pair may find warm caches; repetitions interleave
  arms to spread that.
- The harness evolved throughout the exploratory work; after sealing it must
  not.

## Exploratory evidence (motivation, not results)

All are single-run field checks on episode 1 of the crowded late-fees probe
unless noted; none is scored or pooled.

| Lesson | What it showed |
|---|---|
| `7206b8a1` | Project rules plus guidance got Qwen3.8-27B and gemma-4-26b-a4b to read a rule's defining code before planning; both passed where both had failed |
| `ae50747f` | Dropping the record inventory and compacting claims cut tokens 42-59% with no loss |
| `d23b1dae` | Qwen3.5-9B's harness run refused a disputed plan until the deadline; its native run kept NP-7 but missed rounding |
| `672acdbc` | On build D, grounding gave Qwen3.5-9B the right value, then it looped replan while planning |
| `de463373` | On build E, Qwen3.5-9B passed once planning offered only planning actions (432 s, 393k tokens) |
| `cb5cbfb0` | On build F the 9B pass did not repeat: identical-action loops to the deadline |
| `ca733260` | gemma-4-26b-a4b leaked stray braces into replace text and exhausted repair |
| `1cbc1917` | A 120 s total client timeout cut a long streamed action and ended a correct task |
| `9ef26b02` | Llama-3.3-70B paid about 55 s of JSON schema setup per harness request |
| `ddbe810c` | Hermes-4-70B made no tool-driven progress in four cells, thinking off or on |
| `30a39e45` | LM Studio did not parse Llama-3.3's tool calls; OpenCode looped identical requests |

## Decisions needed from James

1. Threshold **T** and margin **M** for the floor rule (proposed 0.8 and 0.1).
2. Repetitions: n per cell (proposed 3, and 5 for tiers near the floor).
3. Scenario subset, including whether `late_fees_crowded` is promoted from exploratory or `late_fees` is used instead.
4. Tier list: confirm the five local tiers above, and whether to add Qwen3.6-35B-A3B to separate total from active size between T2 and T5.
5. Frontier model ID and cost cap.
6. Whether the frontier arm runs at all, given that data leaves the machine.
