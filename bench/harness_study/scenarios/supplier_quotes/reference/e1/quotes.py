from decimal import Decimal, ROUND_HALF_UP

from catalog import CATALOG


def to_cents(price):
    return int((Decimal(price) * 100).quantize(Decimal("1"), rounding=ROUND_HALF_UP))


class QuoteService:
    def __init__(self, supplier):
        self.supplier = supplier

    def quote(self, sku, qty):
        if type(qty) is not int or qty <= 0:
            raise ValueError("quantity must be a positive integer")
        if sku not in CATALOG:
            raise KeyError(sku)
        unit_cents = to_cents(self.supplier.fetch_price(sku))
        return {"sku": sku, "qty": qty, "unit_cents": unit_cents, "total_cents": unit_cents * qty}
