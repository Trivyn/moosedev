import json


class Registry:
    def __init__(self, outbox):
        self.outbox = outbox
        self.connection = outbox.connection
        with self.connection:
            self.connection.execute("CREATE TABLE IF NOT EXISTS entities (id TEXT PRIMARY KEY, data TEXT NOT NULL)")

    def create(self, entity_id, data):
        if self._row(entity_id) is not None:
            raise ValueError(f"entity already exists: {entity_id}")
        with self.connection:
            self.connection.execute("INSERT INTO entities (id, data) VALUES (?, ?)",
                                    (entity_id, json.dumps(data, sort_keys=True)))
        return self.outbox.emit(entity_id, "created", data, new_epoch=self.outbox.epoch(entity_id) > 0)

    def update(self, entity_id, data):
        if self._row(entity_id) is None:
            raise KeyError(entity_id)
        with self.connection:
            self.connection.execute("UPDATE entities SET data = ? WHERE id = ?",
                                    (json.dumps(data, sort_keys=True), entity_id))
        return self.outbox.emit(entity_id, "updated", data)

    def delete(self, entity_id):
        if self._row(entity_id) is None:
            raise KeyError(entity_id)
        with self.connection:
            self.connection.execute("DELETE FROM entities WHERE id = ?", (entity_id,))
        return self.outbox.emit(entity_id, "deleted", {})

    def delete_many(self, entity_ids):
        entity_ids = list(dict.fromkeys(entity_ids))
        for entity_id in entity_ids:
            if self._row(entity_id) is None:
                raise KeyError(entity_id)
        for entity_id in entity_ids:
            self.delete(entity_id)

    def patch(self, entity_id, changes):
        merged = dict(self.get(entity_id), **changes)
        with self.connection:
            self.connection.execute("UPDATE entities SET data = ? WHERE id = ?",
                                    (json.dumps(merged, sort_keys=True), entity_id))
        return self.outbox.emit(entity_id, "updated", changes)

    def get(self, entity_id):
        row = self._row(entity_id)
        if row is None:
            raise KeyError(entity_id)
        return json.loads(row[0])

    def _row(self, entity_id):
        return self.connection.execute("SELECT data FROM entities WHERE id = ?", (entity_id,)).fetchone()
