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


class EpochTests(Store):
    def test_recreate_starts_new_epoch(self):
        self.registry.create("x", {"v": 1})
        self.registry.update("x", {"v": 2})
        self.registry.delete("x")
        self.assertEqual(self.outbox.epoch("x"), 1)
        self.reopen()
        self.registry.create("x", {"v": 3})
        self.registry.update("x", {"v": 4})
        self.assertEqual(self.outbox.epoch("x"), 2)
        self.assertEqual([(event["epoch"], event["seq"], event["kind"]) for event in self.outbox.pending()],
                         [(1, 1, "created"), (1, 2, "updated"), (1, 3, "deleted"), (2, 1, "created"), (2, 2, "updated")])

    def test_epoch_of_unknown_is_zero(self):
        self.assertEqual(self.outbox.epoch("nobody"), 0)
        self.outbox.emit("a", "updated", {})
        self.assertEqual(self.outbox.epoch("a"), 1)

    def test_compaction_never_resets_sequence_within_epoch(self):
        self.registry.create("x", {"v": 1})
        self.registry.update("x", {"v": 2})
        self.outbox.ack("x", 1)
        self.outbox.ack("x", 2)
        self.outbox.compact()
        self.reopen()
        self.registry.update("x", {"v": 3})
        self.assertEqual([(event["epoch"], event["seq"]) for event in self.outbox.pending()], [(1, 3)])


class PatchTests(Store):
    def test_patch_merges_data(self):
        self.registry.create("x", {"a": 1, "b": 2})
        self.registry.patch("x", {"b": 3, "c": 4})
        self.assertEqual(self.registry.get("x"), {"a": 1, "b": 3, "c": 4})
        self.assertEqual(self.outbox.pending()[-1]["kind"], "updated")
        with self.assertRaises(KeyError):
            self.registry.patch("missing", {"a": 1})

    def test_patch_event_carries_complete_data(self):
        self.registry.create("x", {"a": 1, "b": 2})
        self.registry.patch("x", {"b": 3})
        last = self.outbox.pending()[-1]
        self.assertEqual((last["entity_id"], last["kind"], last["payload"]), ("x", "updated", {"a": 1, "b": 3}))


if __name__ == "__main__":
    unittest.main(verbosity=2)
