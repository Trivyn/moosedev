import os
import sys
import unittest
sys.path.insert(0, os.getcwd())

from quotes import QuoteService


class Supplier:
    def __init__(self, prices):
        self.prices = dict(prices)
        self.calls = []

    def fetch_price(self, sku):
        self.calls.append(sku)
        return self.prices[sku]


class Clock:
    def __init__(self, now=0.0):
        self.now = now

    def __call__(self):
        return self.now


class QuoteTests(unittest.TestCase):
    def test_half_up_per_unit(self):
        service = QuoteService(Supplier({"rice": "0.125", "flour": "2.675"}))
        self.assertEqual(service.quote("rice", 3), {"sku": "rice", "qty": 3, "unit_cents": 13, "total_cents": 39})
        self.assertEqual(service.quote("flour", 1)["unit_cents"], 268)

    def test_rejects_non_positive_quantity(self):
        service = QuoteService(Supplier({"rice": "1.00"}))
        for qty in (0, -2, 1.5):
            with self.assertRaises(ValueError):
                service.quote("rice", qty)

    def test_unknown_sku_rejected_before_fetch(self):
        supplier = Supplier({"caviar": "99.00"})
        with self.assertRaises(KeyError):
            QuoteService(supplier).quote("caviar", 1)
        self.assertEqual(supplier.calls, [])

    def test_every_quote_fetches(self):
        supplier = Supplier({"rice": "1.00"})
        service = QuoteService(supplier)
        service.quote("rice", 1)
        supplier.prices["rice"] = "1.10"
        self.assertEqual(service.quote("rice", 1)["unit_cents"], 110)
        self.assertEqual(supplier.calls, ["rice", "rice"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
