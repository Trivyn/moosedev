from decimal import Decimal, ROUND_HALF_UP

from catalog import CATALOG


def to_cents(price):
    return int((Decimal(price) * 100).quantize(Decimal("1"), rounding=ROUND_HALF_UP))


class QuoteService:
    def __init__(self, supplier):
        self.supplier = supplier

    def quote(self, sku, qty):
        self._check(sku, qty)
        return self._line(sku, qty, self.supplier.fetch_price(sku))

    def quote_many(self, lines):
        lines = list(lines)
        for sku, qty in lines:
            self._check(sku, qty)
        prices = {}
        for sku, _ in lines:
            if sku not in prices:
                prices[sku] = self.supplier.fetch_price(sku)
        quoted = [self._line(sku, qty, prices[sku]) for sku, qty in lines]
        exact = sum(Decimal(prices[sku]) * qty for sku, qty in lines)
        return {"lines": quoted, "total_cents": to_cents(exact)}

    def _check(self, sku, qty):
        if type(qty) is not int or qty <= 0:
            raise ValueError("quantity must be a positive integer")
        if sku not in CATALOG:
            raise KeyError(sku)

    def _line(self, sku, qty, price):
        unit_cents = to_cents(price)
        return {"sku": sku, "qty": qty, "unit_cents": unit_cents, "total_cents": unit_cents * qty}
