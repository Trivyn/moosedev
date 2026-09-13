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


class BasketTests(unittest.TestCase):
    def test_one_fetch_per_distinct_sku(self):
        supplier = Supplier({"rice": "1.00", "milk": "2.50"})
        result = QuoteService(supplier).quote_many([("rice", 1), ("milk", 2), ("rice", 3)])
        self.assertEqual([line["sku"] for line in result["lines"]], ["rice", "milk", "rice"])
        self.assertEqual([line["total_cents"] for line in result["lines"]], [100, 500, 300])
        self.assertEqual(result["total_cents"], 900)
        self.assertEqual(sorted(supplier.calls), ["milk", "rice"])

    def test_empty_basket(self):
        supplier = Supplier({})
        self.assertEqual(QuoteService(supplier).quote_many([]), {"lines": [], "total_cents": 0})
        self.assertEqual(supplier.calls, [])

    def test_prices_refetched_between_baskets(self):
        supplier = Supplier({"rice": "1.00"})
        service = QuoteService(supplier)
        service.quote_many([("rice", 1)])
        supplier.prices["rice"] = "1.20"
        self.assertEqual(service.quote_many([("rice", 1)])["total_cents"], 120)
        self.assertEqual(service.quote("rice", 1)["total_cents"], 120)
        self.assertEqual(supplier.calls, ["rice", "rice", "rice"])

    def test_basket_total_rounds_each_unit(self):
        supplier = Supplier({"rice": "0.125", "flour": "0.125"})
        self.assertEqual(QuoteService(supplier).quote_many([("rice", 1), ("flour", 1)])["total_cents"], 26)

    def test_unknown_sku_rejected_before_any_fetch(self):
        supplier = Supplier({"rice": "1.00", "caviar": "99.00"})
        with self.assertRaises(KeyError):
            QuoteService(supplier).quote_many([("rice", 1), ("caviar", 1)])
        self.assertEqual(supplier.calls, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
