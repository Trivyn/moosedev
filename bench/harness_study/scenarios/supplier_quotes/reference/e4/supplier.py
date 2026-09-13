class Supplier:
    """Supplier price feed.

    fetch_price(sku) returns the current unit price as a decimal string, for example "1.25".
    """

    def fetch_price(self, sku):
        raise NotImplementedError
