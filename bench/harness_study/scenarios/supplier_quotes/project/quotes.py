class QuoteService:
    def __init__(self, supplier):
        self.supplier = supplier

    def quote(self, sku, qty):
        unit = float(self.supplier.fetch_price(sku))
        return {"sku": sku, "qty": qty, "unit_cents": round(unit * 100), "total_cents": round(unit * qty * 100)}
