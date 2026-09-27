"""Prefill probe, against a fake server only."""
import unittest

from bench.harness_study import prefill_probe

SERVER = {"endpoint": "http://local/v1", "model": "m"}


def request(prompt, purpose="harness_action"):
    return {"purpose": purpose, "prompt": prompt, **SERVER}


class PrefillProbeTest(unittest.TestCase):
    def test_the_body_is_the_harness_request_with_one_output_token(self):
        tools = [{"type": "function", "function": {"name": "read"}}]
        with_tools = prefill_probe.body("m", "prompt", tools)
        self.assertEqual(with_tools["messages"], [{"role": "user", "content": "prompt"}])
        self.assertEqual((with_tools["tool_choice"], with_tools["parallel_tool_calls"]), ("required", False))
        self.assertEqual((with_tools["temperature"], with_tools["max_tokens"]), (0.0, 1))
        plain = prefill_probe.body("m", "prompt")
        self.assertNotIn("tools", plain)
        self.assertNotIn("tool_choice", plain)

    def test_the_median_pair_is_chosen_and_other_requests_break_pairs(self):
        base = "x" * 100
        task = {"model_requests": [
            request(base + "a"), request(base + "b" * 10), request("note", "harness_capture_note"),
            request(base + "c" * 50), request(base + "d" * 90), request(base + "e" * 20)]}
        pairs = prefill_probe.action_pairs(task)
        self.assertEqual([index for index, _, _ in pairs], [0, 3, 4])
        index, _, current = prefill_probe.choose_pair(task)
        # Uncached bytes 10, 90 and 20: the median is the pair costing 20.
        self.assertEqual(index, 4)
        self.assertEqual(current["prompt"], base + "e" * 20)

    def test_probe_times_each_variant_and_derives_rates(self):
        calls = []

        def fake(endpoint, payload, timeout=900):
            calls.append(payload)
            prompt = payload["messages"][0]["content"]
            tools = 0.5 if "tools" in payload else 0.0
            seconds = 1.0 + tools + (len(prompt) / 1000 if prompt != prefill_probe.FLUSH else 0)
            return seconds, 2000, 200

        task = {"model_requests": [request("p" * 3000), request("c" * 4000)]}
        result = prefill_probe.probe(task, [{"type": "function"}], repeat=1, sender=fake,
                                     streamer=lambda endpoint, payload: (3.0, 9.0, {"completion_tokens": 120}))
        self.assertEqual(result["pair"], 0)
        # 1 s fixed, 0.5 s tools, 1 s per 1,000 prompt characters (with the marker line).
        self.assertAlmostEqual(result["results"]["with_tools"]["cold"], 5.54, places=2)
        self.assertEqual(result["tool_overhead_seconds"]["cold"], 0.5)
        # Every prompt carries a fresh marker, so no earlier cache can serve it.
        prompts = [call["messages"][0]["content"] for call in calls]
        self.assertTrue(all(prompt.startswith("[probe ") for prompt in prompts))
        self.assertEqual(len({prompt.split("]")[0] for prompt in prompts[:1]}), 1)
        # Per variant set: cold, previous, warm, cached; with and without tools;
        # then the previous prompt before the streamed generate request.
        self.assertEqual(len(calls), 9)
        self.assertEqual(result["results"]["generate"]["seconds_per_token_after_first_chunk"], 0.05)


if __name__ == "__main__":
    unittest.main()
