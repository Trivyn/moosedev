# Long-horizon scenario review

**Status: draft for maintainer gold review. No approval file exists, no model
has run on these packages, and no campaign may start until the decisions below
are settled and the gold is approved.**

MOOSEDev's value is judged on long-horizon, multi-turn work. These three
packages carry code and durable knowledge across four or five episodes. Their
later episodes are decided by an earlier episode's *reason*, which neither the
current prompt nor the previous reference code settles. All three are fully
synthetic; see each package's `SOURCE_MAPPING.md`.

| Package | Track | Episodes | Probes (task / retained / retention / currency) | Negatives | Validation cases |
| --- | --- | --- | --- | --- | --- |
| `supplier_quotes` | accumulation | 5 | 12 / 4 / 7 / 1 | 8 | 37 |
| `entity_outbox` | accumulation | 5 | 10 / 4 / 3 / 1 | 4 | 29 |
| `late_fees` | inherited (2 seeds) | 4 | 6 / 3 / 6 / 1 | 7 | 31 |

Per episode, each package's `DEPENDENCY_MAP.md` gives every later probe's
deciding sentence (JSON pointer and quote), what the previous reference code
reveals, the default a reader with only that code and the current prompt would
pick, why the probe is well-posed, the legitimate channels, the expected
discrimination, the gold record that decides it and how the harness delivers
that record. Full prompts are in `scenario.json`; claims are in `gold.json`.

## Design rules

1. **Clear with history, open without it.** For each later probe, a reader of
   all earlier prompts gives one answer. A reader of only the current prompt
   and the previous reference code sees two plausible answers or defaults to
   the wrong one.
2. **Extend, don't repeat.** A later task creates a new instance of an earlier
   rule (a new code path, deletion or compaction, a representation change).
   Only the earlier reason decides it.
3. **No trick questions.** Later prompts never contradict an earlier decision.
   Early prompts state a rule's scope ("applies to every code path that
   contacts the supplier"), never "remember this". Wording is identical in
   both arms.
4. **Objective, independent probes.** Public API only, injected fakes and
   clocks, no private names; each hidden test method stands alone.
5. **Honest about inference.** Reference code carries no comments explaining
   reasons. Agent-written comments, visible tests and notes are legitimate
   retention channels, recorded rather than penalised.

## Probe taxonomy

Every hidden test method is declared in `scenario.json` as one probe:

- **task**: decided by this episode's prompt.
- **retained**: decided earlier and visible in the previous reference code
  (regression).
- **retention**: decided by an earlier reason, or a seed record, that the
  previous reference code does not decide.
- **currency**: behaviour under a superseding rule, where stale knowledge
  predicts a different result.

Every episode has a task probe. Every episode after e1 has a retention or
currency probe. Every retention and currency probe has at least one negative
fixture that fails exactly the probes it declares.

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
- **Probe outcomes** come from per-test hidden results; a hidden test's
  retired tests (listed in the next episode's `retired_tests`) are excluded
  from later regression checks.

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

This separates a capture failure from a retrieval failure. It also measures
whether the harness acted on supplied graph answers instead of combing source.

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

1. **Failure policy.** Today a hidden-check failure stops the run and hides
   every later episode. *Recommended:* continue after a hidden-check failure
   and stop only when an episode fails to complete, so one retention miss does
   not hide later episodes. Per-test independence makes later probes readable.
2. **Primary outcome.** *Recommended:* an episode passes when it completes, the
   probes it introduces pass, and nothing that passed in the previous attempted
   episode regresses. Horizon reached is the run of leading passing episodes.
   Retention and currency pass rates are the explanatory breakdown, reported
   conditionally (episode completed and its task probes passed) and
   unconditionally.
3. **Build.** *Recommended:* a new frozen build carrying the replan
   continuation, the attested own final capture and graph-first search, not
   v3's `1cd75b0b`.
4. **Native no-notes floor arm.** Open. It adds 6 cells and a new guidance key,
   and shows how much a well-kept notes file contributes. The alternative is
   an offline calibration on single later episodes started from the reference.
5. **Block order.** Open. Qwen first gives horizon evidence sooner, because
   long horizons need early episodes to pass; Gemma first keeps the
   model-size question first, as in the symbolic campaign.

**Fixed, not a decision:** before any block, the first harness cell of the
block order runs alone into a discarded store and its journal is read. An
unexplained terminal path or a harness-class cause (runner error, controller
invariant, daemon rejection, infrastructure, unknown) stops the campaign
before any hours are spent.

Scale: 12 cells over two models, two arms and three packages; 14 episodes per
model and arm, 56 in all; at the 1200-second episode budget, at most about 19
hours before indexing and model loads.

## Expected discrimination

Ratings are against a reader with the previous reference code and the current
prompt. Low-rated probes still matter when an arm's own earlier code differed
from the reference, or when stored knowledge went stale.

| Package | Probe | Kind | Rating |
| --- | --- | --- | --- |
| supplier_quotes | e2-refetch | retention | high |
| supplier_quotes | e2-basket-rounding | retention | medium |
| supplier_quotes | e2-basket-unknown | retention | medium |
| supplier_quotes | e3-discount-rounding | retention | medium |
| supplier_quotes | e4-warm-unknown | retention | medium |
| supplier_quotes | e4-guarantee | currency | low |
| supplier_quotes | e5-micro-rounding | retention | high |
| supplier_quotes | e5-new-perishable | retention | low |
| entity_outbox | e2-recreate | retention | high |
| entity_outbox | e3-compaction | retention | medium |
| entity_outbox | e4-epoch-compaction | retention | medium |
| entity_outbox | e5-rename-epoch | currency | low |
| late_fees | e1-charity | retention | high |
| late_fees | e1-half-up | retention | medium |
| late_fees | e2-foundation | retention | high |
| late_fees | e3-penalty-exempt | retention | high |
| late_fees | e3-penalty-half-up | retention | medium |
| late_fees | e4-association-np9 | currency | low |
| late_fees | e4-association-penalty | retention | low |

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

To be filled by the parent session before gold approval. Two conditions per
retention and currency probe, each answered in writing by a fresh reader:

1. Only `reference/e(N-1)` and prompt eN. A probe this reader answers
   correctly is revised or dropped.
2. The same, plus only the deciding gold records. A probe this reader answers
   incorrectly is ill-posed: the graph answer does not suffice.
