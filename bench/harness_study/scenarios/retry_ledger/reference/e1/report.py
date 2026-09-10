def total(connection):
    return connection.execute("SELECT value FROM balance").fetchone()[0]
