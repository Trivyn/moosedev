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
    def test_sequence_per_entity(self):
        seqs = [self.outbox.emit(entity, "updated", {"n": index}) for index, entity in enumerate(["a", "a", "b", "a"])]
        self.assertEqual(seqs, [1, 2, 1, 3])
        self.assertEqual(self.events(), [("a", 1, "updated"), ("a", 2, "updated"), ("b", 1, "updated"), ("a", 3, "updated")])

    def test_sequence_continues_after_reopen(self):
        self.outbox.emit("a", "created", {})
        self.outbox.emit("a", "updated", {})
        self.reopen()
        self.assertEqual(self.outbox.emit("a", "updated", {}), 3)
        self.assertEqual(self.outbox.emit("b", "created", {}), 1)

    def test_pending_lists_events_in_order(self):
        self.registry.create("x", {"name": "one"})
        self.registry.update("x", {"name": "two"})
        self.assertEqual([(event["entity_id"], event["seq"], event["kind"], event["payload"]) for event in self.outbox.pending()],
                         [("x", 1, "created", {"name": "one"}), ("x", 2, "updated", {"name": "two"})])


if __name__ == "__main__":
    unittest.main(verbosity=2)
