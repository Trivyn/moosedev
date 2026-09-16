# Context management: design plan

**Status: draft for review.** Written 2026-09-16 from floor-study evidence.
Nothing here is implemented. Any change is a new harness build and a new study
identity; none of it can touch the sealed campaign.

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
