"""Long-horizon scenario tables, kept apart from the sealed intent overlay.

Resolution targets and seed associations for these packages live here so that
`intent.OVERLAY`, and every historical design identity derived from it, never
changes when a long-horizon package is added or revised.
"""

SCENARIOS = ("supplier_quotes", "entity_outbox", "late_fees")
PROBE_RULES_VERSION = 1
RESOLUTION_TARGETS = {
    "supplier_quotes": [{"file": "quotes.py", "name": "QuoteService.quote"}],
    "entity_outbox": [{"file": "outbox.py", "name": "Outbox.emit"}],
    "late_fees": [{"file": "fees.py", "name": "FeePolicy.late_fee"}],
}
SEED_ASSOCIATIONS = {
    "supplier_quotes": [],
    "entity_outbox": [],
    "late_fees": [{"fact": fact, "file": "fees.py", "name": "FeePolicy.late_fee"}
                  for fact in ("fees-np7", "fees-cents")],
}
