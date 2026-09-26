# Context management: design plan

**Status: implemented.** Written 2026-09-16 from floor-study evidence; the
delivery contract was completed 2026-09-20. The sealed campaign was not
modified; any comparative rerun still requires a new harness build and study
identity.

## The measured problem

On multi-episode packages the harness's rebuilt prompt grows until the run dies,
either rejected for exceeding the 32,768-token budget or timed out in prefill.
Decomposing archived prompts by section (mean bytes per request):

| section | e1 | e2 | e3 | e4/e5 | growth |
|---|---|---|---|---|---|
| **dossiers** | 246 | 8,079 | 17,476 | **31,247** | **127x** |
| rules | 0 | 3,917 | 4,963 | 6,423 | — |
| topic_evidence | 247 | 594 | 943 | 4,210 | 17x |
| inventory | 125 | 801 | 1,372 | 2,845 | 23x |
| instructions | 4,233 | 1,126 | 1,072 | 1,711 | flat |
| knowledge, navigation | ~500 | ~500 | ~500 | ~500 | flat |

Cell 42, T2, `supplier_quotes`. Cells 26 and 48 show the same shape (120x, 26x).
By the last episode dossiers are about 70% of the prompt. Everything else is
flat or small in absolute terms.

The native arm shows no trend on the same packages, because OpenCode manages its
own context instead of rebuilding a knowledge-laden prompt each step.

## Why it happens

Three design choices compose into unbounded growth. Each is defensible alone.

1. **The prompt is rebuilt statelessly every step.** Prior content is not
   carried, so anything the model still needs must be re-sent every time. Only a
   small "Recent conversation" section survives between steps.
2. **The working set only grows.** Dossiers are rendered for the entities in
   scope, and as a task touches more files across episodes, more entities enter
   and each accumulates more linked records. The model captures roughly one
   record per episode, but its *associations* multiply: measured definition
   anchors per episode ran 5, 4, 20, 15 and derived associations 0, 5, 22, 14.
   Growth is in edges, not records.
3. **Every direct record renders its complete claim** (AD `21855a2a`), and
   dedup is scoped to a single push — "a record already shown earlier in the
   same push renders as a pointer". There is no dedup across steps, and there
   cannot be while the prompt is stateless.

Constraint `212a2026` then makes the failure loud by design: no bound may drop a
direct record's claim or an accepted Constraint, and "required harness prompt
context is never truncated: an over-budget prompt fails loudly instead."

**So the harness is behaving exactly as specified.** The specification has no
concept of a working set that outgrows the window. That is the gap.

## The tension this design must resolve

Bounding naively re-creates the failure the claims work fixed. Lesson `f07aacbb`
established that a deciding Constraint delivered as a name only is *not
delivered*: both harness arms failed the crowded probe when NP-7 arrived as an
inventory entry. Truncating dossiers would reintroduce exactly that.

The resolution is to **bound by scope, not by truncation**:

- Knowledge that decides the current step is always rendered in full.
- Knowledge outside that scope is named, counted, and *retrievable*, not
  silently dropped.
- Retrievability is real: `search` consults accepted knowledge (commit
  `076cf14`), so the promise matches the action schema (Lesson `816063dc`).

This is also what invariant #5 asks for — external, queryable memory rather than
stuffing the window — which the harness currently violates on its own behalf.

## What already exists, and why it does not help yet

The MCP tooling needed dossier pruning and got it. `render_dossiers_within`
bounds a push: sections render whole while they fit; one that does not keeps its
heading, its direct records with claims and its component's accepted Constraint
titles, and a closing line names what was shortened and says to retrieve it with
`get_entity_dossier`. That is already the scope-not-truncation pattern this
document argues for, implemented and tested.

**The harness does not use it.** `harness_file_dossier` takes no bound and ends
with `render_dossiers(&dossiers)`, which is `render_dossiers_within(dossiers,
None)`. MCP threads a caller's `max_bytes` through; the harness path has no
parameter for one.

But wiring it through is not the fix on its own, because the growth is in direct
records carrying full claims, and that is precisely what the bound must protect
under Constraint `212a2026`. The protected core is what grows.

**One clean win is available first.** AD `21855a2a` promises a record already
shown in the same push renders as a pointer, and it does, within one file.
`harness_file_dossier` is called once per file with fresh dedup state, while the
prompt carries several file dossiers. Measured on late-episode prompts:

| cell | record headers | full claim bodies | distinct records |
|---|---|---|---|
| 48 e4 | 56 | 38 | 18 |
| 42 e5 | 119 | 33 | 19 |

In cell 48, `tests/test_visible.py` contributed 23,318 bytes re-rendering claims
`fees.py` had already rendered in the same prompt. Roughly 30-40% of late-episode
dossier bytes are full claims the prompt already contains elsewhere. Carrying one
`ShownInPush` across every file dossier in a prompt removes that, drops no
knowledge, and does not touch `212a2026`'s protected core. Do this before
reaching for a byte bound.

## Proposed module

A `context` module owning one question: **given a budget, what does this step's
prompt contain?** It is deterministic and symbolic; no model call decides what
the model sees (invariant #1).

Responsibilities:

1. **Budget accounting.** Estimate the rendered size of every candidate section
   against the client budget, reserving room for the output schema. Today
   nothing owns this; the prompt is assembled and the overflow discovered at
   request time.
2. **Step scope.** Decide which entities are in scope for *this* step, rather
   than for the task so far. A plan step, an edit step and a check step need
   different sets.
3. **Priority ordering.** Accepted Constraints and the deciding claims for the
   step's files first; then linked evidence; then dossiers for secondary
   entities; then inventory and topic evidence. Constraint `212a2026`'s
   protected core is the floor, not the whole set.
4. **Degradation ladder** per record, in order: full claim → first sentence of
   the claim → title with a retrieval pointer → counted omission line. A record
   never vanishes silently.
5. **Receipts.** Journal what was included, shortened and omitted, and why, so a
   report can attribute an outcome to delivery rather than guess (invariant #6).
   This also makes the lever measurable per tier, as Requirement `e9166711`
   requires.

## Implemented resolution

The harness now owns budget accounting at both sides of the protocol without
creating a second retrieval policy in the runner:

1. `Runner::next_last_result_budget` uses the same mandatory prompt and output
   schema accounting as prompt assembly to compute a safe next-observation byte
   capacity, reserving the proven worst-case JSON expansion of the journal
   preview the pending search will add. Prompt assembly checks the invariant
   again after context refresh and fails loudly rather than clipping graph
   evidence if concurrent revision growth consumed the capacity.
2. `ContextRequest.max_bytes` carries that capacity for evidence-only search.
   The daemon remains the sole owner of record selection and degradation.
3. Search evidence is rendered as atomic record blocks. From the
   lowest-priority record upward, a block degrades from full claim to first
   sentence, then title plus retrieval pointer, then counted omission. Accepted
   Constraints stop at title; a protected core that cannot fit is an error.
4. `ContextDeliveryReceipt` records the requested and rendered bytes and one
   typed tier and reason per selected record. It is copied into the durable
   `KnowledgeSearchResult`; `evidence_iris` contains only records actually shown
   to the model.
5. Repository results spend only what remains and are admitted as whole lines.
   Consequently the generic head/tail observation preview no longer chooses
   which part of a graph record survives.

File dossiers retain the earlier complementary safeguards: one deduplication
state per prompt and a daemon-owned claim budget that preserves the complete
record inventory with a counted by-kind notice.

## Source, the section the study did not grow

The study's files were small, so working-set source stayed mandatory and whole.
The first real multi-file plan outgrew it at once. badciv task `59b33920`
(qwen3.8-27b, 65.5K-token window, 2026-09-24) stopped with "prompt plus output
schema exceeds configured context budget". Its last prompt that went through
was 95.5 KB against a 99 KB budget:

| section | bytes |
|---|---|
| current source (every read or edited file, whole) | 49,894 |
| project rules | 20,868 |
| recent conversation | 8,031 |
| accepted knowledge | 6,767 |
| everything else | about 10,000 |

The next step added a 12 KB test file, and every later step would have built
the same oversized prompt. The failure was loud but said nothing a human could
act on.

The same resolution applies: bound by scope, not by truncation. Source shown in
full is capped at two fifths of the prompt budget. Within that cap files are
ranked by what the step needs: the file read or edited last, the files the
latest failed command names, then the rest by recency. (A first version ranked
the last edit second; in task `c75d5d20` that kept an unrelated file in full
while the three files the model was debugging rotated in and out of outlines,
and it re-read them for 70 events.) The rest are outlined from their
declarations, and none is cut or left out. An edit to an outlined file becomes a
read first. A protected part that still cannot fit stops the task with its
sizes (`context_overflow`). Replayed offline on the failed step, the prompt is
71 KB: eight of ten files in full, and two outlined in 1.3 KB.

## Prompt order and the prefix cache

A local server such as LM Studio keeps the previous request's KV cache and
reuses it up to the first byte where the next prompt differs. Only the rest is
prefilled again. The harness rebuilds its prompt every step, which is what
keeps it bounded without compaction, so how much it reuses depends on where the
changing parts sit.

On badciv task `3ba41310` (qwen3.8-27b, 101 action prompts averaging 78 KB),
each prompt shared 24.8% of its bytes with the request before it, leaving
58.6 KB to prefill each step. OpenCode's append-only conversation on the same objective ran its
late steps in about 5 s against the harness's 87 s, with nearly the same total
input tokens. Two causes put the changing bytes near the front:

- The prompt opened with the recent conversation. Every tool observation the
  model asked for while working (an `inspect` page, a read confirmation,
  command output) was also replayed as an assistant turn, so the conversation
  changed on 67 of 100 steps and the thirteen 2 KB inspect pages of one search
  filled it.
- Full source was ordered by path, so an edit to one file changed every file
  after it.

The prompt is now ordered by how rarely each part changes: role and guidance,
project rules, action meanings, objective, accepted knowledge, dossiers, source,
repository paths, conversation, then the harness state (human guidance, mode,
phase, plan, reads, edits, checks, allowed actions) and the observations. The
conversation still precedes the authoritative state it may contradict. Full
source is shown with files never edited first, in read order, then edited files
least recently edited first. Tool observations are no longer replayed as
conversation turns while the task works, and the conversation window drops old
turns 4.8 KB at a time, so between trims it only grows by appending. The prompt
size and every budget are unchanged.

Replayed offline over the same journal, moving the sections and ordering the
source by edit age raises reuse to 65%, or 27 KB to prefill per step. The
observations and state, about 7 KB, change every step by design.
`python3 -m bench.harness_study prefix-reuse <task journal>` measures a run.

The first rerun with this order (badciv `f2fe1f61`, same model and objective)
measured 92.8% reuse, 6.4 KB to prefill per step, and a median action latency
of 6.2 s against 66.8 s. That run then looped on `inspect`, and its reuse is
flattered by the loop's near-identical steps; the next clean run is the
measurement to quote.

## The plan, bounded where it is shown

The plan is harness state, and it is bounded like source: at injection, not
where it is written. A 4,000-byte cap on the plan summary existed only because
the whole plan was repeated in every step's state. Once all 57 of a spec's
rules reached planning (badciv `e948c9c7`), qwen's plans came to 5.7, 5.1 and
4.0 KB, each refused, and the task parked before writing code. The model now
writes the plan the work needs; each step sees a 4 KB view focused on the files
it is about, with the complete file and check lists and a pointer to the whole
plan in the journal.

## Open questions, to settle with measurement not argument

- **Does scope-narrowing lose deciding knowledge?** Re-run the crowded-probe
  delivery gate against the bounded renderer: NP-7's claim must still arrive
  before the first edit. That gate exists and is offline.
- **Is the stateless rebuild itself worth revisiting?** A delivered-set memo
  across steps would cut far more than any renderer change, but it depends on
  the conversation section reliably carrying what was already shown. Measure how
  much of each prompt is genuinely re-sent content first; the crude
  longest-common-prefix figure was 20-40%, which is suggestive but not a
  decomposition.
- **What budget?** 32,768 is a pinned study parameter, not a law. But raising it
  alone is not a fix: prefill time is the other half of the failure, and cells
  26, 40 and 42 died on the 300 s first-chunk bound rather than on the budget.
- **Do the variant-C levers already apply here?** Inventory omission and compact
  claims cut tokens 42-59% on the crowded probe (Lesson `ae50747f`). The
  inventory is still present and growing in these prompts, so verify whether
  build `98d0bfa4` carries them before assuming the cheap wins are spent.

## Validation

- Offline first: the crowding delivery gate and this segment decomposition,
  which need no models.
- Then a harness build with the module, qualified as usual, and a study whose
  tiers are the same four and whose packages are the same four, so that
  harness-v2 can be read against both harness-v1 and native on the long-horizon
  episodes where the failure appears.
- The pre-registered expectation: prompt size stays flat across episodes, the
  `runner_error` family disappears, and delivery of deciding knowledge before
  the first edit is unchanged. If delivery degrades, the bound is wrong even if
  the runs survive.
