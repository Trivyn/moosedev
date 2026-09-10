import unittest
from rules import Ruleset
from service import ScoringService

class VisibleTests(unittest.TestCase):
    def test_scores_integer(self):
        self.assertEqual(ScoringService(Ruleset("basic", 1, 3)).score(4), 12)

if __name__ == "__main__":
    unittest.main()
