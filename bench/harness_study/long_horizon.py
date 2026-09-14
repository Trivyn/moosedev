"""Long-horizon scenario tables, kept apart from the sealed intent overlay.

Resolution targets and seed associations for these packages live here so that
`intent.OVERLAY`, and every historical design identity derived from it, never
changes when a long-horizon package is added or revised.
"""

SCENARIOS = ("supplier_quotes", "entity_outbox", "late_fees")
# Exploratory probe packages: runnable only through the field check, never part
# of the long-horizon campaign's scenario set.
EXPLORATORY = ("late_fees_crowded",)
PROBE_RULES_VERSION = 1
RESOLUTION_TARGETS = {
    "supplier_quotes": [{"file": "quotes.py", "name": "QuoteService.quote"}],
    "entity_outbox": [{"file": "outbox.py", "name": "Outbox.emit"}],
    "late_fees": [{"file": "fees.py", "name": "FeePolicy.late_fee"}],
    "late_fees_crowded": [{"file": "fees.py", "name": "FeePolicy.late_fee"}, {"file": "invoices.py", "name": "Invoice.__init__"}, {"file": "accounts.py", "name": "Account.__init__"}, {"file": "statements.py", "name": "statement_line"}],
}
SEED_ASSOCIATIONS = {
    "supplier_quotes": [],
    "entity_outbox": [],
    "late_fees": [{"fact": fact, "file": "fees.py", "name": "FeePolicy.late_fee"}
                  for fact in ("fees-np7", "fees-cents")],
    "late_fees_crowded": [
        {"fact": "fees-cents", "file": "fees.py", "name": "FeePolicy.late_fee"},
        {"fact": "billing-fee-on-demand", "file": "fees.py", "name": "FeePolicy.late_fee"},
        {"fact": "billing-feepolicy-stateless", "file": "fees.py", "name": "FeePolicy.late_fee"},
        {"fact": "billing-fee-rules-in-policy", "file": "fees.py", "name": "FeePolicy.late_fee"},
        {"fact": "billing-late-fee-per-invoice", "file": "fees.py", "name": "FeePolicy.late_fee"},
        {"fact": "invoicing-day-numbers", "file": "invoices.py", "name": "Invoice.__init__"},
        {"fact": "invoicing-amount-cents", "file": "invoices.py", "name": "Invoice.__init__"},
        {"fact": "invoicing-due-day-fixed", "file": "invoices.py", "name": "Invoice.__init__"},
        {"fact": "accounts-one-segment", "file": "accounts.py", "name": "Account.__init__"},
        {"fact": "accounts-opaque-ids", "file": "accounts.py", "name": "Account.__init__"},
        {"fact": "accounts-stable-codes", "file": "accounts.py", "name": "Account.__init__"},
        {"fact": "statements-line-shows-fee", "file": "statements.py", "name": "statement_line"},
        {"fact": "statements-statement-day", "file": "statements.py", "name": "statement_line"},
        {"fact": "statements-cp3", "file": "statements.py", "name": "statement_line"},
    ],
}
