from storage import connect
from report import total

class Ledger:
    def __init__(self, path):
        self.connection = connect(path)

    def close(self):
        self.connection.close()

    def total(self, account="default"):
        return total(self.connection, account)

    def process(self, request_id, amount, *, account="default"):
        with self.connection:
            old = self.connection.execute("SELECT amount, total FROM receipts WHERE account = ? AND request_id = ?", (account, request_id)).fetchone()
            if old is not None:
                if old[0] != amount:
                    raise ValueError("request ID already used with different amount")
                return {"request_id": request_id, "amount": old[0], "total": old[1]}
            value = self.total(account) + amount
            self.connection.execute("INSERT INTO balances VALUES (?, ?) ON CONFLICT(account) DO UPDATE SET value = excluded.value", (account, value))
            self.connection.execute("INSERT INTO receipts VALUES (?, ?, ?, ?)", (account, request_id, amount, value))
        return {"request_id": request_id, "amount": amount, "total": value}

    def process_batch(self, operations):
        return [self.process(request_id, amount, account=account)
                for account, request_id, amount in operations]
