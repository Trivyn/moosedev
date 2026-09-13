import tempfile
import unittest
from pathlib import Path

from outbox import Outbox
from registry import Registry


class VisibleTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.outbox = Outbox(Path(directory.name) / "outbox.sqlite")
        self.addCleanup(self.outbox.close)
        self.registry = Registry(self.outbox)

    def test_create_and_get(self):
        self.assertEqual(self.registry.create("x", {"name": "one"}), 1)
        self.assertEqual(self.registry.get("x"), {"name": "one"})


if __name__ == "__main__":
    unittest.main()
