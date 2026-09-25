"""Prefix-cache reuse report, on a scripted journal only."""
import unittest

from bench.harness_study import prefix_reuse

SERVER = {"endpoint": "http://local/v1", "model": "m"}


def request(prompt, purpose="harness_action", **server):
    return {"purpose": purpose, "prompt": prompt, **(server or SERVER)}


def journal(*requests):
    return {"model_requests": list(requests)}


class PrefixReuseTest(unittest.TestCase):
    def test_reuse_counts_the_shared_prefix_in_bytes(self):
        stable = "You are the coding sensor. λ Project rules (x)\n"
        first = stable + "Recent observations (a)\nLast result:\none"
        second = stable + "Recent observations (a)\nLast result:\ntwo!"
        result = prefix_reuse.report(journal(request(first), request(second)))
        shared = len((stable + "Recent observations (a)\nLast result:\n").encode())
        size = len(second.encode())
        self.assertEqual(result["pairs"], 1)
        self.assertEqual(result["uncached_bytes_per_step"], size - shared)
        self.assertEqual(result["reuse_percent"], round(100 * shared / size, 1))
        self.assertEqual(result["first_divergence"], {"last result": 1})

    def test_an_action_is_compared_with_the_request_just_before_it(self):
        # The server holds only its last prompt: a capture note in between is
        # what the next action reuses from, not the previous action.
        result = prefix_reuse.report(journal(
            request("You are the coding sensor. a"),
            request("Write one capture note.", purpose="harness_capture_note"),
            request("You are the coding sensor. a")))
        self.assertEqual(result["pairs"], 1)
        self.assertEqual(result["reuse_percent"], 0.0)

    def test_a_request_to_another_model_breaks_the_pair(self):
        result = prefix_reuse.report(journal(
            request("You are the coding sensor. a"),
            request("You are the coding sensor. a", endpoint="http://local/v1", model="other")))
        self.assertEqual(result["pairs"], 0)
        self.assertEqual(result["after_other_server"], 1)
        self.assertIsNone(result["reuse_percent"])


if __name__ == "__main__":
    unittest.main()
