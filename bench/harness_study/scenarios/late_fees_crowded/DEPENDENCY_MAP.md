# Dependency map: late fees, crowded probe

Status: exploratory probe package; draft for maintainer gold review; crowding
gate and blind audit pending. Runs use episode 1 only. Episodes e2 to e4 are
copied from `late_fees` with renumbered seed pointers and are not run and not
re-audited for distractors.

Seeds: 44 (the two `late_fees` seeds plus 42 distractors; see
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
association). Whether topic evidence or the link walk carries it is measured by
the crowding gate: pending. The inventory lists only its neutral title.

## e1: the late-fee formula

### e1-charity: `LateFeeTests.test_charity_pays_no_late_fee` (retention, measures correctness)
- Deciding sentence: seed 7.
- Starter code: `project/fees.py` returns 0 for everyone; nothing mentions charities.
- Code-plus-prompt default: apply the formula to every account.
- Discrimination: **high** (only the seed carries it).
- Gold record: `fees-np7`. Harness delivery: search; plan evidence after the plan-time recall build. Gate: pending.
- Negative: `charity_charged`. Audit: pending.

### e1-half-up: `LateFeeTests.test_percentage_rounds_half_up` (retention, measures correctness)
- Deciding sentence: seed 5.
- Code-plus-prompt default: Python `round`, which rounds 100.5 to 100.
- Gold record: `fees-cents`. Harness delivery: dossier title in both builds (positive control); topic evidence per the gate.
- Negative: `bankers_rounding`. Audit: pending.

### e1-grace and e1-minimum (task)
- Decided by the e1 prompt.

## e2 to e4

Not run and not re-audited. Probes, deciding pointers and negatives are those
of `late_fees` with seed 0 renumbered to 7 and seed 1 to 5.
