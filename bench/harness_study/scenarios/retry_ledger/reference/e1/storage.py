import sqlite3

def connect(path):
    connection = sqlite3.connect(path)
    connection.execute("CREATE TABLE IF NOT EXISTS balance (value INTEGER NOT NULL)")
    if connection.execute("SELECT COUNT(*) FROM balance").fetchone()[0] == 0:
        connection.execute("INSERT INTO balance VALUES (0)")
    connection.execute("CREATE TABLE IF NOT EXISTS receipts (request_id TEXT PRIMARY KEY, amount INTEGER NOT NULL, total INTEGER NOT NULL)")
    connection.commit()
    return connection
