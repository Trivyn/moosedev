# Dependency map: late fees

Status: draft for maintainer gold review, revision 2 after blind reader audit
round 1. Written before the hidden tests.

Each probe records its round-1 verdict. Condition A gave a reader only the
episode prompt and the previous reference code; condition B added only the
deciding records current at that episode. Every retention probe is labelled
with what it measures. **Correctness**: the previous code cannot show the rule.
**Cost**: the rule is visible in the previous code, so a graph-first agent's
value is not having to read and infer it from source; it is scored on reads,
searches, requests and tokens before the first correct edit.

This package is **inherited**: two reviewed seed records start in both arms,
rendered identically into `PROJECT_NOTES.md` and the graph. Both seeds are
associated with `fees.py` `FeePolicy.late_fee`, the resolution target, so the
harness delivers them in the `fees.py` dossier. Seed recall and newly captured
recall are reported separately.

- Seed 0, `fees-np7` (`#/initial_facts/0`): "No late fee or penalty fee of any kind may be charged to an account in a registered non-profit segment. A reduced late fee for charities was proposed and rejected because NP-7 allows no fee at all."
- Seed 1, `fees-cents` (`#/initial_facts/1`): "Every fee is an integer number of cents. A percentage fee is rounded half up to the cent, never with banker's rounding."

Revision 2 removes the seed's time-bound clause "Today charity is the only
non-profit segment". That fact lives in the code, went stale by design at e2,
and forced a reader who trusts records to charge foundations at e3.

Harness delivery routes: **dossier** (records linked to `FeePolicy.late_fee`;
the `fees.py` dossier), **topic** (top 12 records matching the objective; this
project stays under a dozen), **search**. Native OpenCode reads
`PROJECT_NOTES.md` in full.

## e1: the late-fee formula

### e1-charity: `LateFeeTests.test_charity_pays_no_late_fee` (retention, measures correctness)
- Round 1: PASS (condition A charged the charity).
- Deciding sentence: seed 0; the charity segment is displayed as "Registered charity" in `project/accounts.py`, and the seed names charities as covered.
- Previous reference code: `project/fees.py` returns 0 for everyone; nothing mentions charities.
- Code-plus-prompt default: apply the formula to every account.
- Discrimination: **high** (only the seed carries it).
- Gold record: `fees-np7`. Harness delivery: dossier (seed association), topic.
- Negative: `charity_charged`.

### e1-half-up: `LateFeeTests.test_percentage_rounds_half_up` (retention, measures correctness)
- Round 1: PASS (condition A could not determine the rounding).
- Deciding sentence: seed 1.
- Previous reference code: none; the starter has no arithmetic.
- Code-plus-prompt default: Python `round`, which rounds 100.5 to 100.
- Discrimination: **medium**.
- Gold record: `fees-cents`. Harness delivery: dossier, topic.
- Negative: `bankers_rounding`.

## e2: new segments

### e2-foundation: `SegmentTests.test_foundation_exempt_cooperative_charged` (retention, measures correctness)
- Round 1: PASS (condition A charged the foundation).
- Deciding sentence: seed 0 ("any account in a registered non-profit segment"), with the e2 prompt classifying foundation as a registered non-profit.
- Previous reference code: `reference/e1/fees.py` exempts `segment == "charity"` only.
- Code-plus-prompt default: keep the charity check; the new foundation segment pays the fee.
- Discrimination: **high**.
- Gold record: `fees-np7`. Harness delivery: dossier, topic.
- Negative: `foundation_charged`.

## e3: NP-9 partially replaces NP-7; returned-payment fee

### e3-penalty-exempt: `ReturnedPaymentTests.test_nonprofits_pay_no_returned_payment_fee` (retention, measures correctness)
- Round 1: RECORDS-FAIL. Condition B charged the foundation because the seed said "today charity is the only non-profit segment". Revision 2 removes that clause; the probe is re-audited in round 2.
- Deciding records, both current at e3: `fees-np7` (seed 0: "no late fee or penalty fee of any kind" for registered non-profit segments) and `fees-segments` (e2: foundation is a registered non-profit, cooperative is for-profit). NP-9 replaces NP-7 only "for late fees on invoices due on or after day 1000".
- Previous reference code: `reference/e2/fees.py` exempts non-profits inside `late_fee` only; the new method has no precedent.
- Code-plus-prompt default: charge the returned-payment fee to every account.
- Discrimination: **high**.
- Gold records: `fees-np7`, `fees-segments`. Harness delivery: dossier (`fees-np7`), topic (both).
- Negative: `penalty_charged_to_nonprofits`.

### e3-penalty-half-up: `ReturnedPaymentTests.test_returned_payment_fee_rounds_half_up` (retention, measures correctness)
- Round 1: PASS, weak (condition A right but ambiguous; it followed the `percent_of` convention).
- Deciding sentence: seed 1.
- Previous reference code: `reference/e2/fees.py` rounds the late fee half up with `Decimal`; a new method may not reuse it.
- Code-plus-prompt default: `round(amount_cents * 0.025)`, which rounds 252.5 to 252.
- Discrimination: **medium** (low if the helper is reused).
- Gold record: `fees-cents`. Harness delivery: dossier, topic.
- Negative: `penalty_bankers_rounding`.

## e4: a third non-profit segment, a forecast and a collection penalty

### e4-collection-exempt: `CollectionTests.test_nonprofits_pay_no_collection_penalty` (retention, measures correctness, new)
- Round 1: not audited (new in revision 2).
- Deciding records, current at e4: `fees-np7` (current for every penalty fee and for late fees on invoices due before day 1000), `fees-np9` (e3: replaces NP-7 only for late fees on invoices due on or after day 1000), and `fees-segments`.
- Previous reference code: `reference/e3/fees.py` has two precedents. `late_fee` charges a registered non-profit a flat 200 cents on invoices due on or after day 1000; `returned_payment_fee` exempts non-profits. A collection penalty is tied to an invoice's due day, like the late fee.
- Code-plus-prompt default: copy the late fee's structure, charging a non-profit 200 cents on an invoice due on or after day 1000.
- Well-posed: the e4 prompt calls it a penalty; NP-9 covers late fees only, so NP-7 still forbids every penalty fee for non-profits.
- Discrimination: **medium to high**.
- Gold records: `fees-np7`, `fees-np9`. Harness delivery: dossier, topic.
- Negative: `collection_like_late_fee`.

### e4-association-penalty: `AssociationTests.test_association_pays_no_returned_payment_fee` (retention, measures cost)
- Round 1: INFERABLE (the prompt classifies association as a registered non-profit, and `NON_PROFIT_SEGMENTS` is used by both fee paths).
- Deciding sentence: seed 0.
- Cost measured: taking the exemption from `fees-np7` instead of reading `fees.py` to see which paths use the non-profit set.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `fees-np7`. Harness delivery: dossier, topic.
- Negative: `association_for_profit`.

### e4-association-np9: `AssociationTests.test_association_late_fee_follows_np9` (currency)
- Round 1: CURRENCY. A no-memory reader is right by design; condition A does not apply to currency probes.
- Deciding sentence: `#/episodes/2/prompt`, "Regulation NP-9 replaces NP-7 for late fees on invoices due on or after day 1000: a late invoice of a registered non-profit account due on or after day 1000 has a flat late fee of 200 cents."
- A reader holding the stale, unconditional NP-7 exempts the association entirely.
- Gold records: `fees-np9` (current), with `fees-np7` current only in its narrowed scope. Harness delivery: dossier, topic.
- Negatives: `stale_np7`, `association_for_profit`.
