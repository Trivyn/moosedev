# Source mapping: late fees, crowded probe

Status: fully synthetic exploratory probe package, derived from `late_fees`;
maintainer gold review pending. It is never scored, never pooled and not part
of the long-horizon campaign's scenario set (`long_horizon.EXPLORATORY`). It
exists to test whether the harness delivers a deciding record that is neither
linked to code the model reads nor among the top 12 records for the task
(plan-time recall; review packet decision 3). Regulations NP-7, CP-3, DS-2 and
PC-1, the segments, the fee amounts and every distractor are fictional.

## Seeds

The two `late_fees` seeds are kept, rendered identically as
`PROJECT_NOTES.md` sections and graph records:

- `fees-np7` (seed 7): description unchanged. Its title is neutralised to
  "Regulation NP-7 compliance for account billing", because the `late_fees`
  title ("no late or penalty fees for registered non-profits") states the rule
  and the inventory and dossiers push titles. It has no code association and
  no relations.
- `fees-cents` (seed 5): unchanged, and still associated with `fees.py`
  `FeePolicy.late_fee`, so its title reaches the harness in that file's dossier
  (the positive control for probe e1-half-up).

## Distractors

52 further seed records describe a plausible billing project across six
components (Billing, Invoicing, Accounts, Statements, Payments, Engineering).
Fourteen are associated with code (`FeePolicy.late_fee`, `Invoice.__init__`,
`Account.__init__`, `statement_line`); a few carry `isMotivatedBy` or
`learnedFrom` relations to other distractors. Every distractor:

- is timeless and consistent with the starter code and all four episode
  prompts;
- never mentions rounding;
- never says how non-profits or charities are charged;
- never lists the segment set, returned payments, NP-9 or collection
  penalties;
- never restates a task probe's rule (the grace period or the minimum fee).

Only distractors may be revised to keep NP-7 out of today's push; the task
text, NP-7's description and `fees-cents` never change. Distractors are frozen
before any plan-time rank is computed.

## Crowding gate and audit

Gate round 0 on build 1d31f454 (42 distractors): NP-7 ranked 17 for the full
episode prompt but 9 for the bare task sentence, so its claim reached topic
evidence for the bare sentence. Revision round 1 added ten
late-fee-vocabulary distractors, all unlinked, appended after the existing
seeds; nothing else changed.

Gate round 1 (V1, build 1d31f454, frozen distractors,
`seed_graph_sha256` 6ca94a9158cdfc075645fa7ae912c70f99497d777a94214c9a1f4e818501b8ed):
for both topics and every file set (none, `fees.py`, all project files), NP-7
is in the inventory (neutral title only) and absent from topic evidence, the
link walk, every dossier and every policy reason, and its claim appears
nowhere. Search rank: 25 for the full episode prompt and 16 for the bare task
sentence (cross-checked against the daemon's own evidence). Largest pushed
context plus dossiers: 18,417 bytes. Evidence:
`target/harness-crowded-probe-v1/gate/v1-round1/gate.json`.

Gate V2 (same build, the three condition-A plans; Definition A topic = plan
summary + newline + plan files): **fail, 0 of 3 plans reach NP-7** (search
ranks 31, 36 and 28; not in any plan's top 12 or walk; claim absent). The
plans repeat the task's vocabulary and never mention segments, charities or
non-profits. Diagnostics, not part of Definition A: adding identifiers from
the plan files and the modules they import (Definition B) also misses (ranks
26, 36, 27); adding plan 1's source string literals, which name the charity
segment, reaches rank 11 with the claim delivered. Evidence:
`target/harness-crowded-probe-v1/gate/v2/` and
`target/harness-crowded-probe-v1/gate/v2-definition-b-diagnostic/`. The final
gate re-run on this package (V1 and V2) is
`target/harness-crowded-probe-v1/gate/final/gate.json`, which records the
package hash.

Blind-audit round 1 (2026-09-13, fresh Claude Sonnet readers, one per packet):

- **A** (e1 prompt and starter project; three readers, each writing a plan
  first): e1-charity answered 500, determined, by all three (wrong); e1-half-up
  ambiguous (100 or 101); grace and minimum right.
- **Push-only** (A plus today's harness push for the full episode prompt and
  all project files): e1-charity 500, determined, so the push does not decide
  it, as V1 requires; e1-half-up 101, determined, citing the `fees.py` dossier
  title "Fees are integer cents rounded half up" (positive control works);
  grace and minimum right.
- **B** (A plus every seed rendered as `PROJECT_NOTES.md`): all four right and
  determined; e1-charity 0, citing `fees-np7`. The distractors do not mislead.
- **Plan-push**: not built, because no plan reached NP-7 in V2.

Packets, keys and answers: `/Users/jcadam/.claude/jobs/7f3a44c1/tmp/probe/audit/`.

## What each episode introduces

Episodes, references, hidden tests and negatives are copied from `late_fees`
with renumbered seed pointers (seed 0 is now 7, seed 1 is now 5). Runs use
episode 1 only; e2 to e4 are not run and not re-audited for distractors.
