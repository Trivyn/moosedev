import sqlite3

def connect(path):
    connection = sqlite3.connect(path)
    with connection:
        tables = {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
        if "balance" in tables:
            connection.execute("CREATE TABLE balances (account TEXT PRIMARY KEY, value INTEGER NOT NULL)")
            connection.execute("INSERT INTO balances SELECT 'default', value FROM balance")
            connection.execute("ALTER TABLE receipts RENAME TO old_receipts")
            connection.execute("CREATE TABLE receipts (account TEXT NOT NULL, request_id TEXT NOT NULL, amount INTEGER NOT NULL, total INTEGER NOT NULL, PRIMARY KEY(account, request_id))")
            connection.execute("INSERT INTO receipts SELECT 'default', request_id, amount, total FROM old_receipts")
            connection.execute("DROP TABLE old_receipts")
            connection.execute("DROP TABLE balance")
        else:
            connection.execute("CREATE TABLE IF NOT EXISTS balances (account TEXT PRIMARY KEY, value INTEGER NOT NULL)")
            connection.execute("CREATE TABLE IF NOT EXISTS receipts (account TEXT NOT NULL, request_id TEXT NOT NULL, amount INTEGER NOT NULL, total INTEGER NOT NULL, PRIMARY KEY(account, request_id))")
    return connection
