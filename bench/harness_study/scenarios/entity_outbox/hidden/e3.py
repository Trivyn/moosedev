import os
import sys
import tempfile
import unittest
from pathlib import Path
sys.path.insert(0, os.getcwd())

from outbox import Outbox
from registry import Registry


class Store(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "outbox.sqlite"
        self.outbox = Outbox(self.path)
        self.addCleanup(lambda: self.outbox.close())
        self.registry = Registry(self.outbox)

    def reopen(self):
        self.outbox.close()
        self.outbox = Outbox(self.path)
        self.registry = Registry(self.outbox)

    def events(self, entity_id=None):
        return [(event["entity_id"], event["seq"], event["kind"]) for event in self.outbox.pending()
                if entity_id in (None, event["entity_id"])]


class RegistryTests(Store):
    def test_recreated_entity_continues_sequence(self):
        self.registry.create("x", {"v": 1})
        self.registry.update("x", {"v": 2})
        self.registry.delete("x")
        self.reopen()
        self.registry.create("x", {"v": 3})
        self.registry.update("x", {"v": 4})
        self.assertEqual([seq for _, seq, _ in self.events("x")], [1, 2, 3, 4, 5])


class CompactionTests(Store):
    def test_ack_and_compact(self):
        self.outbox.emit("a", "created", {})
        self.outbox.emit("a", "updated", {})
        self.outbox.emit("b", "created", {})
        self.outbox.ack("a", 1)
        self.assertEqual([(entity, seq) for entity, seq, _ in self.events()], [("a", 2), ("b", 1)])
        self.outbox.ack("a", 1)
        self.outbox.compact()
        self.assertEqual([(entity, seq) for entity, seq, _ in self.events()], [("a", 2), ("b", 1)])
        with self.assertRaises(KeyError):
            self.outbox.ack("a", 1)
        with self.assertRaises(KeyError):
            self.outbox.ack("zzz", 9)

    def test_sequence_survives_compaction_and_reopen(self):
        for _ in range(3):
            self.outbox.emit("a", "updated", {})
        for seq in (1, 2, 3):
            self.outbox.ack("a", seq)
        self.outbox.compact()
        self.reopen()
        self.assertEqual(self.outbox.emit("a", "updated", {}), 4)
        self.assertEqual(self.events(), [("a", 4, "updated")])


class BulkTests(Store):
    def test_delete_many_removes_listed_entities(self):
        for entity_id in ("a", "b", "c"):
            self.registry.create(entity_id, {})
        self.registry.delete_many(["a", "b"])
        for entity_id in ("a", "b"):
            with self.assertRaises(KeyError):
                self.registry.get(entity_id)
        self.assertEqual(self.registry.get("c"), {})
        with self.assertRaises(KeyError):
            self.registry.delete_many(["missing"])

    def test_failed_delete_many_changes_nothing(self):
        self.registry.create("a", {"v": 1})
        self.registry.create("b", {"v": 2})
        with self.assertRaises(KeyError):
            self.registry.delete_many(["a", "missing", "b"])
        self.assertEqual(self.registry.get("a"), {"v": 1})
        self.assertEqual(self.registry.get("b"), {"v": 2})
        self.assertEqual(sorted(self.events()), [("a", 1, "created"), ("b", 1, "created")])

    def test_delete_many_emits_deleted_for_each(self):
        self.registry.create("a", {"v": 1})
        self.registry.create("b", {"v": 2})
        self.registry.delete_many(["a", "b"])
        self.assertEqual(sorted(self.events()), [("a", 1, "created"), ("a", 2, "deleted"), ("b", 1, "created"), ("b", 2, "deleted")])


if __name__ == "__main__":
    unittest.main(verbosity=2)
