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


class SegmentTests(unittest.TestCase):
    def test_foundation_exempt_cooperative_charged(self):
        self.assertEqual(late("foundation", 10000, 100, 30), 0)
        self.assertEqual(late("cooperative", 10000, 100, 30), 500)


class Np9Tests(unittest.TestCase):
    def test_charity_flat_late_fee_from_day_1000(self):
        self.assertEqual(late("charity", 10000, 1000, 15), 200)
        self.assertEqual(late("charity", 10000, 1000, 14), 0)
        self.assertEqual(late("charity", 10000, 999, 15), 0)
        self.assertEqual(late("retail", 10000, 1000, 15), 500)


class ReturnedPaymentTests(unittest.TestCase):
    def test_returned_payment_fee_for_retail(self):
        policy = FeePolicy()
        self.assertEqual(policy.returned_payment_fee(Account("r", "retail"), 10000), 250)
        self.assertEqual(policy.returned_payment_fee(Account("r", "retail"), 20000), 500)
        self.assertEqual(policy.returned_payment_fee(Account("r", "wholesale"), 1000), 250)

    def test_nonprofits_pay_no_returned_payment_fee(self):
        policy = FeePolicy()
        self.assertEqual(policy.returned_payment_fee(Account("c", "charity"), 20000), 0)
        self.assertEqual(policy.returned_payment_fee(Account("f", "foundation"), 20000), 0)
        self.assertEqual(policy.returned_payment_fee(Account("o", "cooperative"), 20000), 500)

    def test_returned_payment_fee_rounds_half_up(self):
        policy = FeePolicy()
        self.assertEqual(policy.returned_payment_fee(Account("r", "retail"), 10100), 253)
        self.assertEqual(policy.returned_payment_fee(Account("r", "retail"), 10500), 263)


if __name__ == "__main__":
    unittest.main(verbosity=2)
