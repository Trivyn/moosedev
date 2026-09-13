# Source mapping: supplier quotes

Status: fully synthetic; maintainer gold review pending. No private or public
MOOSEDev source is adapted. The supplier, catalog, contract clauses and
prices are fictional and exist only to create decisions whose reasons matter
in later episodes.

## What each episode introduces

- e1 states three rules with reasons: per-unit rounding (the supplier invoices
  per unit; rounding totals rejected), billed supplier calls (unknown SKUs
  rejected before any contact, on every path), and binding quotes under clause 7
  (a price cache rejected).
- e2 adds batch quoting under a rate limit. It does not restate any e1 rule.
- e3 replaces clause 7 with a 600-second guarantee and adds a volume discount
  without saying where to round.
- e4 excludes perishables by shelf life and adds cache warming, a new path that
  contacts the supplier.
- e5 changes the price representation to micro-units and adds catalog SKUs.

`DEPENDENCY_MAP.md` lists, for every later probe, the earlier sentence that
decides it. Reference code carries no comments explaining reasons.
