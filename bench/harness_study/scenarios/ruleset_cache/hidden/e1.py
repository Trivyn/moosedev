import os
import sys
import unittest
sys.path.insert(0, os.getcwd())

from rules import Ruleset
from service import ScoringService

class CacheTests(unittest.TestCase):
    def make_service(self):
        calls = []
        def evaluate(value, ruleset):
            calls.append((ruleset.name, ruleset.revision, value))
            return value * ruleset.multiplier
        return ScoringService(Ruleset("a", 1, 3), evaluator=evaluate), calls

    def test_reuses_result_including_zero(self):
        service, calls = self.make_service()
        for value in [4, 4, 0, 0, -2, -2]:
            self.assertEqual(service.score(value), value * 3)
        self.assertEqual(calls, [("a", 1, 4), ("a", 1, 0), ("a", 1, -2)])

    def test_evaluator_exception_is_not_cached(self):
        calls = []
        def fail_once(value, ruleset):
            calls.append(value)
            if len(calls) == 1:
                raise ValueError("transient computation failure")
            return 7
        service = ScoringService(Ruleset("a", 1, 3), evaluator=fail_once)
        with self.assertRaises(ValueError):
            service.score(1)
        self.assertEqual(service.score(1), 7)
        self.assertEqual(service.score(1), 7)
        self.assertEqual(len(calls), 2)

if __name__ == "__main__":
    unittest.main()
