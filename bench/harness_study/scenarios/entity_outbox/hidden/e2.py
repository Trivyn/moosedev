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


class OutboxTests(Store):
    def test_sequence_continues_after_reopen(self):
        self.outbox.emit("a", "created", {})
        self.outbox.emit("a", "updated", {})
        self.reopen()
        self.assertEqual(self.outbox.emit("a", "updated", {}), 3)
        self.assertEqual(self.outbox.emit("b", "created", {}), 1)


class RegistryTests(Store):
    def test_delete_emits_and_hides(self):
        self.registry.create("x", {"v": 1})
        self.registry.delete("x")
        with self.assertRaises(KeyError):
            self.registry.get("x")
        with self.assertRaises(KeyError):
            self.registry.delete("x")
        with self.assertRaises(KeyError):
            self.registry.delete("never")
        self.assertEqual([kind for _, _, kind in self.events("x")], ["created", "deleted"])

    def test_recreate_after_delete_allowed(self):
        self.registry.create("x", {"v": 1})
        self.registry.delete("x")
        self.registry.create("x", {"v": 2})
        self.assertEqual(self.registry.get("x"), {"v": 2})

    def test_recreated_entity_continues_sequence(self):
        self.registry.create("x", {"v": 1})
        self.registry.update("x", {"v": 2})
        self.registry.delete("x")
        self.reopen()
        self.registry.create("x", {"v": 3})
        self.registry.update("x", {"v": 4})
        self.assertEqual([seq for _, seq, _ in self.events("x")], [1, 2, 3, 4, 5])


if __name__ == "__main__":
    unittest.main(verbosity=2)
