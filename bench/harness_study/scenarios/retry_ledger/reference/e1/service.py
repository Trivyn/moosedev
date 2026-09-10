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
            old = self.connection.execute("SELECT amount, total FROM receipts WHERE request_id = ?", (request_id,)).fetchone()
            if old is not None:
                if old[0] != amount:
                    raise ValueError("request ID already used with different amount")
                return {"request_id": request_id, "amount": old[0], "total": old[1]}
            value = self.total() + amount
            self.connection.execute("UPDATE balance SET value = ?", (value,))
            self.connection.execute("INSERT INTO receipts VALUES (?, ?, ?)", (request_id, amount, value))
        return {"request_id": request_id, "amount": amount, "total": value}
