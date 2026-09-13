# Source mapping: late fees

Status: fully synthetic; maintainer gold review pending. No private or public
MOOSEDev source is adapted. Regulations NP-7 and NP-9, the segments and the
fee amounts are fictional.

## Seeds

Two reviewed seed records start in both arms, rendered identically as
`PROJECT_NOTES.md` sections and graph records, and associated with
`fees.py` `FeePolicy.late_fee`:

- `fees-np7`: no late or penalty fee of any kind for registered non-profit
  segments; a reduced charity fee was rejected.
- `fees-cents`: fees are integer cents; percentage fees round half up.

## What each episode introduces

- e1 gives the late-fee formula without mentioning segments or rounding.
- e2 classifies two new segments without mentioning fees.
- e3 partially replaces NP-7 with NP-9 for late fees on invoices due on or
  after day 1000, and adds a penalty fee without mentioning segments or
  rounding.
- e4 classifies a third non-profit segment and adds a forecast.

`DEPENDENCY_MAP.md` lists, for every later probe, the earlier sentence or seed
that decides it. Reference code carries no comments explaining reasons.
