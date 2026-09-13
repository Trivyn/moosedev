import os
import sys
import unittest
sys.path.insert(0, os.getcwd())

from accounts import Account
from fees import FeePolicy
from invoices import Invoice
from statements import statement_line


def late(segment, amount_cents, due_day, days_late):
    return FeePolicy().late_fee(Account("acct", segment), Invoice("INV-1", amount_cents, due_day), due_day + days_late)


class Np9Tests(unittest.TestCase):
    def test_charity_flat_late_fee_from_day_1000(self):
        self.assertEqual(late("charity", 10000, 1000, 15), 200)
        self.assertEqual(late("charity", 10000, 1000, 14), 0)
        self.assertEqual(late("charity", 10000, 999, 15), 0)
        self.assertEqual(late("retail", 10000, 1000, 15), 500)


class ForecastTests(unittest.TestCase):
    def test_forecast_matches_late_fee_by_day(self):
        policy = FeePolicy()
        invoice = Invoice("INV-1", 10000, 100)
        self.assertEqual(policy.fee_forecast(Account("r", "retail"), invoice, [110, 115, 200]), [0, 500, 500])
        self.assertEqual(policy.fee_forecast(Account("r", "retail"), invoice, []), [])


class AssociationTests(unittest.TestCase):
    def test_association_late_fee_follows_np9(self):
        self.assertEqual(late("association", 10000, 1200, 15), 200)
        self.assertEqual(late("association", 10000, 900, 15), 0)

    def test_association_pays_no_returned_payment_fee(self):
        self.assertEqual(FeePolicy().returned_payment_fee(Account("a", "association"), 20000), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
