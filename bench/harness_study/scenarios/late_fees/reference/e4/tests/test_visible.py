import unittest

from accounts import Account
from fees import FeePolicy
from invoices import Invoice
from statements import statement_line


class VisibleTests(unittest.TestCase):
    def test_retail_late_fee(self):
        self.assertEqual(FeePolicy().late_fee(Account("r", "retail"), Invoice("INV-1", 10000, 100), 130), 500)

    def test_statement_line(self):
        line = statement_line(Account("r", "retail"), Invoice("INV-1", 10000, 100), 101, FeePolicy())
        self.assertEqual(line, "INV-1 Retail late fee 0")

    def test_returned_payment_fee(self):
        self.assertEqual(FeePolicy().returned_payment_fee(Account("r", "retail"), 20000), 500)

    def test_forecast(self):
        self.assertEqual(FeePolicy().fee_forecast(Account("r", "retail"), Invoice("INV-1", 10000, 100), [101, 130]), [0, 500])

    def test_collection_penalty(self):
        self.assertEqual(FeePolicy().collection_penalty(Account("r", "retail"), Invoice("INV-1", 10000, 100), 170), 1500)


if __name__ == "__main__":
    unittest.main()
