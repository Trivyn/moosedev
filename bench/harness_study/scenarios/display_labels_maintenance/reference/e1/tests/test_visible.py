import unittest
from labels import render_name, render_names


class VisibleBehavior(unittest.TestCase):
    def test_scalar(self):
        self.assertEqual(render_name("  Ada  "), "Ada")
        self.assertEqual(render_name("  "), "(unnamed)")

    def test_batch(self):
        names = [" Ada ", "", " Ada "]
        self.assertEqual(render_names(iter(names)), ["Ada", "(unnamed)", "Ada"])
        self.assertEqual(names, [" Ada ", "", " Ada "])
        self.assertEqual(render_names([]), [])


if __name__ == "__main__":
    unittest.main()
