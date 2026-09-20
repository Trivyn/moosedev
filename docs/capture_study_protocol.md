# Capture study: pre-registration

Status: DRAFT. Not sealed. No scored run has been executed.

This document is hashed into the capture study's design identity
(`bench/harness_study/capture_study.py`). Editing it after sealing changes the
identity rather than silently rescoring finished runs.

Governing records: Requirement `f4b3e3d6` (a definitive generic-harness
comparison and a model-size floor), AD `2399f114` (this study), AD `3f450d51`
(OpenCode is the generic comparator at every tier), AD `f19ce43b` (the
three-arm baseline), Requirement `d2a54c9c` (long-horizon work is where the
value is judged). The accepted typed records govern this document if they
disagree with it.

## Question

Does the MOOSEDev harness make small local models record durable knowledge that
a generic harness does not — and is what it records actually usable later?

This is the harness's founding premise and no measurement has tested it. The
sealed floor study (Lesson `0d2ca6de`) compared the harness against OpenCode
with a **markdown notes file**, which needs no tool-calling discipline at all.
Its own scope note says so: a tie there "never tested the harness's founding
claim that small models will not call MCP tools."

The arm that would test it was deferred on purpose, as Alternative `75fd7e89`:
adding an OpenCode-with-MOOSEDev-MCP arm was set aside because "local-model pull
reliability through MCP is a known confound and doubles the baseline cells."
That confound is the hypothesis this study exists to measure.

## Rigour

Confirmatory. Runs are scored and pooled with the runs of this design identity
alone. Thresholds, arms, scenarios and outcomes are fixed here before the first
scored run; N per cell is fixed at sealing. Field checks and pilots are never
pooled with these runs.

## Arms

Three, on the same model, the same scenario package and the same seeded graph.
Only the way memory is offered differs.

| Arm | Backend / condition | Memory |
|---|---|---|
| **B-mcp** | `opencode_mcp` / `opencode_mcp` | OpenCode holding the MOOSEDev MCP tools. The model decides whether and what to capture. |
| **C-harness** | `harness` / `harness` | The MOOSEDev harness. Capture is structurally enforced: the runner decides when capture is due, the model supplies only a prose note, and the daemon types it. |
| **A-notes** | `opencode` / `without` | OpenCode with `PROJECT_NOTES.md` and no MOOSEDev. |

**B-mcp against C-harness is the comparison that answers the question.** A-notes
only bounds what the enforcement costs, carried from AD `f19ce43b`.

Both graph arms hold the **same** seeded graph and the same code index. Both MCP
arms receive the identical guidance paragraph (`seed.GUIDANCE`). The native tool
policy is identical across both OpenCode arms; the MCP arm additionally allows
the MOOSEDev tools by exact name, because a wildcard deny would silence them and
manufacture this study's own finding.

## Scenarios

Accumulation track only: `entity_outbox`, `supplier_quotes`, `retry_ledger`.
Capture is meaningless without a later episode to consume what was captured
(Requirement `d2a54c9c`), and the inherited-track packages seed their knowledge
rather than requiring the agent to write it down.

Each package carries per-episode `expected_fact_ids`, `forbidden_claims` with
episode windows, hidden executable probes, and a negative control for every
retention and currency probe. Gold must be approved by the maintainer
(`approve-gold`) before sealing; every package is currently `review_status:
pending`.

## Primary outcome

**Retention.** The share of `retention` probes passed, pooled per arm. A
retention probe is an executable hidden test whose `decided_by` pointer names a
*strictly earlier* episode's prompt or a seed fact, and the conversation history
is destroyed between episodes — so the only way to pass is to have written the
reason down and recovered it.

Mechanism outcomes are reported beside the primary and never in place of it.

## Mechanism outcomes

Computed deterministically from the sealed per-episode `kg.nq` and event stream
(`bench/harness_study/capture.py`):

- **capture rate** — episodes producing at least one durable record.
- **capture validity** — per record: SHACL conformance from the episode
  checkpoint, a non-degenerate title, a non-empty description, and at least one
  link to a component, record or code entity. An orphaned record is findable by
  lexical luck alone.
- **capture attempts** — classified capture tool calls, and their errors.
  Reported for the arms whose model calls the tools; `null` for C-harness, where
  the model is never asked to.
- **task completion** — to detect whether enforcement costs capability. Lesson
  `1aa42909` found it did not at episode 1.

Semantic truth is not computed from graph shape. **Capture fidelity** — does the
record describe the decision the episode actually made — comes from retained
claim-level judgments against each scenario's gold facts, blind to arm.

### Two asymmetries that must be stated, never smoothed over

1. C-harness writes proposals that a scripted structural reviewer ratifies;
   B-mcp writes directly. Both are therefore measured at the **end** of the
   episode, after review, which is the only arm-neutral moment.
2. A harness capture scored `Restates` deliberately produces **no record** —
   only a receipt and links onto the record it restates. A zero-record episode
   is not necessarily a capture failure, which is why attempts and records are
   reported separately and never summed.

## Pre-registered expectation and falsifiers

C-harness should trivially exceed B-mcp on capture **rate**, since one enforces
what the other leaves to the model. **That number alone proves nothing.** The
claim under test is that C also wins on retention and on validity — that
enforced capture is not merely more frequent but more useful.

- **Falsifier 1 — the premise is weakened.** If B-mcp reaches adequate capture
  unprompted (valid-capture rate ≥ `adequate_capture`, default 0.5, a majority
  of episodes producing valid, recallable records), then the model would have
  done without the harness what the harness exists to enforce.
- **Falsifier 2 — the records are unusable.** If C-harness wins on capture rate
  but its retention is below B-mcp's by more than `retention_margin`
  (default 0.0), the harness is manufacturing records nobody can use, which is
  worse than the tie the floor study already reports.

## Analysis plan

Pooled per arm, per tier, with Clopper-Pearson exact 95% intervals. Per-scenario
breakdowns reported but not used for the decision. Infrastructure failures are
classified separately from agent failures and retained in the denominator;
attempts are never selectively repeated.

## Known threats to validity

- **Gold is unapproved.** No result may be reported before `approve-gold`.
- **Quantization.** AD `2399f114` names 8-bit to inherit the matched regime of
  AD `93271bea`. The model table currently pins 5-bit and 4-bit rows. Whichever
  is used must be stated with the result; the two must not be pooled.
- **Scripted, not human, review.** The reviewer accepts structurally valid
  proposals without consulting gold. This measures unguided capture, not human
  curation quality, exactly as the approved protocol requires.
- **Client context is pinned at 32,768 tokens** while the tiers load far larger
  windows. Results are conditioned on that budget. Constraint `dbb5beda` applies
  if a reasoning regime is ever varied here.
- **Three arms cost half again what two did.** 3 arms × tiers × scenarios ×
  repetitions; at `EPISODE_SECONDS = 1200` this is an overnight-class campaign
  even at one repetition.

## What must be settled before sealing

- [ ] `approve-gold` on the three accumulation packages.
- [ ] Quantization decided, and model-table rows added if 8-bit.
- [ ] N per cell fixed.
- [ ] Judge model named, and its identity resolved against the provider.
