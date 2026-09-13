import time
from decimal import Decimal, ROUND_HALF_UP

from catalog import CATALOG

GUARANTEE_SECONDS = 600
DISCOUNT_QUANTITY = 10
DISCOUNT = Decimal("0.95")
PERISHABLE_DAYS = 30


def to_cents(price, qty):
    cents = Decimal(price) * 100
    if qty >= DISCOUNT_QUANTITY:
        cents *= DISCOUNT
    return int(cents.quantize(Decimal("1"), rounding=ROUND_HALF_UP))


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

    def _check(self, sku, qty):
        if type(qty) is not int or qty <= 0:
            raise ValueError("quantity must be a positive integer")
        if sku not in CATALOG:
            raise KeyError(sku)

    def warm(self, skus):
        now = self.clock()
        for sku in dict.fromkeys(skus):
            if sku not in CATALOG:
                raise KeyError(sku)
            self._fetch(sku, now)

    def _price(self, sku):
        now = self.clock()
        cached = self._prices.get(sku)
        if cached is not None and now - cached[1] < GUARANTEE_SECONDS:
            return cached[0]
        return self._fetch(sku, now)

    def _fetch(self, sku, now):
        price = self.supplier.fetch_price(sku)
        if CATALOG[sku]["shelf_life_days"] >= PERISHABLE_DAYS:
            self._prices[sku] = (price, now)
        return price

    def _line(self, sku, qty, price):
        unit_cents = to_cents(price, qty)
        return {"sku": sku, "qty": qty, "unit_cents": unit_cents, "total_cents": unit_cents * qty}
