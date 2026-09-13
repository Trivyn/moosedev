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
    def test_charity_pays_no_late_fee(self):
        self.assertEqual(late("charity", 10000, 100, 30), 0)
        self.assertEqual(late("charity", 1000, 100, 30), 0)


class SegmentTests(unittest.TestCase):
    def test_new_segments_and_display_names(self):
        policy = FeePolicy()
        for segment, shown in (("foundation", "Registered foundation"), ("cooperative", "Member cooperative"),
                               ("retail", "Retail"), ("charity", "Registered charity")):
            line = statement_line(Account("acct", segment), Invoice("INV-7", 10000, 100), 101, policy)
            self.assertEqual(line, f"INV-7 {shown} late fee 0")
        with self.assertRaises(ValueError):
            Account("acct", "club")

    def test_foundation_exempt_cooperative_charged(self):
        self.assertEqual(late("foundation", 10000, 100, 30), 0)
        self.assertEqual(late("cooperative", 10000, 100, 30), 500)


if __name__ == "__main__":
    unittest.main(verbosity=2)
