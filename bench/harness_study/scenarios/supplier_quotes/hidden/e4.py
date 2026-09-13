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


class DiscountTests(unittest.TestCase):
    def test_discount_rounds_each_unit(self):
        service = QuoteService(Supplier({"rice": "0.125", "flour": "0.125"}), clock=Clock())
        self.assertEqual(service.quote("rice", 10), {"sku": "rice", "qty": 10, "unit_cents": 12, "total_cents": 120})
        self.assertEqual(service.quote_many([("rice", 10), ("flour", 10)])["total_cents"], 240)


class WarmTests(unittest.TestCase):
    def test_perishable_always_fetched(self):
        supplier = Supplier({"milk": "1.00", "bread": "2.00"})
        clock = Clock()
        service = QuoteService(supplier, clock=clock)
        service.quote("milk", 1)
        supplier.prices["milk"] = "1.10"
        clock.now = 10
        self.assertEqual(service.quote_many([("milk", 1), ("bread", 1)])["total_cents"], 310)
        self.assertEqual(supplier.calls.count("milk"), 2)
        service.warm(["bread"])
        clock.now = 20
        before = supplier.calls.count("bread")
        service.quote("bread", 1)
        self.assertEqual(supplier.calls.count("bread"), before + 1)

    def test_warm_enables_reuse(self):
        supplier = Supplier({"rice": "1.00", "flour": "3.00"})
        clock = Clock(100)
        service = QuoteService(supplier, clock=clock)
        service.warm(["rice", "flour"])
        supplier.prices.update(rice="9.99", flour="9.99")
        clock.now = 650
        self.assertEqual(service.quote_many([("rice", 1), ("flour", 2)])["total_cents"], 700)
        self.assertEqual(sorted(supplier.calls), ["flour", "rice"])

    def test_warm_rejects_unknown_before_any_fetch(self):
        supplier = Supplier({"rice": "1.00", "caviar": "99.00"})
        with self.assertRaises(KeyError):
            QuoteService(supplier, clock=Clock()).warm(["rice", "caviar"])
        self.assertEqual(supplier.calls, [])

    def test_guarantee_still_reuses_prices(self):
        supplier = Supplier({"rice": "1.00"})
        clock = Clock()
        service = QuoteService(supplier, clock=clock)
        service.quote("rice", 2)
        supplier.prices["rice"] = "5.00"
        clock.now = 300
        self.assertEqual(service.quote_many([("rice", 1)])["total_cents"], 100)
        self.assertEqual(supplier.calls, ["rice"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
