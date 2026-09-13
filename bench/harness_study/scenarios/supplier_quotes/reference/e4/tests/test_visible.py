import unittest

from quotes import QuoteService


class Supplier:
    def fetch_price(self, sku):
        return "1.25"


class VisibleTests(unittest.TestCase):
    def test_quotes_one_line(self):
        self.assertEqual(QuoteService(Supplier()).quote("rice", 2),
                         {"sku": "rice", "qty": 2, "unit_cents": 125, "total_cents": 250})

    def test_quotes_a_basket(self):
        result = QuoteService(Supplier()).quote_many([("rice", 2), ("flour", 1)])
        self.assertEqual(result["total_cents"], 375)
        self.assertEqual(len(result["lines"]), 2)

    def test_volume_discount(self):
        self.assertEqual(QuoteService(Supplier()).quote("rice", 10)["unit_cents"], 119)

    def test_warm_then_quote(self):
        service = QuoteService(Supplier())
        service.warm(["rice"])
        self.assertEqual(service.quote("rice", 1)["total_cents"], 125)


if __name__ == "__main__":
    unittest.main()
