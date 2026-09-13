import json

from db import connect


class Outbox:
    def __init__(self, path):
        self.connection = connect(path)
        with self.connection:
            self.connection.execute(
                "CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, "
                "entity_id TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL)")

    def close(self):
        self.connection.close()

    def emit(self, entity_id, kind, payload):
        with self.connection:
            cursor = self.connection.execute(
                "INSERT INTO events (entity_id, kind, payload) VALUES (?, ?, ?)",
                (entity_id, kind, json.dumps(payload, sort_keys=True)))
        return cursor.lastrowid

    def pending(self):
        rows = self.connection.execute("SELECT entity_id, seq, kind, payload FROM events ORDER BY seq").fetchall()
        return [{"entity_id": entity_id, "seq": seq, "kind": kind, "payload": json.loads(payload)}
                for entity_id, seq, kind, payload in rows]
