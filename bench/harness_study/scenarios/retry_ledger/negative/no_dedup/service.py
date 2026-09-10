from storage import connect
from report import total

class Ledger:
    def __init__(self, path):
        self.connection = connect(path)

    def close(self):
        self.connection.close()

    def total(self):
        return total(self.connection)

    def process(self, request_id, amount):
        with self.connection:
            value = self.total() + amount
            self.connection.execute("UPDATE balance SET value = ?", (value,))
        return {"request_id": request_id, "amount": amount, "total": value}
