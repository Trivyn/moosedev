"""Prefix-cache reuse report, on a scripted journal only."""
import json
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


    def test_sections_split_and_rejoin_the_prompt(self):
        prompt = ("You are the coding sensor. r\nProject rules (1)\nrule\n"
                  "\nEntity dossiers:\n[d]\nCurrent source, refreshed x\n{}\n"
                  "Repository paths (2)\na\n\nLast result:\nok")
        parts = prefix_reuse.sections(prompt)
        self.assertEqual([name for name, _ in parts],
                         ["role and guidance", "project rules", "entity dossiers", "source",
                          "repository paths", "last result"])
        self.assertEqual("".join(text for _, text in parts), prompt)

    def test_a_marker_inside_a_file_is_not_a_section(self):
        prompt = ('You are the coding sensor.\nCurrent source, refreshed x\n'
                  '{"notes.md":"Repository paths (a heading)"}\n'
                  'Entity dossiers:\n[d]\nRepository paths (1)\na')
        names = [name for name, _ in prefix_reuse.sections(prompt)]
        self.assertEqual(names, ["role and guidance", "source", "entity dossiers", "repository paths"])

    def test_a_reorder_moves_only_the_named_section(self):
        prompt = ("You are the coding sensor.\nEntity dossiers:\n[d]\n"
                  "Current source, refreshed x\n{}\nRepository paths (2)\na")
        moved = prefix_reuse.reorder(prompt, "entity dossiers", "source")
        self.assertEqual(moved, "You are the coding sensor.Current source, refreshed x\n{}\n"
                                "\nEntity dossiers:\n[d]\nRepository paths (2)\na")
        self.assertEqual(prefix_reuse.reorder(prompt, "observations", "source"), prompt)

    def test_a_dossier_change_after_source_keeps_the_source_cached(self):
        source = "Current source, refreshed x\n" + "s" * 1000 + "\n"
        first = "You are the coding sensor.\nEntity dossiers:\n[one]\n" + source + "Repository paths (1)\na"
        second = "You are the coding sensor.\nEntity dossiers:\n[two]\n" + source + "Repository paths (1)\na"
        as_is = prefix_reuse.report(journal(request(first), request(second)))
        moved = prefix_reuse.report(journal(request(first), request(second)),
                                    ("entity dossiers", "source"))
        self.assertEqual(as_is["first_divergence"], {"entity dossiers": 1})
        self.assertGreater(as_is["uncached_bytes_per_step"], 1000)
        self.assertLess(moved["uncached_bytes_per_step"], 50)

    def test_churn_counts_changed_sections_and_shows_the_rules_change(self):
        head = "You are the coding sensor.\nProject rules (2)\nkeep\n"
        first = head + "old rule\n\nLast result:\na"
        second = head + "new rule\n\nLast result:\nb"
        result = prefix_reuse.churn(journal(request(first), request(second)))
        self.assertEqual(result["changed_pairs"], {"project rules": 1, "last result": 1})
        example = result["rules_examples"][0]
        self.assertEqual((example["before"], example["after"]), ("old rule", "new rule"))

    def test_latency_splits_step_time_into_prefill_and_generation(self):
        # Built as 2 s fixed + 1 s per uncached KB + 0.05 s per token.
        shape = [(1000, 10), (3000, 40), (500, 80), (2000, 20), (4000, 60)]
        stable = "You are the coding sensor. " + "x" * 4000
        prompts = [request(stable + str(step) + "y" * tail) for step, (tail, _) in enumerate(shape)]
        task = journal(*prompts)
        uncached, previous = [], None
        for item in prompts:
            current = item["prompt"].encode()
            shared = prefix_reuse.common_prefix(previous, current) if previous else 0
            uncached.append(len(current) - shared)
            previous = current
        receipts = []
        for step, ((_, tokens), bytes_) in enumerate(zip(shape, uncached)):
            receipts.append({"context": {"purpose": "harness_action"}, "status": "completed",
                             "started_at": f"2026-09-26T00:00:0{step}",
                             "elapsed_ms": round(1000 * (2 + bytes_ / 1000 + 0.05 * tokens)),
                             "tokens": {"completion_tokens": tokens}})
        # A transport retry's receipt, with no journal entry, is not joined.
        receipts.append({"context": {"purpose": "harness_action"}, "status": "failed",
                         "started_at": "2026-09-26T00:00:00.5", "elapsed_ms": 900,
                         "tokens": {"completion_tokens": None}})
        report = prefix_reuse.latency(task, receipts)
        self.assertAlmostEqual(report["seconds_fixed_per_step"], 2, delta=0.05)
        self.assertAlmostEqual(report["seconds_per_uncached_kb"], 1, delta=0.01)
        self.assertAlmostEqual(report["seconds_per_completion_token"], 0.05, delta=0.001)

    def test_flips_count_changes_of_the_full_source_set(self):
        def action(full, budget=None):
            prompt = "You are the coding sensor.\nCurrent source, refreshed x\n" + json.dumps(
                {name: "text" for name in full}) + "\n"
            entry = request(prompt)
            if budget is not None:
                entry.update({"source_full": sorted(full), "source_budget": budget})
            return entry
        # From the prompts alone (older journals): one flip in two pairs.
        old = prefix_reuse.flips(journal(action(["a", "b"]), action(["a", "b"]), action(["a"])))
        self.assertEqual(old, {"steps": 2, "flips": 1, "flips_with_budget_change": None})
        # From the journal's records, with the budget behind each flip.
        new = prefix_reuse.flips(journal(action(["a", "b"], 100), action(["a"], 90), action(["b"], 90)))
        self.assertEqual(new, {"steps": 2, "flips": 2, "flips_with_budget_change": 1})


if __name__ == "__main__":
    unittest.main()
