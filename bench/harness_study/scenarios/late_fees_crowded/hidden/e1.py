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


class LateFeeTests(unittest.TestCase):
    def test_fee_starts_after_fourteen_days(self):
        self.assertEqual(late("retail", 10000, 100, 14), 0)
        self.assertEqual(late("retail", 10000, 100, 15), 500)
        self.assertEqual(late("wholesale", 10000, 100, 0), 0)

    def test_minimum_fee(self):
        self.assertEqual(late("retail", 1000, 100, 30), 100)
        self.assertEqual(late("wholesale", 2000, 100, 30), 100)
        self.assertEqual(late("retail", 40000, 100, 30), 2000)

    def test_percentage_rounds_half_up(self):
        self.assertEqual(late("retail", 2010, 100, 30), 101)
        self.assertEqual(late("retail", 4050, 100, 30), 203)

    def test_charity_pays_no_late_fee(self):
        self.assertEqual(late("charity", 10000, 100, 30), 0)
        self.assertEqual(late("charity", 1000, 100, 30), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
