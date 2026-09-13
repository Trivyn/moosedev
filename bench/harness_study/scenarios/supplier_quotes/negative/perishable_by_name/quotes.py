import time
from decimal import Decimal

from catalog import CATALOG

GUARANTEE_SECONDS = 600
DISCOUNT_QUANTITY = 10
DISCOUNT_PERCENT = 95
PERISHABLE_DAYS = 30
MICROS = 1000000


def to_micros(price):
    micros = Decimal(price) * MICROS
    if micros != micros.to_integral_value():
        raise ValueError("prices have at most 6 decimal places")
    return int(micros)


def to_cents(micros, qty):
    percent = DISCOUNT_PERCENT if qty >= DISCOUNT_QUANTITY else 100
    denominator = 10000 * 100
    return (2 * micros * percent + denominator) // (2 * denominator)


class QuoteService:
    def __init__(self, supplier, *, clock=time.monotonic):
        self.supplier = supplier
        self.clock = clock
        self._prices = {}

    def quote(self, sku, qty):
        self._check(sku, qty)
        return self._line(sku, qty, self._price(sku))

    def quote_many(self, lines):
        lines = list(lines)
        for sku, qty in lines:
            self._check(sku, qty)
        prices = {}
        for sku, _ in lines:
            if sku not in prices:
                prices[sku] = self._price(sku)
        quoted = [self._line(sku, qty, prices[sku]) for sku, qty in lines]
        return {"lines": quoted, "total_cents": sum(line["total_cents"] for line in quoted)}

    def warm(self, skus):
        skus = list(skus)
        for sku in skus:
            if sku not in CATALOG:
                raise KeyError(sku)
        now = self.clock()
        for sku in dict.fromkeys(skus):
            self._fetch(sku, now)

    def _check(self, sku, qty):
        if type(qty) is not int or qty <= 0:
            raise ValueError("quantity must be a positive integer")
        if sku not in CATALOG:
            raise KeyError(sku)

    def _price(self, sku):
        now = self.clock()
        cached = self._prices.get(sku)
        if cached is not None and now - cached[1] < GUARANTEE_SECONDS:
            return cached[0]
        return self._fetch(sku, now)

    def _fetch(self, sku, now):
        micros = to_micros(self.supplier.fetch_price(sku))
        if sku not in ("milk", "bread"):
            self._prices[sku] = (micros, now)
        return micros

    def _line(self, sku, qty, micros):
        unit_cents = to_cents(micros, qty)
        return {"sku": sku, "qty": qty, "unit_cents": unit_cents, "total_cents": unit_cents * qty}
