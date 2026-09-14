# Long-horizon scenario review

**Status: draft for maintainer gold review, revision 3 after blind reader audit
round 2. No approval file exists, no model has run on these packages, and no
campaign may start until the decisions below are settled and the gold is
approved.**

MOOSEDev's value is judged on long-horizon, multi-turn work. These three
packages carry code and durable knowledge across four or five episodes. Their
later episodes are decided by an earlier episode's reason or a seeded record.
All three are fully synthetic; see each package's `SOURCE_MAPPING.md`.

| Package | Track | Episodes | Probes: task / retained / retention / currency | Negatives | Validation cases |
| --- | --- | --- | --- | --- | --- |
| `supplier_quotes` | accumulation | 5 | 13 / 4 / 8 (5 correctness, 3 cost) / 1 | 9 | 39 |
| `entity_outbox` | accumulation | 5 | 13 / 4 / 9 (4 correctness, 5 cost) / 1 | 10 | 41 |
| `late_fees` | inherited (2 seeds) | 4 | 7 / 3 / 7 (5 correctness, 2 cost) / 1 | 8 | 33 |

Per episode, each package's `DEPENDENCY_MAP.md` gives every later probe's
round-1 verdict, what it measures, its deciding sentence (JSON pointer and
quote), what the previous reference code reveals, the default a reader with
only that code and the current prompt would pick, why the probe is well-posed,
the expected discrimination, the gold records that decide it and how the
harness delivers them. Full prompts are in `scenario.json`; claims are in
`gold.json`.

## Design rules

1. **Clear with history.** For each later probe, a reader of all earlier
   prompts and the seeds gives one answer.
2. **Extend, don't repeat.** A later task creates a new instance of an earlier
   rule (a new code path or category, a deletion or compaction, a
   representation change). Later prompts do not restate the rule.
3. **No trick questions.** Later prompts never contradict an earlier decision.
   Early prompts state a rule's scope ("every code path that contacts the
   supplier", "every code path that removes an entity"), never "remember
   this". Wording is identical in both arms.
4. **Natural code.** Reference code is what a competent developer would write
   for each episode. It is never obscured to hide a rule and carries no
   comments explaining reasons.
5. **Timeless records.** Seed and gold records state rules and reasons, not
   facts that later episodes change by design (round 1 found a seed that went
   stale at e2 and forced a wrong answer under graph authority).
6. **Objective, independent probes.** Public API only, injected fakes and
   clocks, no private names; each hidden test method stands alone.

## Probe taxonomy

Every hidden test method is declared in `scenario.json` as one probe:

- **task**: decided by this episode's prompt.
- **retained**: decided earlier and restated by the current prompt or the
  immediately visible edit site (regression).
- **retention**: decided by an earlier reason or a seed record. Each carries
  `measures`, validated by the loader:
  - **correctness**: the previous code cannot show the rule, so an arm without
    the knowledge is expected to get it wrong;
  - **cost**: the rule is visible in the previous code. A graph-first agent
    should take it from the graph instead of reading and inferring it from
    source; the difference is cost, not correctness.
- **currency**: behaviour under a superseding rule, where stale knowledge
  predicts a different result. A reader with no memory is right by design, so
  blind reader condition A does not apply to currency probes; they
  discriminate only against stale notes or records.

Every episode has a task probe. After audit round 3, all three packages have
at least one audited correctness retention probe (PASS or weak PASS; currency
does not count) in every episode after e1. Every retention and currency probe has at least one negative fixture
that fails exactly the probes it declares.

## Measurement

- **Correctness probes** are scored pass or fail from per-test hidden results,
  as planned.
- **Cost probes** are also scored pass or fail, and are paired with per-episode
  cost counts:
  - reads and searches before the first edit;
  - searches answered from knowledge (the harness journals
    `knowledge_search` with record and repository hit counts);
  - reads of files the task never planned or edited;
  - provider requests and tokens.
- Harness counts come from the task journal; native counts come from the
  OpenCode tool log. Cost is reported separately from correctness, per arm,
  and never summed across arms.
- A cost probe that passes in both arms is still informative: the graph-first
  arm should reach the correct edit with fewer reads of source that only
  re-derive what an accepted record states.

## Scoring rules

- Evaluate each episode only against its `expected_fact_ids`. Future
  requirements earn no credit. Seeded `late_fees` facts are inherited
  knowledge, never newly captured knowledge; report seeded and new recall
  separately.
- **Supersession and stale credit.** A superseded fact leaves
  `expected_fact_ids` and moves to `stale_fact_ids` from its superseding
  episode. Presenting it as current is a stale-knowledge error; keeping it as
  clearly marked history is valid. A partially superseded fact carries
  `current_scope` and stays expected in that scope (`late_fees` `fees-np7`
  after NP-9); presenting its unconditional form as current is stale.
- **Forbidden claims** apply only within their `applies_from_episode` to
  `applies_through_episode` range (open-ended when null). Each is a check for
  an unsupported or stale assertion, not a string blacklist. Discussing a
  rejected alternative as rejected is valid.
- Credit semantically equivalent notes and graph records. Judge rationale from
  agent-visible prompts and facts, not from whether code matches a reference.
- Probe outcomes come from per-test hidden results. Tests listed in an
  episode's `retired_tests` are excluded from that episode's regression checks.

## Review attribution

For each retention and currency probe, the reviewer records how the arm had,
or lacked, the deciding knowledge:

- `stored_before`: judged from the previous episode's `PROJECT_NOTES.md` or
  `.moosedev/kg.nq` snapshot.
- `current_before`: whether that stored knowledge was current.
- `channels`, any of: **graph supplied** (in the task context or a dossier),
  **graph searched** (returned by the harness search), **source** (read from
  code), `notes`, `code_comment`, `tests`, `readme`, `none`.
- `used`, `rationale` and `evidence`.

This separates a capture failure from a retrieval failure. For cost probes it
also shows whether the arm took the rule from the graph or combed source.

## Defaults assumed while drafting

These can change at review; each change means re-validating, because package
hashes change.

- `late_fees` is inherited, so one package tests whether a **seeded** record
  stays current.
- No continuity cue in later prompts; scope wording lives only in the earlier
  prompts that state a rule.
- Same-conversation follow-ups are out of scope: they measure context, not
  durable knowledge.

## Decisions for James before any run

1. **Failure policy.** *Decided (maintainer, 2026-09-13):* continue after a
   hidden-check failure and stop only when an episode fails to complete, so one
   retention miss does not hide later episodes. Today's driver stops on any
   hidden-check failure; the long-horizon mode must implement the new policy.
   Per-test independence makes later probes readable.
2. **Primary outcome.** *Decided (maintainer, 2026-09-13):* an episode passes when it completes, the
   probes it introduces pass, and nothing that passed in the previous attempted
   episode regresses. Horizon reached is the run of leading passing episodes.
   Correctness retention and currency pass rates are the explanatory
   breakdown, reported conditionally (episode completed and its task probes
   passed) and unconditionally; cost counts are reported beside them.
3. **Build.** *Decided (maintainer, 2026-09-13; graph AD a238e9ba):* the next
   build carries claim-bearing entity dossiers and capture-time linking, not
   plan-time recall. The crowded late fees probe (offline gate on build
   1d31f454, blind audits, no model runs) showed that today's push never
   delivers an unlinked deciding record hidden among 52 realistic distractors
   (ranks 25 and 16), that plan-time recall reaches it for 0 of 3 blind plans
   (ranks 28 to 36), and that recall driven by read source reaches it only for
   one narrow read set. Claim-bearing dossiers deliver every code-linked
   record's claim for about 1 to 2.4 KB. Knowledge a small model builds over
   episodes travels on the links the harness creates at capture; unlinked
   knowledge is addressed by linking at capture or seeding (Lesson 6f238b1f).
   The probe's before cells still run on 1d31f454 to show whether models
   search when the push misses and how the native arm uses a large notes file.
4. **Native no-notes floor arm.** Open. It adds 6 cells and a new guidance key,
   and shows how much a well-kept notes file contributes. The alternative is
   an offline calibration on single later episodes started from the reference.
5. **Block order and model roles.** *Recommended:* Qwen3.8-27B first. Qwen is
   the model of record: primary outcomes and conclusions rest on its cells,
   and its first harness cell is the pre-block harness-bug check. The point of
   the harness studies is to find the floor, the smallest reasonable local
   model that works with the harness (maintainer, 2026-09-13); the v2 MCP-only
   setup with Codex and Claude Code effectively needed Sonnet-class models to
   use MOOSEDev reliably. Qwen first also gives horizon evidence sooner,
   because long horizons need early episodes to pass.
6. **Second model.** *Decided (maintainer, 2026-09-13; graph AD 68697d8d):*
   `google/gemma-4-26b-a4b` replaces Gemma E4B. E4B's floor evidence is
   already clear: its harness retry-ledger run never passed in v3, and after
   the replan fixes it never edited, continuing 82 replans to the deadline
   (Lesson d42f1fba). In long-horizon cells a failed early episode blocks the
   later ones, so E4B would mostly re-measure episode 1. gemma-4-26b-a4b is a
   mixture-of-experts model in the same family, 26B total and about 4B active,
   already downloaded and close to small-model speed; it tests whether active
   capacity or total knowledge sets the floor. Its cells run second, are
   reported separately, and a failure there is floor evidence, not a verdict
   on the harness. E4B stays at most an optional single-episode probe outside
   the comparison.
7. **Further floor bracketing.** Open. If Qwen succeeds and gemma-4-26b-a4b
   fails, the floor lies between 4B-active and 27B-dense. A third model would
   narrow it at the cost of 6 more cells: Qwen3.5-35B-A3B is on disk; a dense
   8 to 14B model would need a download.

8. **Thinking mode.** *Decided (maintainer, 2026-09-13):* pin the harness arm to reasoning off for
   both models (`MOOSEDEV_HARNESS_RESPONSE_POLICY=reasoning-off`) and disclose
   the arm difference. State on 2026-09-13: thinking is enabled in LM Studio
   for gemma-4-26b-a4b, and Qwen3.8-27B's provider default already thinks.
   The native OpenCode arm sends no reasoning option, so it inherits LM
   Studio's setting and thinks for both models. The harness's automatic
   response check resolved reasoning off for Qwen (its default returned
   reasoning with no message content) and provider default for E4B, so left
   on `auto` the harness mode for gemma-4-26b-a4b depends on what that check
   sees. Thinking in the harness arm, if wanted, is a separate later variable.

**Fixed, not a decision:** before any block, the first harness cell of the
block order (Qwen's, under the recommended order) runs alone into a discarded store and its journal is read. An
unexplained terminal path or a harness-class cause (runner error, controller
invariant, daemon rejection, infrastructure, unknown) stops the campaign
before any hours are spent.

Scale: 12 cells over two models, two arms and three packages; 14 episodes per
model and arm, 56 in all; at the 1200-second episode budget, at most about 19
hours before indexing and model loads.

## Retention probes: what each measures

Ratings are against a reader with the previous reference code and the current
prompt. Audit verdicts are from the blind reader audit rounds below (R1, R2, R3).

| Package | Probe | Measures | Audit verdicts | Expected discrimination |
| --- | --- | --- | --- | --- |
| supplier_quotes | e2-refetch | correctness | R1 INFERABLE (prompt scoped dedupe); prompt revised; R2 PASS (weak) | high |
| supplier_quotes | e2-basket-rounding | cost | R1 INFERABLE | low |
| supplier_quotes | e2-basket-unknown | correctness | R1 PASS | medium |
| supplier_quotes | e3-discount-rounding | correctness | R1 INFERABLE; discount restated per line; R2 PASS (weak) | medium |
| supplier_quotes | e4-warm-unknown | correctness | R1 PASS (weak) | medium |
| supplier_quotes | e5-orders-unknown | correctness | new in revision 2; R2 PASS | medium to high |
| supplier_quotes | e5-micro-rounding | cost | R1 INFERABLE | low |
| supplier_quotes | e5-new-perishable | cost | R1 INFERABLE | low |
| entity_outbox | e2-delete-event | correctness | new in revision 2; R2 PASS | high |
| entity_outbox | e2-recreate | cost | R1 INFERABLE | low |
| entity_outbox | e3-compaction | cost | R1 INFERABLE | low |
| entity_outbox | e3-delete-many-atomic | correctness | new in revision 3; R3 pending | high |
| entity_outbox | e3-delete-many-events | cost | R2 INFERABLE; relabelled cost | low |
| entity_outbox | e4-epoch-compaction | cost | R1 INFERABLE | low |
| entity_outbox | e4-patch-full | cost | R2 INFERABLE; relabelled cost | low |
| entity_outbox | e4-ack-many-atomic | correctness | new in revision 3; R3 pending | medium to high |
| entity_outbox | e5-rename-events | correctness | new in revision 2; R2 PASS (weak) | medium |
| late_fees | e1-half-up | correctness | R1 PASS | medium |
| late_fees | e1-charity | correctness | R1 PASS | high |
| late_fees | e2-foundation | correctness | R1 PASS | high |
| late_fees | e3-penalty-exempt | correctness | R1 RECORDS-FAIL; seed clause removed; R2 PASS | high |
| late_fees | e3-penalty-half-up | cost | R1 PASS (weak); R2 INFERABLE; relabelled cost | low |
| late_fees | e4-association-penalty | cost | R1 INFERABLE | low |
| late_fees | e4-collection-exempt | correctness | new in revision 2; R2 PASS | medium to high |

Currency probes, not scored by condition A:

| Package | Probe |
| --- | --- |
| supplier_quotes | e4-guarantee |
| entity_outbox | e5-rename-epoch |
| late_fees | e4-association-np9 |

## Validation evidence

```
python -m bench.harness_study validate --scenarios supplier_quotes entity_outbox late_fees --output <path outside target>/long-horizon-validation.json
```

For each package this runs, in disposable network-denied sandboxes on the
system Python 3.9: every reference's visible and hidden tests with every probe
observed by name and passing; `project/` failing e1's hidden test; each
reference against the previous episode's hidden tests, excluding retired
tests; each negative passing visible tests and failing exactly its declared
probes; and an offline check that the resolution target stays exactly one
definition in `project/` and every reference. The pilot `validate` (no
`--scenarios`) is unchanged at 18 cases.

## Blind reader audit

Two conditions per retention probe, each answered in writing by a fresh
reader. Condition A: only the episode prompt and the complete previous source.
Condition B: A plus only the deciding records current at that episode. A
correctness probe that condition A answers correctly and confidently is
revised or relabelled cost; a probe that condition B answers wrongly means the
records do not carry the answer.

### Round 1 (2026-09-13)

Readers: fresh Claude Sonnet subagents, one per package episode. Condition A = episode prompt + clarifications + complete previous-state source only; answered and saved first. Condition B = A plus only the deciding accepted records current at that episode. Packets were split per episode so no reader saw later code. Grading compares answers with gold values computed by running each snippet against reference/eN and the matching negative overlay.

Verdicts: PASS = A wrong or not determined, B right and determined. INFERABLE = A right and determined (retention probe fails the audit). RECORDS-FAIL = B wrong (the deciding records do not carry the answer). CURRENCY = no-memory reader is expected to be right by design; the probe discriminates only against stale knowledge.

| Package | Q | Episode | Probe | Kind | A | B | Verdict |
|---|---|---|---|---|---|---|---|
| supplier_quotes | Q1 | e2 | e2-refetch | retention | right, determined (prompt scopes dedupe to one call) | right | INFERABLE |
| supplier_quotes | Q2 | e2 | e2-basket-rounding | retention | right, determined (per-unit to_cents in code) | right | INFERABLE |
| supplier_quotes | Q3 | e2 | e2-basket-unknown | retention | wrong (one supplier call) | right | PASS |
| supplier_quotes | Q4 | e3 | e3-discount-rounding | retention | right, determined (per-unit rounding convention) | right | INFERABLE |
| supplier_quotes | Q5 | e4 | e4-warm-unknown | retention | right, ambiguous | right, determined | PASS (weak) |
| supplier_quotes | Q6 | e4 | e4-guarantee | currency | right, determined | right | CURRENCY |
| supplier_quotes | Q7 | e5 | e5-micro-rounding | retention | right, determined (per-unit to_cents in code) | right | INFERABLE |
| supplier_quotes | Q8 | e5 | e5-new-perishable | retention | right, determined (shelf-life check in code; default equals gold) | right | INFERABLE |
| entity_outbox | Q1 | e2 | e2-recreate | retention | right, determined (separate persistent sequences table) | right | INFERABLE |
| entity_outbox | Q2 | e3 | e3-compaction | retention | right, determined (sequences table survives compaction; default equals gold) | right | INFERABLE |
| entity_outbox | Q3 | e4 | e4-epoch-compaction | retention | right, determined (last_seq in sequences table) | right | INFERABLE |
| entity_outbox | Q4 | e5 | e5-rename-epoch | currency | right, ambiguous | right, determined | CURRENCY |
| late_fees | Q1 | e1 | e1-half-up | retention | not determined (no rounding rule) | right | PASS |
| late_fees | Q2 | e1 | e1-charity | retention | wrong (charity charged) | right | PASS |
| late_fees | Q3 | e2 | e2-foundation | retention | wrong (foundation charged) | right | PASS |
| late_fees | Q4 | e3 | e3-penalty-exempt | retention | wrong (all charged) | wrong: foundation charged, because seed NP-7 says "today charity is the only non-profit segment" | RECORDS-FAIL |
| late_fees | Q5 | e3 | e3-penalty-half-up | retention | right, ambiguous (followed percent_of convention) | right, determined | PASS (weak) |
| late_fees | Q6 | e4 | e4-association-np9 | currency | right, determined | right | CURRENCY |
| late_fees | Q7 | e4 | e4-association-penalty | retention | right, determined (prompt wording + NON_PROFIT_SEGMENTS set used by both fee paths) | right | INFERABLE |

Totals over 16 retention probes: PASS 6 (2 weak), INFERABLE 9, RECORDS-FAIL 1. Currency probes: 3, none discriminate against a no-memory reader (by design).

Pattern: a retention probe discriminated only when the previous code could not show the rule, because the rule concerned a code path or category that did not exist yet (unknown SKU on a new batch path, rounding before any half-cent case, an exemption before any exemption code, a new segment before a segment set). Every probe whose rule the reference implemented as a general mechanism (persistent sequence table, per-unit rounding helper, shelf-life check, non-profit set) was recovered by a code reader.

Second finding: a seed record carrying a time-bound clause ("today charity is the only non-profit segment") goes stale by scenario design at e2, and a reader that treats records as authoritative is then forced to a wrong answer even though the code is right.

**Revision 2 response.** Every probe is kept and the reference code stays
natural. Inferable probes whose rule the previous code shows are labelled
cost. New correctness instances were added on new code paths or categories
(`supplier_quotes` e5-orders-unknown; `entity_outbox` e2-delete-event,
e3-delete-many-events, e4-patch-full, e5-rename-events; `late_fees`
e4-collection-exempt). Two prompts were rescoped (`supplier_quotes`
e2-refetch and e3-discount-rounding), and the stale seed clause was removed
from `late_fees` NP-7.

### Round 2 (2026-09-13)

Same method as round 1: fresh Claude Sonnet readers, one per package episode, condition A answered and saved before condition B records were opened. Round 2 covers only new or changed correctness probes and the corrected late_fees e3 records.

| Package | Probe | Measures | A | B | Verdict |
|---|---|---|---|---|---|
| supplier_quotes | e2-refetch | correctness | right, ambiguous (raised the instance-cache alternative) | right, determined | PASS (weak) |
| supplier_quotes | e3-discount-rounding | correctness | right, ambiguous | right, determined | PASS (weak) |
| supplier_quotes | e5-orders-unknown | correctness | wrong (one supplier call before the unknown SKU) | right | PASS |
| entity_outbox | e2-delete-event | correctness | wrong (no deleted event), ambiguous | right | PASS |
| entity_outbox | e3-delete-many-events | correctness | right, determined (followed the single delete convention) | right | INFERABLE |
| entity_outbox | e4-patch-full | correctness | right, determined (followed the update payload convention) | right | INFERABLE |
| entity_outbox | e5-rename-events | correctness | right, ambiguous | right, determined | PASS (weak) |
| late_fees | e3-penalty-exempt | correctness | wrong (all segments charged) | right (records fix confirmed) | PASS |
| late_fees | e3-penalty-half-up | correctness | right, determined (followed percent_of convention) | right | INFERABLE |
| late_fees | e4-collection-exempt | correctness | wrong (non-profits charged) | right | PASS |

Pattern confirmed: probes pass when the new path forces a choice the existing code does not settle (reject an unknown input before any side effect, classify a new category, apply an exemption to a new fee). Probes fail when the new path can copy a convention the code already shows (emit on delete, full payload on update, the rounding helper).

After round 2, supplier_quotes and late_fees have at least one audited correctness probe in every episode after e1. entity_outbox e3 and e4 have none.

**Revision 3 response.** Every probe is kept. `entity_outbox`
e3-delete-many-events and e4-patch-full and `late_fees` e3-penalty-half-up are
relabelled cost. `entity_outbox` e1 now also states, with its reason, that a
request that raises must leave no events and no writes: "A request that raises
must leave the database exactly as it was, with no events and no other writes,
because the indexer must never see an event for a change that did not happen,
and callers retry a failed request after fixing it." Two new correctness
probes put that rule on paths whose failure behaviour the existing code does
not settle: e3-delete-many-atomic (the e3 prompt now says only that an unknown
ID raises KeyError) and e4-ack-many-atomic on a new `Outbox.ack_many` (the
existing `ack` writes before it checks).

### Round 3

Same method: fresh Claude Sonnet readers, condition A answered and saved before
the condition B records were opened. Packets covered only the two new
`entity_outbox` correctness probes.

| Package | Probe | Measures | A | B | Verdict |
|---|---|---|---|---|---|
| entity_outbox | e3-delete-many-atomic | correctness | right, ambiguous (inferred validate-first from `update`/`delete` raising before mutating) | right, determined (cites the e1 no-partial-writes constraint) | PASS (weak) |
| entity_outbox | e4-ack-many-atomic | correctness | right, ambiguous (inferred from the e3 reference's validate-first `delete_many`) | right, determined | PASS (weak) |

Both probes pass weakly: a no-memory reader reaches the gold answer by analogy
but cannot settle it, and the deciding record settles it. The e4 reader's
analogy came from the e3 reference itself, the long-horizon form of the round-1
pattern (earlier episodes' code becomes local evidence). With these, every
episode after e1 in all three packages has an audited correctness probe.
Recorded as graph Lesson 2f56f4de; the stale seed clause finding is Lesson
8b72b8ed.

## Gold review

Maintainer walk-through with James, 2026-09-13, one package at a time. Review
page: generated from the committed `scenario.json` and `gold.json` files.

### `late_fees` (decided)

1. **Seed wording.** `SOURCE_MAPPING.md` still described the `fees-np7` seed
   as saying charity is today's only non-profit segment, the clause the blind
   audit removed from the seed record. Agreed: the clause is removed from the
   source mapping (a57816e).
2. **NP-9 scope and NP-7's current scope.** The episode 3 task text scopes
   NP-9 to late fees twice ("replaces NP-7 for late fees on invoices due on or
   after day 1000"; "Late fees on invoices due before day 1000 still follow
   NP-7"). Agreed: keep that wording and `fees-np7`'s `current_scope`.
   Adding "NP-9 covers late fees only" was rejected, because it would state
   outright the scope that `e4-collection-exempt` tests.
3. **Facts that bundled a consequence.** `fees-segments`,
   `fees-returned-payment`, `fees-association` and `fees-collection` each
   added an exemption their episode's task text does not state. Agreed: each
   claim is trimmed to what its episode states. The exemptions stay expected
   through `fees-np7`'s current scope and `fees-np9`, so graded notes are not
   credited twice for the same knowledge; hidden tests and probes are
   unchanged.

Supersession coverage after the review: `late_fees` keeps the partial
supersession NP-7 to NP-9 (task probe `e3-np9`, currency probe
`e4-association-np9` with the `stale-np7` negative, correctness probe
`e4-collection-exempt`, and the episode-3 forbidden claims);
`supplier_quotes` keeps clause 7 to the 600-second guarantee and
`entity_outbox` keeps lifetime numbering to epochs, both full supersessions
with a retired test.

### `supplier_quotes` and `entity_outbox` (decided)

4. **Claims that named later episodes or restated earlier rules.** The
   late-fees rule was applied to both packages: each gold claim states only
   what its own episode's task text states. Trimmed in `supplier_quotes`:
   `quote-unit-rounding` (basket, discount and micro-unit clauses),
   `quote-billed-calls` (the list of later paths), `quote-basket` (the
   within-one-call scoping the refetch probe tests), `quote-price-guarantee`
   ("later warm"), `quote-volume-discount`, `quote-orders` and
   `quote-new-skus` (consequences of earlier rules). Trimmed in
   `entity_outbox`: `outbox-seq-contiguous` (compaction and epochs),
   `outbox-seq-lifetime` (now episode 1's own wording, so re-creation stays
   the inference `e2-recreate` measures), `outbox-failed-request-no-effect`
   (the later multi-item paths), `outbox-compaction`, `outbox-delete-many`,
   `outbox-patch`, `outbox-ack-many` and `outbox-rename` (consequences of
   episode-1 and episode-4 rules). Every consequence stays expected through the
   earlier fact that states it. Hidden tests, probes, blind audit results,
   forbidden claims and both full supersessions are unchanged. Long-horizon
   validate 113/113; study suite green.
