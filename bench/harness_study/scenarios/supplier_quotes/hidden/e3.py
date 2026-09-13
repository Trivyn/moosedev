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
    def test_unknown_sku_rejected_before_fetch(self):
        supplier = Supplier({"caviar": "99.00"})
        with self.assertRaises(KeyError):
            QuoteService(supplier, clock=Clock()).quote("caviar", 1)
        self.assertEqual(supplier.calls, [])


class GuaranteeTests(unittest.TestCase):
    def test_reuse_within_600_seconds(self):
        supplier = Supplier({"rice": "1.00"})
        clock = Clock()
        service = QuoteService(supplier, clock=clock)
        service.quote("rice", 1)
        supplier.prices["rice"] = "1.50"
        clock.now = 599
        self.assertEqual(service.quote_many([("rice", 1)])["total_cents"], 100)
        self.assertEqual(service.quote("rice", 1)["unit_cents"], 100)
        clock.now = 600
        self.assertEqual(service.quote("rice", 1)["unit_cents"], 150)
        self.assertEqual(supplier.calls, ["rice", "rice"])


class DiscountTests(unittest.TestCase):
    def test_discount_applies_from_ten_units(self):
        service = QuoteService(Supplier({"rice": "2.00"}), clock=Clock())
        self.assertEqual(service.quote("rice", 9), {"sku": "rice", "qty": 9, "unit_cents": 200, "total_cents": 1800})
        self.assertEqual(service.quote("rice", 10), {"sku": "rice", "qty": 10, "unit_cents": 190, "total_cents": 1900})
        self.assertEqual(service.quote_many([("rice", 10), ("rice", 1)])["total_cents"], 2100)

    def test_discount_rounds_each_unit(self):
        service = QuoteService(Supplier({"rice": "0.125", "flour": "0.125"}), clock=Clock())
        self.assertEqual(service.quote("rice", 10), {"sku": "rice", "qty": 10, "unit_cents": 12, "total_cents": 120})
        self.assertEqual(service.quote_many([("rice", 10), ("flour", 10)])["total_cents"], 240)


if __name__ == "__main__":
    unittest.main(verbosity=2)
