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


class RenameTests(Store):
    def test_rename_moves_data(self):
        self.registry.create("old", {"v": 1})
        self.registry.rename("old", "new")
        self.assertEqual(self.registry.get("new"), {"v": 1})
        with self.assertRaises(KeyError):
            self.registry.get("old")

    def test_rename_errors(self):
        self.registry.create("a", {})
        self.registry.create("b", {})
        with self.assertRaises(ValueError):
            self.registry.rename("a", "b")
        with self.assertRaises(KeyError):
            self.registry.rename("missing", "c")
        self.assertEqual(self.registry.get("a"), {})

    def test_rename_emits_deleted_and_full_created(self):
        self.registry.create("old", {"v": 1, "w": 2})
        self.registry.rename("old", "new")
        tail = self.outbox.pending()[1:]
        self.assertEqual(sorted((event["entity_id"], event["kind"]) for event in tail), [("new", "created"), ("old", "deleted")])
        self.assertEqual([event["payload"] for event in tail if event["entity_id"] == "new"], [{"v": 1, "w": 2}])

    def test_rename_onto_deleted_id_starts_new_epoch(self):
        self.registry.create("y", {"v": 1})
        self.registry.update("y", {"v": 2})
        self.registry.delete("y")
        self.registry.create("x", {"v": 3})
        self.registry.rename("x", "y")
        last = self.outbox.pending()[-1]
        self.assertEqual((last["entity_id"], last["kind"], last["epoch"], last["seq"]), ("y", "created", 2, 1))
        self.assertEqual(self.outbox.epoch("y"), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
