import json

from db import connect


class Outbox:
    def __init__(self, path):
        self.connection = connect(path)
        with self.connection:
            self.connection.execute(
                "CREATE TABLE IF NOT EXISTS events (position INTEGER PRIMARY KEY AUTOINCREMENT, "
                "entity_id TEXT NOT NULL, epoch INTEGER NOT NULL, seq INTEGER NOT NULL, kind TEXT NOT NULL, "
                "payload TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0, UNIQUE (entity_id, epoch, seq))")
            self.connection.execute(
                "CREATE TABLE IF NOT EXISTS sequences (entity_id TEXT PRIMARY KEY, epoch INTEGER NOT NULL, "
                "last_seq INTEGER NOT NULL)")

    def close(self):
        self.connection.close()

    def emit(self, entity_id, kind, payload, *, new_epoch=False):
        with self.connection:
            row = self.connection.execute(
                "SELECT epoch, last_seq FROM sequences WHERE entity_id = ?", (entity_id,)).fetchone()
            if row is None:
                epoch, seq = 1, 1
            elif new_epoch:
                epoch, seq = row[0] + 1, 1
            else:
                epoch, seq = row[0], row[1] + 1
            self.connection.execute(
                "INSERT OR REPLACE INTO sequences (entity_id, epoch, last_seq) VALUES (?, ?, ?)",
                (entity_id, epoch, seq))
            self.connection.execute(
                "INSERT INTO events (entity_id, epoch, seq, kind, payload) VALUES (?, ?, ?, ?, ?)",
                (entity_id, epoch, seq, kind, json.dumps(payload, sort_keys=True)))
        return seq

    def epoch(self, entity_id):
        row = self.connection.execute("SELECT epoch FROM sequences WHERE entity_id = ?", (entity_id,)).fetchone()
        return 0 if row is None else row[0]

    def ack(self, entity_id, seq, *, epoch=None):
        epoch = self.epoch(entity_id) if epoch is None else epoch
        with self.connection:
            cursor = self.connection.execute(
                "UPDATE events SET delivered = 1 WHERE entity_id = ? AND epoch = ? AND seq = ?",
                (entity_id, epoch, seq))
        if cursor.rowcount == 0:
            raise KeyError((entity_id, epoch, seq))

    def ack_many(self, events):
        events = list(events)
        with self.connection:
            for entity_id, seq in events:
                cursor = self.connection.execute(
                    "UPDATE events SET delivered = 1 WHERE entity_id = ? AND epoch = ? AND seq = ?",
                    (entity_id, self.epoch(entity_id), seq))
                if cursor.rowcount == 0:
                    raise KeyError((entity_id, seq))

    def compact(self):
        with self.connection:
            self.connection.execute("DELETE FROM events WHERE delivered = 1")

    def pending(self):
        rows = self.connection.execute(
            "SELECT entity_id, epoch, seq, kind, payload FROM events WHERE delivered = 0 ORDER BY position").fetchall()
        return [{"entity_id": entity_id, "epoch": epoch, "seq": seq, "kind": kind, "payload": json.loads(payload)}
                for entity_id, epoch, seq, kind, payload in rows]
