def total(connection, account="default"):
    row = connection.execute("SELECT value FROM balances WHERE account = ?", (account,)).fetchone()
    return row[0] if row else 0
