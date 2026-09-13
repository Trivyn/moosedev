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


class WarmTests(unittest.TestCase):
    def test_guarantee_still_reuses_prices(self):
        supplier = Supplier({"rice": "1.00"})
        clock = Clock()
        service = QuoteService(supplier, clock=clock)
        service.quote("rice", 2)
        supplier.prices["rice"] = "5.00"
        clock.now = 300
        self.assertEqual(service.quote_many([("rice", 1)])["total_cents"], 100)
        self.assertEqual(supplier.calls, ["rice"])


class MicroTests(unittest.TestCase):
    def test_six_decimal_prices_parse(self):
        service = QuoteService(Supplier({"rice": "1.234567", "flour": "2.0000005"}), clock=Clock())
        self.assertEqual(service.quote("rice", 1), {"sku": "rice", "qty": 1, "unit_cents": 123, "total_cents": 123})
        result = service.quote("rice", 2)
        self.assertIs(type(result["unit_cents"]), int)
        self.assertIs(type(result["total_cents"]), int)
        with self.assertRaises(ValueError):
            service.quote("flour", 1)

    def test_new_skus_are_quotable(self):
        service = QuoteService(Supplier({"frozen_peas": "3.25", "flowers": "4.00"}), clock=Clock())
        self.assertEqual(service.quote("frozen_peas", 2)["total_cents"], 650)
        self.assertEqual(service.quote("flowers", 1)["total_cents"], 400)

    def test_six_decimal_prices_round_each_unit(self):
        service = QuoteService(Supplier({"rice": "0.333333", "flour": "0.005", "milk": "0.333333"}), clock=Clock())
        self.assertEqual(service.quote("rice", 3), {"sku": "rice", "qty": 3, "unit_cents": 33, "total_cents": 99})
        self.assertEqual(service.quote("flour", 1)["total_cents"], 1)
        self.assertEqual(service.quote_many([("milk", 10)])["total_cents"], 320)

    def test_new_short_shelf_life_sku_is_perishable(self):
        supplier = Supplier({"flowers": "4.00", "frozen_peas": "3.25"})
        clock = Clock()
        service = QuoteService(supplier, clock=clock)
        service.quote("flowers", 1)
        service.quote("frozen_peas", 1)
        clock.now = 10
        service.quote("flowers", 1)
        service.quote("frozen_peas", 1)
        self.assertEqual(supplier.calls.count("flowers"), 2)
        self.assertEqual(supplier.calls.count("frozen_peas"), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
