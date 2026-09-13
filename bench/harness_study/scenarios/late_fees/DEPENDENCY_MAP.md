# Dependency map: late fees

Status: draft for maintainer gold review. Written before the hidden tests.

This package is **inherited**: two reviewed seed records start in both arms,
rendered identically into `PROJECT_NOTES.md` and the graph. Both seeds are
associated with `fees.py` `FeePolicy.late_fee`, the resolution target, so the
harness delivers them in the `fees.py` dossier. Seed recall and newly captured
recall are reported separately.

- Seed 0, `fees-np7` (`#/initial_facts/0`): "Regulation NP-7: no late fee or penalty fee of any kind may be charged to an account in a registered non-profit segment. A reduced late fee for charities was proposed and rejected because NP-7 allows no fee at all. Today charity is the only non-profit segment."
- Seed 1, `fees-cents` (`#/initial_facts/1`): "Every fee is an integer number of cents. A percentage fee is rounded half up to the cent, never with banker's rounding."

Columns per probe: the deciding sentence, what the previous reference code
reveals, the default a reader with only that code and the current prompt would
pick, why the probe is well-posed, channels, expected discrimination, the gold
record that decides it, and how the harness delivers it. Harness delivery
routes: **dossier** (records linked to `FeePolicy.late_fee`; the `fees.py`
dossier), **topic** (top 12 records matching the objective; this project stays
under a dozen), **search**. Native OpenCode reads `PROJECT_NOTES.md` in full.

## e1: the late-fee formula

### e1-charity: `LateFeeTests.test_charity_pays_no_late_fee` (retention)
- Deciding sentence: seed 0.
- Previous reference code: `project/fees.py` returns 0 for everyone; nothing mentions charities.
- Code-plus-prompt default: apply the formula to every account.
- Well-posed: the e1 prompt gives the formula and says nothing about segments.
- Discrimination: **high** (only the seed carries it).
- Gold record: `fees-np7`. Harness delivery: dossier (seed association), topic.
- Negative: `charity_charged`.

### e1-half-up: `LateFeeTests.test_percentage_rounds_half_up` (retention)
- Deciding sentence: seed 1.
- Previous reference code: none; the starter has no arithmetic.
- Code-plus-prompt default: Python `round`, which rounds 100.5 to 100.
- Well-posed: the prompt says "5% of amount_cents" without a rounding rule.
- Discrimination: **medium** (Decimal half-up is also a common choice).
- Gold record: `fees-cents`. Harness delivery: dossier, topic.
- Negative: `bankers_rounding`.

## e2: new segments

### e2-foundation: `SegmentTests.test_foundation_exempt_cooperative_charged` (retention)
- Deciding sentence: seed 0 ("any account in a registered non-profit segment").
- Previous reference code: `reference/e1/fees.py` exempts `segment == "charity"` only.
- Code-plus-prompt default: keep the charity check; the new foundation segment pays the fee.
- Well-posed: e2 classifies foundation as a registered non-profit and cooperative as for-profit, and says nothing about fees.
- Discrimination: **high**.
- Gold record: `fees-np7`. Harness delivery: dossier, topic.
- Negative: `foundation_charged`.

## e3: NP-9 partially replaces NP-7; returned-payment fee

### e3-penalty-exempt: `ReturnedPaymentTests.test_nonprofits_pay_no_returned_payment_fee` (retention)
- Deciding sentence: seed 0 ("no late fee or penalty fee of any kind"). NP-9 replaces NP-7 only "for late fees on invoices due on or after day 1000".
- Previous reference code: `reference/e2/fees.py` exempts non-profits inside `late_fee` only; the new method has no precedent.
- Code-plus-prompt default: charge the returned-payment fee to every account.
- Well-posed: the e3 prompt defines the fee amount and says nothing about segments; NP-9 is explicitly limited to late fees.
- Discrimination: **high**.
- Gold record: `fees-np7` (current in its narrowed scope). Harness delivery: dossier, topic.
- Negative: `penalty_charged_to_nonprofits`.

### e3-penalty-half-up: `ReturnedPaymentTests.test_returned_payment_fee_rounds_half_up` (retention)
- Deciding sentence: seed 1.
- Previous reference code: `reference/e2/fees.py` rounds the late fee half up with `Decimal`; a new method may not reuse it.
- Code-plus-prompt default: `round(amount_cents * 0.025)`, which rounds 252.5 to 252.
- Well-posed: the prompt says "2.5% of amount_cents" without a rounding rule.
- Discrimination: **medium** (low if the helper is reused).
- Gold record: `fees-cents`. Harness delivery: dossier, topic.
- Negative: `penalty_bankers_rounding`.

## e4: a third non-profit segment and a forecast

### e4-association-np9: `AssociationTests.test_association_late_fee_follows_np9` (currency)
- Deciding sentence: `#/episodes/2/prompt`, "Regulation NP-9 replaces NP-7 for late fees on invoices due on or after day 1000: a late invoice of a registered non-profit account due on or after day 1000 has a flat late fee of 200 cents."
- Previous reference code: `reference/e3/fees.py` applies NP-9 through the non-profit set.
- Code-plus-prompt default: add the association to the non-profit set, which passes. A reader holding the **stale** seed record (NP-7 unconditional, never revised when NP-9 arrived) exempts the association entirely and charges 0 for an invoice due on day 1200.
- Well-posed: e4 classifies association as a registered non-profit; both regulations are current in their scopes.
- Discrimination: **low** against a code reader; aimed at a seeded record that was not kept current.
- Gold record: `fees-np9` (current), with `fees-np7` current only in its narrowed scope. Harness delivery: dossier, topic.
- Negatives: `stale_np7`, `association_for_profit`.

### e4-association-penalty: `AssociationTests.test_association_pays_no_returned_payment_fee` (retention)
- Deciding sentence: seed 0.
- Previous reference code: `reference/e3/fees.py` exempts the non-profit set from the returned-payment fee, so adding the association to the set passes.
- Code-plus-prompt default: an association added as a plain segment (not in the set) pays the fee.
- Well-posed: NP-9 never covered penalty fees.
- Discrimination: **low** against the reference code.
- Gold record: `fees-np7`. Harness delivery: dossier, topic.
- Negative: `association_for_profit`.
