# Dependency map: late fees, crowded probe

Status: exploratory probe package; draft for maintainer gold review; crowding
gate V1 passed, V2 failed (no blind plan reaches NP-7); blind audit round 1 done. Runs use episode 1 only. Episodes e2 to e4 are
copied from `late_fees` with renumbered seed pointers and are not run and not
re-audited for distractors.

Seeds: 54 (the two `late_fees` seeds plus 52 distractors; see
`SOURCE_MAPPING.md`).

- Seed 7, `fees-np7` (`#/initial_facts/7`), title "Regulation NP-7 compliance
  for account billing": "No late fee or penalty fee of any kind may be charged
  to an account in a registered non-profit segment. A reduced late fee for
  charities was proposed and rejected because NP-7 allows no fee at all." No
  code association, no relations.
- Seed 5, `fees-cents` (`#/initial_facts/5`): "Every fee is an integer number
  of cents. A percentage fee is rounded half up to the cent, never with
  banker's rounding." Associated with `fees.py` `FeePolicy.late_fee`.

Harness delivery routes for the deciding record `fees-np7`: **search** (both
builds), **plan evidence** (the plan-time recall build only), and native
OpenCode reading `PROJECT_NOTES.md`. It is not delivered by a dossier (no code
association). Crowding gate round 1 (build 1d31f454): it is absent from topic
evidence and the link walk for the full episode prompt (search rank 25) and
for the bare task sentence (rank 16), and absent from every dossier and policy
reason. The inventory lists only its neutral title.

## e1: the late-fee formula

### e1-charity: `LateFeeTests.test_charity_pays_no_late_fee` (retention, measures correctness)
- Deciding sentence: seed 7.
- Starter code: `project/fees.py` returns 0 for everyone; nothing mentions charities.
- Code-plus-prompt default: apply the formula to every account.
- Discrimination: **high** (only the seed carries it).
- Gold record: `fees-np7`. Harness delivery: search; plan evidence after the plan-time recall build. Gate round 1: not pushed (ranks 25 and 16). Gate V2: no blind plan reaches it (ranks 31, 36, 28).
- Negative: `charity_charged`.
- Audit round 1: A wrong (500, determined, all three readers); push-only FAIL to decide (500, determined), as V1 requires; B PASS (0, determined, citing `fees-np7`); plan-push not built.

### e1-half-up: `LateFeeTests.test_percentage_rounds_half_up` (retention, measures correctness)
- Deciding sentence: seed 5.
- Code-plus-prompt default: Python `round`, which rounds 100.5 to 100.
- Gold record: `fees-cents`. Harness delivery: dossier title in both builds (positive control); not in topic evidence for either gate topic.
- Negative: `bankers_rounding`.
- Audit round 1: A ambiguous (100 or 101, all three readers); push-only PASS (101, determined, citing the `fees.py` dossier title "Fees are integer cents rounded half up"), so the dossier title delivers it; B PASS (101, determined).

### e1-grace and e1-minimum (task)
- Decided by the e1 prompt.
- Audit round 1: right in A, push-only and B.

## e2 to e4

Not run and not re-audited. Probes, deciding pointers and negatives are those
of `late_fees` with seed 0 renumbered to 7 and seed 1 to 5.
