import unittest

from quotes import QuoteService


class Supplier:
    def fetch_price(self, sku):
        return "1.25"


class VisibleTests(unittest.TestCase):
    def test_quotes_one_line(self):
        self.assertEqual(QuoteService(Supplier()).quote("rice", 2),
                         {"sku": "rice", "qty": 2, "unit_cents": 125, "total_cents": 250})


if __name__ == "__main__":
    unittest.main()
