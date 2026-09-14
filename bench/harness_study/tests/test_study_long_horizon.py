"""Long-horizon scenario groundwork: schema 2 loading, probe rules, per-test grading, isolation."""
import copy
import json
from pathlib import Path
import platform
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import config, indexing, intent, long_horizon, validation
from bench.harness_study.scenario import list_scenarios, load_scenario

INTENT_DESIGN_SHA256 = "f28f83ca29deff64f2009817a34ac59f50347e939dff423ec5388811321f236f"


def probe(identifier, kind, test, decided, facts=("f-rule",)):
    value = {"id": identifier, "kind": kind, "test": test, "fact_ids": list(facts),
             "decided_by": f"scenario.json#/episodes/{decided}/prompt"}
    if kind == "retention":
        value["measures"] = "correctness"
    return value


def episode(number, probes, *, expected=("f-rule",), stale=(), retired=()):
    return {"id": f"e{number}", "prompt": f"Episode {number} task.", "clarifications": {"scope": "stdlib"},
            "expected_fact_ids": list(expected), "stale_fact_ids": list(stale),
            "visible_checks": ["python3 -m unittest discover -s tests -v"],
            "hidden_test": f"hidden/e{number}.py", "reference": f"reference/e{number}",
            "allowed_paths": ["**/*.py"], "probes": probes, "retired_tests": list(retired)}


def long_package():
    scenario = {
        "schema_version": 2, "id": "sample", "track": "accumulation", "title": "Sample", "horizon": "long",
        "initial_facts": [], "source_mapping": "SOURCE_MAPPING.md", "dependency_map": "DEPENDENCY_MAP.md",
        "episodes": [
            episode(1, [probe("e1-task", "task", "Tests.test_one", 0)], expected=("f-rule", "f-keep")),
            episode(2, [probe("e2-task", "task", "Tests.test_two", 1),
                        probe("e2-keep", "retention", "Tests.test_keep", 0, ("f-keep",))],
                    expected=("f-rule", "f-keep")),
            episode(3, [probe("e3-task", "task", "Tests.test_three", 2, ("f-new",)),
                        probe("e3-keep", "retention", "Tests.test_keep_again", 0, ("f-keep",))],
                    expected=("f-new", "f-keep"), stale=("f-rule",), retired=("Tests.test_keep",)),
            episode(4, [probe("e4-task", "task", "Tests.test_four", 3, ("f-new",)),
                        probe("e4-now", "currency", "Tests.test_now", 2, ("f-new",))],
                    expected=("f-new", "f-keep"), stale=("f-rule",)),
        ],
        "negative_checks": [
            {"id": f"n-{pid}", "episode": ep, "base_reference": f"reference/{ep}", "overlay": f"negative/{pid}",
             "expected": "fail", "visible": "pass", "fails_probes": [pid]}
            for ep, pid in (("e2", "e2-keep"), ("e3", "e3-keep"), ("e4", "e4-now"))],
    }
    gold = {"schema_version": 2, "scenario_id": "sample", "review_status": "pending",
            "facts": [{"id": "f-rule", "claim": "Old rule.", "kind": "Constraint", "evidence": [],
                       "introduced_episode": 1, "seeded": False, "superseded_episode": 3, "superseded_by": "f-new"},
                      {"id": "f-keep", "claim": "Kept rule.", "kind": "Constraint", "evidence": [],
                       "introduced_episode": 1, "seeded": False},
                      {"id": "f-new", "claim": "New rule.", "kind": "ArchitecturalDecision", "evidence": [],
                       "introduced_episode": 3, "seeded": False}],
            "forbidden_claims": [{"claim": "The old rule is current.", "applies_from_episode": 3,
                                  "applies_through_episode": None}]}
    return scenario, gold


def write_package(directory, scenario, gold):
    root = Path(directory) / scenario["id"]
    for relative in ("project/m.py", "SOURCE_MAPPING.md", "DEPENDENCY_MAP.md",
                     *(f"reference/e{n}/m.py" for n in range(1, 6)), *(f"hidden/e{n}.py" for n in range(1, 6)),
                     *(n["overlay"] + "/m.py" for n in scenario["negative_checks"])):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("class Service:\n    def run(self):\n        return 1\n")
    (root / "scenario.json").write_text(json.dumps(scenario))
    (root / "gold.json").write_text(json.dumps(gold))
    return root


class LoaderTests(unittest.TestCase):
    def load(self, scenario, gold):
        with tempfile.TemporaryDirectory() as directory:
            write_package(directory, scenario, gold)
            return load_scenario(scenario["id"], Path(directory))

    def rejects(self, message, mutate):
        scenario, gold = long_package()
        mutate(scenario, gold)
        with self.assertRaisesRegex(ValueError, message):
            self.load(scenario, gold)

    def test_valid_package_loads_with_hashes(self):
        loaded = self.load(*long_package())
        self.assertEqual(len(loaded["episodes"]), 4)
        self.assertEqual(len(loaded["package_sha256"]), 64)

    def test_unknown_fields_are_rejected_everywhere(self):
        self.rejects("unknown long-horizon scenario", lambda s, g: s.update(extra=1))
        self.rejects("unknown long-horizon episode", lambda s, g: s["episodes"][0].update(extra=1))
        self.rejects("unknown probe", lambda s, g: s["episodes"][0]["probes"][0].update(extra=1))
        self.rejects("unknown negative", lambda s, g: s["negative_checks"][0].update(extra=1))
        self.rejects("unknown gold fact", lambda s, g: g["facts"][0].update(extra=1))
        self.rejects("unknown forbidden", lambda s, g: g["forbidden_claims"][0].update(extra=1))

    def test_episode_count_and_horizon(self):
        self.rejects("4 or 5", lambda s, g: s["episodes"].pop())
        self.rejects("long horizon", lambda s, g: s.update(horizon="pilot"))

    def test_probe_kinds_are_decided_where_required(self):
        self.rejects("required task", lambda s, g: s["episodes"][1]["probes"].pop(0))
        self.rejects("required task", lambda s, g: s["episodes"][3]["probes"].pop(1)
                     or s["negative_checks"].pop())
        self.rejects("not decided", lambda s, g: s["episodes"][3]["probes"][1].update(
            decided_by="scenario.json#/episodes/3/prompt"))
        self.rejects("not decided", lambda s, g: s["episodes"][1]["probes"][1].update(
            decided_by="scenario.json#/episodes/1/prompt"))
        self.rejects("not decided", lambda s, g: s["episodes"][1]["probes"][0].update(
            decided_by="scenario.json#/episodes/0/prompt"))
        self.rejects("duplicate or unknown", lambda s, g: s["episodes"][1]["probes"][1].update(kind="memory"))
        self.rejects("Class.test_method", lambda s, g: s["episodes"][1]["probes"][1].update(test="keep"))
        self.rejects("measure correctness or cost", lambda s, g: s["episodes"][1]["probes"][1].pop("measures"))
        self.rejects("measure correctness or cost", lambda s, g: s["episodes"][1]["probes"][1].update(measures="speed"))
        self.rejects("measure correctness or cost", lambda s, g: s["episodes"][1]["probes"][0].update(measures="cost"))
        self.rejects("measure correctness or cost", lambda s, g: s["episodes"][3]["probes"][1].update(measures="cost"))

    def test_every_retention_and_currency_probe_needs_a_negative(self):
        self.rejects("need a negative", lambda s, g: s["negative_checks"].pop(0))
        self.rejects("own episode", lambda s, g: s["negative_checks"][0].update(fails_probes=["e4-now"]))

    def test_supersession_moves_facts_to_stale(self):
        self.rejects("misclassifies", lambda s, g: s["episodes"][2]["stale_fact_ids"].clear())
        self.rejects("misclassifies", lambda s, g: s["episodes"][1]["stale_fact_ids"].append("f-rule")
                     or s["episodes"][1]["expected_fact_ids"].remove("f-rule"))
        self.rejects("future fact", lambda s, g: s["episodes"][0]["expected_fact_ids"].append("f-new"))
        self.rejects("episode range", lambda s, g: g["forbidden_claims"][0].update(applies_from_episode=9))

        def scoped(scenario, gold):
            gold["facts"][0]["current_scope"] = "invoices due before day 1000"
            for later in scenario["episodes"][2:]:
                later["stale_fact_ids"].clear()
                later["expected_fact_ids"].append("f-rule")
        self.load(*(lambda pair: (scoped(*pair), pair)[1])(long_package()))

    def test_retired_tests_must_come_from_the_previous_episode(self):
        self.rejects("retires", lambda s, g: s["episodes"][3]["retired_tests"].append("Tests.test_one"))

    def test_tracks_bind_seeding(self):
        self.rejects("seed", lambda s, g: s.update(track="inherited"))


class ResultParserTests(unittest.TestCase):
    def test_both_python_formats_and_colour(self):
        output = ("test_one (__main__.Tests) ... ok\n"
                  "test_two (__main__.Tests.test_two) ... \x1b[31mFAIL\x1b[0m\n"
                  "test_three (__main__.Tests) ... ERROR\n"
                  "FAIL: test_two (__main__.Tests.test_two)\n")
        self.assertEqual(validation.test_results(output),
                         {"Tests.test_one": "ok", "Tests.test_two": "FAIL", "Tests.test_three": "ERROR"})

    def test_duplicate_results_are_an_error(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            validation.test_results("test_one (__main__.T) ... ok\ntest_one (__main__.T.test_one) ... ok\n")

    @unittest.skipUnless(platform.system() == "Darwin" and Path("/usr/bin/python3").is_file(), "system Python sandbox")
    def test_real_sandboxed_run_is_parsed(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory).resolve()
            workspace = directory / "workspace"
            workspace.mkdir()
            (workspace / "m.py").write_text("VALUE = 1\n")
            hidden = directory / "hidden.py"
            hidden.write_text("import os, sys, unittest\nsys.path.insert(0, os.getcwd())\nimport m\n"
                              "class Tests(unittest.TestCase):\n"
                              "    def test_ok(self):\n        self.assertEqual(m.VALUE, 1)\n"
                              "    def test_bad(self):\n        self.assertEqual(m.VALUE, 2)\n"
                              "if __name__ == '__main__':\n    unittest.main(verbosity=2)\n")
            result = validation.execute_check(workspace, hidden)
        self.assertEqual(validation.test_results(result["stderr"]), {"Tests.test_ok": "ok", "Tests.test_bad": "FAIL"})


class OfflineResolutionTests(unittest.TestCase):
    def resolve(self, source, name):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "m.py").write_text(source)
            return indexing.resolve_offline(directory, {"file": "m.py", "name": name})

    def test_exactly_one_definition(self):
        self.assertEqual(self.resolve("class A:\n    def b(self):\n        pass\n", "A.b")["line"], 2)
        self.assertEqual(self.resolve("def f():\n    pass\n", "f")["line"], 1)
        for source, name in (("class A:\n    pass\n", "A.b"),
                             ("def _b(self):\n    pass\nclass A:\n    b = _b\n", "A.b"),
                             ("class A:\n    def b(self):\n        pass\n    def b(self):\n        pass\n", "A.b")):
            with self.assertRaisesRegex(RuntimeError, "exactly one"):
                self.resolve(source, name)

    def test_long_horizon_tables_are_separate_from_the_sealed_overlay(self):
        self.assertEqual(intent.design_identity()["sha256"], INTENT_DESIGN_SHA256)
        for name in (*long_horizon.SCENARIOS, *long_horizon.EXPLORATORY):
            self.assertNotIn(name, intent.RESOLUTION_TARGETS)
            self.assertEqual(indexing.resolution_tables(name),
                             (long_horizon.RESOLUTION_TARGETS[name], long_horizon.SEED_ASSOCIATIONS[name]))
        self.assertEqual(indexing.resolution_tables("retry_ledger"), (intent.RESOLUTION_TARGETS["retry_ledger"], []))


class TrackTests(unittest.TestCase):
    def test_an_empty_starting_graph_follows_the_track_not_the_package_name(self):
        from bench.harness_study.scenario import starts_empty
        self.assertTrue(starts_empty({"id": "anything", "track": "accumulation"}))
        self.assertFalse(starts_empty({"id": "retry_ledger", "track": "inherited"}))
        for name, expected in (("retry_ledger", True), ("ruleset_cache", False), ("display_labels_maintenance", False),
                               ("supplier_quotes", True), ("entity_outbox", True), ("late_fees", False)):
            with self.subTest(name=name):
                self.assertEqual(starts_empty(load_scenario(name)), expected)


class IsolationTests(unittest.TestCase):
    def test_pilot_consumers_never_see_long_horizon_packages(self):
        self.assertEqual(list_scenarios(), ["display_labels_maintenance", "retry_ledger", "ruleset_cache"])
        self.assertEqual(list_scenarios(horizon="long"), sorted((*long_horizon.SCENARIOS, *long_horizon.EXPLORATORY)))
        self.assertEqual(long_horizon.EXPLORATORY, ("late_fees_crowded",))
        self.assertFalse(set(long_horizon.EXPLORATORY) & set(long_horizon.SCENARIOS))
        for name in (*long_horizon.SCENARIOS, *long_horizon.EXPLORATORY):
            self.assertEqual(load_scenario(name)["horizon"], "long")
        old = {"seed": 1, "frontier_model": "frontier", "local_models": [{"id": m} for m in ("a", "b", "c")]}
        self.assertEqual(len(config.schedule(old)), 16)
        self.assertEqual(len(config.schedule(dict(old, evaluation_mode="local-harness-development"))), 6)
        self.assertEqual(set(config.approval_payload("fixture")["scenarios"]), {"ruleset_cache", "retry_ledger"})
        with self.assertRaisesRegex(ValueError, "horizon"):
            list_scenarios(horizon="forever")


class LongValidationTests(unittest.TestCase):
    """Probe-level grading of a long package, with the sandbox replaced by scripted outputs."""

    def run_validation(self, outputs):
        scenario, gold = long_package()
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory).resolve()
            root = write_package(directory, scenario, gold)
            for number in range(1, 5):
                (root / f"reference/e{number}/tests").mkdir(exist_ok=True)

            def fake(workspace, hidden=None, **kwargs):
                if hidden is None and "hidden_test" in kwargs:
                    hidden = kwargs["hidden_test"]
                lines = outputs(str(workspace), None if hidden is None else Path(hidden).stem)
                failed = any(not line.endswith("... ok") for line in lines)
                return {"status": "agent_failure" if failed else "success", "passed": not failed,
                        "tests_run": max(len(lines), 1), "timed_out": False, "returncode": int(failed),
                        "stdout": "", "stderr": "\n".join(lines) + "\n"}

            with patch.object(validation, "SCENARIOS", Path(directory)),                     patch.object(validation, "load_scenario", lambda name: load_scenario(name, Path(directory))),                     patch.dict(long_horizon.RESOLUTION_TARGETS, {"sample": [{"file": "m.py", "name": "Service.run"}]}),                     patch.object(validation, "_execute", side_effect=lambda w, **k: fake(w, **k)),                     patch.object(validation, "execute_check", side_effect=lambda w, h, **k: fake(w, h)):
                return validation.validate_fixtures(["sample"])

    @staticmethod
    def probe_tests(stem):
        scenario, _ = long_package()
        return [p["test"] for e in scenario["episodes"] if e["id"] == stem for p in e["probes"]]

    def scripted(self, workspace, stem, *, extra_negative_failure=False):
        if stem is None:
            return ["test_visible (__main__.Visible) ... ok"]
        tests = self.probe_tests(stem)
        failing = set()
        if workspace.endswith("/project"):
            failing = set(tests)
        elif "moosedev-negative-" in workspace:
            failing = {tests[1]} | ({tests[0]} if extra_negative_failure else set())
        return [f"{t.split('.')[1]} (__main__.{t.split('.')[0]}) ... {'FAIL' if t in failing else 'ok'}" for t in tests]

    def test_all_probe_cases_pass_for_a_consistent_package(self):
        report = self.run_validation(lambda w, s: self.scripted(w, s))
        self.assertTrue(report["passed"], [c["id"] for c in report["cases"] if not c["passed"]])
        kinds = {case["kind"] for case in report["cases"]}
        self.assertEqual(kinds, {"offline_target", "reference_visible", "reference_probes", "project_hidden",
                                 "chain", "negative_visible", "negative_probes"})
        self.assertEqual(sum(c["kind"] == "chain" for c in report["cases"]), 3)

    def test_negative_failing_more_than_it_declares_is_not_a_valid_control(self):
        report = self.run_validation(lambda w, s: self.scripted(w, s, extra_negative_failure=True))
        failed = {case["id"] for case in report["cases"] if not case["passed"]}
        self.assertEqual(failed, {"n-e2-keep", "n-e3-keep", "n-e4-now"})

    def test_chain_ignores_only_retired_tests(self):
        def outputs(workspace, stem):
            lines = self.scripted(workspace, stem)
            if workspace.endswith("reference/e3") and stem == "e2":
                return [line.replace("test_keep (__main__.Tests) ... ok", "test_keep (__main__.Tests) ... FAIL")
                        for line in lines]
            if workspace.endswith("reference/e4") and stem == "e3":
                return [line.replace("... ok", "... FAIL") for line in lines]
            return lines
        report = self.run_validation(outputs)
        failed = {case["id"] for case in report["cases"] if not case["passed"]}
        self.assertEqual(failed, {"e4/chain/e3"})


if __name__ == "__main__":
    unittest.main()
