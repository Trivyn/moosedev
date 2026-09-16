# Large-tier study: pre-registration

**Status: draft, not sealed, not approved.** Written 2026-09-16 for James Adam's
review. No mode, design identity or approval exists for this study yet.

This document is deliberately separate from `docs/floor_study_protocol.md`. That
document is hashed into the running floor study's design identity and is
verified on every cell, so editing it mid-campaign would break the campaign
(Lesson `de8baa05`). This study gets its own document and its own hash.

Governing records: Requirement `f4b3e3d6` (a definitive generic-harness
comparison and model-size floor), AD `3f450d51` (OpenCode is the generic
comparator at every tier), AD `c944806b` (the confirmatory floor-study mode),
and the composition decision recorded for this study. Constraints `40d414ce`,
`83c18bb9` and `d2569270` apply unchanged.

## Question

The floor study asks where the floor is. This study asks the other end of
Requirement `f4b3e3d6`: **where do the harness's benefits stop, and does the
harness begin to do harm to a capable model?** It is the local half of that
question. The frontier half (Sonnet via OpenRouter, hypothesis H6) is a third
identity, blocked on hosted traffic recording.

It also closes a gap the floor study's sealed tier list names and accepts:
nothing there separates total parameters from active parameters above T2
(26B total, about 4B active).

## Tiers

| Tier | Model | Size | Runtime | State |
|---|---|---|---|---|
| L1 | `kimi-dev-72b-dwq` | dense 72B | MLX 4-bit DWQ, 41 GB, `qwen2`, 131072 ctx | ready |
| L2 | `qwen3.5-122b-a10b` | MoE 122B, about 10B active | MLX 4-bit, 70 GB, `qwen3_5_moe`, 262144 ctx | ready |

Both confirmed indexed by LM Studio on 2026-09-16 with `trained_for_tool_use: true`.

Ordered by capability as the mode requires; L1 is dense, L2 is the mixture of
experts, so the pair separates total from active capacity at one size class.

Both are Qwen-family tool-call formats, which is the point. The two 70B models
excluded from the floor study failed on runtime, never on capability: LM Studio
did not parse Llama-3.3's tool calls and it paid about 55 s of schema setup per
request (Lessons `30a39e45`, `9ef26b02`), and Hermes-4-70B made no tool-driven
progress in four cells in either thinking mode (Lesson `ddbe810c`).

**Nemotron-3-Super-120B-A12B is dropped** (James, 2026-09-15): a custom
`configuration_nemotron_h.py` and hybrid reasoning repeat exactly that runtime
risk, in a size class L2 already covers.

Each tier enters only after a qualifying **harness episode** with a plan and an
edit on the sealed build. A clean tool-call probe is not sufficient; that is the
whole lesson of `ddbe810c`.

Thinking is off for both tiers, so `reasoning-off` for the harness arm, matched
across arms.

## Arms, scenarios, rule

Identical to the floor study, so the two read against each other even though
their identities never pool:

- **Arms**: MOOSEDev harness (native tool calls, AD `84dfd153`) against OpenCode.
- **Scenarios**: `late_fees`, `supplier_quotes`, `retry_ledger`,
  `display_labels_maintenance` — 13 episodes per run.
- **Build**: `98d0bfa455d845c8`, the floor study's sealed build.
- **Repetitions**: n = 3 per cell.
- **Rule**: T = 0.8, M = 0.1, exact Clopper-Pearson intervals.

2 tiers x 4 scenarios x 3 repetitions x 2 arms = **48 cells, 156 episodes**.

## What is expected, stated before the runs

The pre-registered direction, so a surprise is visible as one: at these sizes
the harness's **grounding** levers should matter less than they do at 9B to 31B,
because a capable model finds more of the deciding knowledge on its own, while
the **control** levers cost the same or more (Requirement `e9166711`). If the
harness begins to trail OpenCode here, that is the local edge of its usefulness
and is reported as such, not explained away.

Both directions are reported with exact intervals. A harness advantage at 122B
is a reportable result.

## Compute

Rough budget at 10 minutes per attempted episode: **about 26 hours**, likely
more, since these models are slower per token than every floor-study tier.

Each tier runs **alone**: 41 to 70 GB of weights plus the 7 GB helper against
96 GiB of machine memory. Neither can share the machine with another campaign,
so this study is strictly serial with the floor study.

L2 is the tight one: 70 GB plus the 7 GB helper is 77 GB against 96 GiB, leaving
about 19 GiB for the operating system, the daemon and the driver. Confirm the
pair actually loads together before the qualification cell, since every harness
cell needs the helper resident at the same time as the coding model. If it does
not fit, L2 cannot run the harness arm on this machine at all, which is a
finding about the machine rather than about the model, and must be reported as
such rather than as a model failure.

## What must be built before sealing

| Need | State |
|---|---|
| Qwen3.5-122B-A10B download | complete, 14 of 14 shards, indexed |
| L2 plus helper co-residency at 96 GiB | unverified; 77 GB of weights, check before the qualification cell |
| `model_table.py` rows with pinned weights fingerprints | not written; the edit changes the driver fingerprint, so it must land between campaign tiers or after the campaign, then a fresh preflight |
| A qualifying harness episode per tier | not run |
| Per-study protocol document hashing | `floor_study.DOCUMENT` is hardcoded to the floor protocol; make it selectable per design, or fold this section into that document once no campaign is running |
| Schema-4 approval from James | not signed |

## Not in this study

- The Sonnet frontier anchor and hypothesis H6: its own identity, blocked on a
  recording hosted proxy. `ModelProxy` accepts only a local
  `http://127.0.0.1:PORT/v1` upstream, and `DomainProxy` leaves TLS opaque and
  unrecorded, so a hosted tier cannot yet produce the evidence every local cell
  produces under Requirement `bc93e612`.
- The native no-notes third arm (AD `ba086e95`), deferred with the floor study.
- Nemotron-3-Super-120B-A12B.
