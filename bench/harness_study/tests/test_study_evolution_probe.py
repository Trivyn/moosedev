"""Negative checks for the unscored discovery readiness proof."""
import copy
import unittest

from bench.harness_study.evolution_probe import _pages


class StubDaemon:
    def __init__(self, responses):
        self.responses = iter(responses)

    def _request(self, _route, _payload):
        response = next(self.responses)
        if isinstance(response, Exception):
            raise response
        return response


class EvolutionProbeTests(unittest.TestCase):
    def test_failed_request_remains_available_for_reproduction(self):
        receipts = []
        with self.assertRaisesRegex(RuntimeError, "offline"):
            _pages(StubDaemon([RuntimeError("offline")]), "/intent/purpose/candidates",
                   {"objective": "preserve behavior", "files": ["labels.py"]}, receipts)
        self.assertEqual(receipts, [{"request": {
            "objective": "preserve behavior", "files": ["labels.py"], "cursor": None, "limit": 1}}])

    def test_purpose_pages_must_preserve_snapshot_and_choice_identity(self):
        pages = [{"revision": "snapshot", "candidates": [{"iri": "record-a", "handle": "a"}],
                  "next_cursor": "next"},
                 {"revision": "snapshot", "candidates": [{"iri": "record-b", "handle": "b"}],
                  "next_cursor": None}]
        receipts = []
        self.assertEqual(len(_pages(StubDaemon(pages), "/intent/purpose/candidates", {}, receipts)), 2)
        for change, message in (({"revision": "changed"}, "snapshot"),
                                ({"candidates": [{"iri": "record-a", "handle": "b"}]}, "identity"),
                                ({"candidates": [{"iri": "record-b", "handle": "a"}]}, "handle")):
            with self.subTest(change=change):
                altered = copy.deepcopy(pages)
                altered[1].update(change)
                with self.assertRaisesRegex(RuntimeError, message):
                    _pages(StubDaemon(altered), "/intent/purpose/candidates", {}, [])

    def test_continuation_must_not_refresh_or_switch_index(self):
        first = {"knowledge_revision": "knowledge", "scope_digest": "scope",
                 "index": {"revision": "index", "status": "current", "refresh_action": "refreshed"},
                 "candidates": [{"id": "one"}], "next_cursor": "next"}
        second = copy.deepcopy(first)
        second.update(candidates=[{"id": "two"}], next_cursor=None)
        second["index"]["refresh_action"] = "not_requested"
        request = {"refresh_policy": "supported_frozen"}
        self.assertEqual(len(_pages(StubDaemon([first, second]), "/intent/candidates", request, [])), 2)
        for field, value in (("revision", "different"), ("refresh_action", "refreshed")):
            with self.subTest(field=field):
                altered = copy.deepcopy(second)
                altered["index"][field] = value
                with self.assertRaises(RuntimeError):
                    _pages(StubDaemon([first, altered]), "/intent/candidates", request, [])


if __name__ == "__main__":
    unittest.main()
