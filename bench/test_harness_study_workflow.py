import base64
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study.artifacts import ArtifactStore, canonical_json
from bench.harness_study_workflow import analyze, observe, write_report


def events(*pairs):
    return [{"sequence": index, "channel": channel, "payload": {"episode": "e1", **payload}}
            for index, (channel, payload) in enumerate(pairs, 1)]


def raw(native):
    return "stdout", {"text": json.dumps(native)}


def model(kind, *, role="agent", request_id="one", **payload):
    return "model", {"event": kind, "role": role, "id": request_id, **payload}


def encoded(value):
    return base64.b64encode(value).decode()


class ObservationTests(unittest.TestCase):
    def test_raw_and_normalized_errors_are_not_added_together(self):
        observed = events(raw({"type": "tool_use", "part": {
            "tool": "read", "callID": "call1", "state": {"status": "error", "error": "bad arguments"}}}),
            ("native", {"error": "bad arguments", "event_type": "tool_use"}))
        result = observe(observed, {"backend": "opencode"}, {})
        self.assertEqual(result["native_error_observation_count"], 1)
        self.assertEqual(result["native_error_observations"][0]["source"]["sequence"], 1)
        self.assertIsNone(result["unavailable"]["tool_failure_rate"])

    def test_repeated_harness_snapshots_preserve_journal_identity(self):
        capture = {"message": "Capture rejected before persistence; invalid relation"}
        rejection = {"message": "Step rejected or interrupted: invalid relation"}
        state = {"type": "state", "task": {"id": "task1", "last_error": "invalid relation",
                                          "events": [capture, rejection]}}
        second_task = {"type": "state", "task": {**state["task"], "id": "task2"}}
        result = observe(events(raw(state), raw(state), raw(second_task)), {"backend": "harness"}, {})
        self.assertEqual(len(result["harness_state_error_observations"]), 2)
        self.assertEqual(result["harness_state_error_observations"][0]["source_sequences"], [1, 2])
        self.assertEqual(len(result["harness_journal_observations"]), 4)
        self.assertEqual(result["harness_journal_observations"][0]["journal_index"], 0)
        changed = {"type": "state", "task": {**state["task"], "events": [rejection]}}
        with self.assertRaisesRegex(ValueError, "journal changed"):
            observe(events(raw(state), raw(changed)), {"backend": "harness"}, {})

    def test_proxy_identity_sampling_and_split_sse_are_separate_from_completion(self):
        body = {"model": "local", "temperature": 0, "top_p": 1}
        stream = b'data: {"model":"local"}\n\ndata: [DONE]\n\n'
        result = observe(events(
            model("request_bytes", raw_base64=encoded(json.dumps(body).encode())),
            model("request", body=body),
            model("response_chunk", raw_base64=encoded(stream[:13])),
            model("response_chunk", raw_base64=encoded(stream[13:])),
            model("complete"),
            model("request", role="helper", body={"model": "wrong", "temperature": 0.5}),
            model("error", role="helper", error="unexpected model")),
            {"model": "local"}, {"helper_model": "helper", "generation_policy": {"local_temperature": 0}})
        self.assertEqual(len(result["proxy_requests"]), 2)
        agent, helper = result["proxy_requests"]
        self.assertTrue(agent["response_identity_matches"])
        self.assertTrue(agent["temperature_matches"])
        self.assertEqual(agent["event_sequences"]["request_bytes"], [1])
        self.assertFalse(helper["request_identity_matches"])
        self.assertFalse(helper["temperature_matches"])
        self.assertIsNone(helper["response_identity_matches"])
        self.assertFalse(helper["proxy_complete_observed"])
        self.assertEqual([stream["observed_request_identities"] for stream in result["proxy_streams"]], [1, 1])

    def test_missing_policy_malformed_request_and_partial_response_stay_unavailable(self):
        result = observe(events(
            model("request_bytes", raw_base64=encoded(b"not json")),
            model("error", error="invalid JSON"),
            model("request", request_id="two", body={"model": "local", "temperature": 0}),
            model("response_chunk", request_id="two", raw_base64=encoded(b'data: {"model":'))),
            {"model": "local"}, {})
        bad, partial = result["proxy_requests"]
        self.assertIsNone(bad["request_identity_matches"])
        self.assertIn("malformed", bad["request_body_status"])
        self.assertIsNone(partial["temperature_matches"])
        self.assertIsNone(partial["response_identity_matches"])

    def test_simulated_inputs_are_distinct_from_prompt_and_quit(self):
        result = observe(events(
            ("input", {"type": "input", "text": "do work"}),
            ("input", {"type": "input", "text": "/approve", "reason": "simulated approval"}),
            ("input", {"type": "input", "text": "/no-knowledge"}),
            ("input", {"type": "quit", "text": "/approve"})), {}, {})
        self.assertEqual(result["simulated_reviewer_input_count"], 2)
        self.assertEqual([item["source"]["sequence"] for item in result["simulated_reviewer_inputs"]], [2, 3])

    def test_rejected_nonfinite_wire_request_can_be_reported(self):
        result = observe(events(model("request_bytes", raw_base64=encoded(
            b'{"model":"local","temperature":NaN}'))), {"model": "local"}, {})
        self.assertIn("malformed", result["proxy_requests"][0]["request_body_status"])
        canonical_json(result)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.store = ArtifactStore(self.root / "evidence")

    def make_run(self, sealed=True):
        run = self.store.create_run({"backend": "harness", "model": "local"})
        self.store.append_event(run, "input", {"episode": "e1", "type": "input", "text": "/approve"})
        self.store.put_bytes(run, "outcome.json", canonical_json({"status": "agent_failure", "episodes": []}))
        if sealed:
            self.store.seal_run(run)
        return run

    def test_sealed_only_and_tampering_never_earns_observations(self):
        sealed, changed, unfinished = self.make_run(), self.make_run(), self.make_run(False)
        (changed / "events.jsonl").write_text("tampered\n")
        with patch("socket.socket", side_effect=AssertionError("network prohibited")), \
                patch("subprocess.Popen", side_effect=AssertionError("execution prohibited")):
            result = analyze(self.store.root)
        rows = {row["run_id"]: row for row in result["runs"]}
        self.assertEqual(rows[sealed.name]["observations"]["simulated_reviewer_input_count"], 1)
        self.assertEqual(rows[changed.name]["integrity"], "invalid")
        self.assertIsNone(rows[changed.name]["observations"])
        self.assertIsNone(rows[unfinished.name]["observations"])

    def test_report_preserves_sources_and_evidence_without_overwrite(self):
        run = self.make_run()
        before = {path.name: path.read_bytes() for path in run.iterdir()}
        output = write_report(self.store.root, self.root / "output")
        provenance = json.loads((output / "provenance.json").read_bytes())
        self.assertEqual(provenance["workflow_sha256"], hashlib.sha256((output / "workflow.json").read_bytes()).hexdigest())
        for name, metadata in provenance["source_files"].items():
            self.assertEqual(metadata["sha256"], hashlib.sha256((output / name).read_bytes()).hexdigest())
        replay = self.root / "replay"
        subprocess.run([sys.executable, "-m", "bench.harness_study_workflow",
                        "--store", str(self.store.root), "--output", str(replay)],
                       cwd=output / "sources", check=True, capture_output=True)
        self.assertEqual((output / "workflow.json").read_bytes(), (replay / "workflow.json").read_bytes())
        self.assertEqual(before, {path.name: path.read_bytes() for path in run.iterdir()})
        with self.assertRaises(FileExistsError):
            write_report(self.store.root, output)
        with self.assertRaisesRegex(ValueError, "outside"):
            write_report(self.store.root, self.store.root / "output")


if __name__ == "__main__":
    unittest.main()
