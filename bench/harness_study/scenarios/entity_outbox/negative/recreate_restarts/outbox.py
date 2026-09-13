import json

from db import connect


class Outbox:
    def __init__(self, path):
        self.connection = connect(path)
        with self.connection:
            self.connection.execute(
                "CREATE TABLE IF NOT EXISTS events (position INTEGER PRIMARY KEY AUTOINCREMENT, "
                "entity_id TEXT NOT NULL, seq INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL)")
            self.connection.execute(
                "CREATE TABLE IF NOT EXISTS sequences (entity_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL)")

    def close(self):
        self.connection.close()

    def emit(self, entity_id, kind, payload):
        with self.connection:
            row = self.connection.execute(
                "SELECT last_seq FROM sequences WHERE entity_id = ?", (entity_id,)).fetchone()
            seq = 1 if row is None else row[0] + 1
            self.connection.execute(
                "INSERT OR REPLACE INTO sequences (entity_id, last_seq) VALUES (?, ?)", (entity_id, seq))
            self.connection.execute(
                "INSERT INTO events (entity_id, seq, kind, payload) VALUES (?, ?, ?, ?)",
                (entity_id, seq, kind, json.dumps(payload, sort_keys=True)))
        return seq

    def pending(self):
        rows = self.connection.execute(
            "SELECT entity_id, seq, kind, payload FROM events ORDER BY position").fetchall()
        return [{"entity_id": entity_id, "seq": seq, "kind": kind, "payload": json.loads(payload)}
                for entity_id, seq, kind, payload in rows]
