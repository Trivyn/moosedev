import base64
import json
from pathlib import Path
import tempfile
import unittest

from bench.harness_study.usage import UsageLedger, normalize, resource_metrics, replay
from bench.harness_study.proxy import usage_metadata


def chunk(data):
    return base64.b64encode(data).decode()


class UsageTests(unittest.TestCase):
    def setUp(self):
        self.ledger = UsageLedger("harness")
        self.events = []

    def model(self, kind, id="one", role="agent", **values):
        event = {"id": id, "event": kind, "episode": "e1", "role": role, "model": "test", **values}
        self.events.append({"channel": "model", "payload": event})
        self.ledger.consume("model", event)

    def start(self, id="one", stream=True, role="agent", metadata=None):
        self.model("request", id, role, body={"stream": stream}, usage_metadata=metadata or {})
        self.model("response_headers", id, role, status=200)

    def response(self, usage, id="one", role="agent", stream=True):
        raw = json.dumps({"usage": usage}).encode()
        if stream:
            raw = b"data: " + raw + b"\r\n\r\n"
        # Include a split inside the JSON payload to exercise HTTP fragmentation.
        self.model("response_chunk", id, role, raw_base64=chunk(raw[:7]))
        self.model("response_chunk", id, role, raw_base64=chunk(raw[7:]))

    def finish(self, id="one", role="agent", stream=True):
        if stream:
            self.model("response_chunk", id, role, raw_base64=chunk(b"data: [DONE]\n\n"))
        self.model("response_body_complete", id, role)
        self.model("complete", id, role, elapsed_seconds=1)

    def proxy(self):
        return self.ledger.report()["sources"]["proxy"]

    def test_malformed_content_and_native_state_never_change_accounting_flow(self):
        self.start(stream=False)
        self.model("response_chunk", raw_base64=chunk(b'{"choices":1,"usage":{"prompt_tokens":10}}'))
        self.finish(stream=False)
        self.assertEqual(resource_metrics(self.ledger.report())["input_tokens"], 10)
        for value in (1, [], {"requests": 5}, {"requests": [{"id": {"bad": 1}}]}):
            self.ledger.native({"type": "state", "task": {"token_usage": value}})

    def test_final_cumulative_snapshot_replaces_chunks_not_sums(self):
        self.start()
        self.response({"prompt_tokens": 100, "completion_tokens": 2})
        self.response({"prompt_tokens": 100, "completion_tokens": 5,
                       "prompt_tokens_details": {"cached_tokens": 80},
                       "completion_tokens_details": {"reasoning_tokens": 3}})
        self.finish()
        report = self.proxy()
        self.assertEqual(report["summary"]["requests"], 1)
        self.assertEqual(report["summary"]["fields"]["input_tokens"]["total"], 100)
        self.assertEqual(report["summary"]["fields"]["output_tokens"]["total"], 5)
        self.assertEqual(len(report["requests"][0]["usage_snapshots"]), 2)
        self.assertIsNone(report["summary"]["fields"]["cache_write_tokens"]["total"])

    def test_partial_last_snapshot_does_not_fill_fields_from_earlier_snapshot(self):
        self.start()
        self.response({"prompt_tokens": 10, "completion_tokens": 1})
        self.response({"completion_tokens": 5})
        self.finish()
        self.assertIsNone(resource_metrics(self.ledger.report())["input_tokens"])
        self.assertEqual(resource_metrics(self.ledger.report())["output_tokens"], 5)

    def test_missing_truncated_error_and_cancelled_requests_are_in_denominator(self):
        self.start("good")
        self.response({"prompt_tokens": 10, "completion_tokens": 3}, "good")
        self.finish("good")
        self.start("missing")
        self.finish("missing")
        self.start("error")
        self.response({"prompt_tokens": 20}, "error")
        self.model("error", "error", error="stream disconnected")
        self.start("cancel")
        self.model("shutdown", interrupted_handlers=1)
        report = self.proxy()
        self.assertEqual(report["summary"]["requests"], 4)
        self.assertEqual(report["summary"]["statuses"], {"completed": 2, "error": 1, "cancelled": 1})
        self.assertEqual(report["summary"]["fields"]["input_tokens"]["observed_sum"], 30)
        self.assertEqual(report["summary"]["fields"]["input_tokens"]["final_requests"], 1)
        self.assertIsNone(report["summary"]["fields"]["input_tokens"]["total"])

    def test_final_usage_before_bad_framing_remains_observed_only(self):
        self.start()
        self.response({"prompt_tokens": 10})
        self.model("response_chunk", raw_base64=chunk(b"data: {\"usage\":"))
        self.model("response_body_complete")
        self.model("error", error="incomplete framing")
        self.assertFalse(self.proxy()["requests"][0]["usage_final"])

    def test_http_error_body_retains_usage_and_separate_status(self):
        self.start(stream=False)
        self.model("response_headers", status=400)
        self.response({"prompt_tokens": 7}, stream=False)
        self.model("response_body_complete")
        self.model("error", error="provider returned HTTP 400")
        request = self.proxy()["requests"][0]
        self.assertTrue(request["usage_final"])
        self.assertEqual(request["tokens"]["input_tokens"], 7)
        self.assertEqual(request["status"], "error")

    def test_helper_probe_and_repair_are_distinct_physical_calls(self):
        for id, role, purpose, candidate in (("probe", "agent", "harness_response_probe", None),
                ("repair", "agent", "harness_capture", 2), ("fallback", "agent", "harness_capture", 2),
                ("helper", "helper", None, None)):
            self.start(id, stream=False, role=role, metadata={"purpose": purpose, "candidate_attempt": candidate})
            self.response({"prompt_tokens": 10}, id, role, stream=False)
            self.finish(id, role, stream=False)
        report = self.proxy()
        self.assertEqual(report["by_repair"]["repair"]["requests"], 2)
        self.assertEqual(report["by_purpose"]["harness_response_probe"]["requests"], 1)
        metrics = resource_metrics(self.ledger.report())
        self.assertEqual(metrics["input_tokens"], 30)
        self.assertEqual(metrics["helper_input_tokens"], 10)

    def test_evolution_purposes_keep_complete_separate_request_accounting(self):
        purposes = ("harness_capture_resolution", "harness_purpose_selection",
                    "harness_association_selection")
        for index, purpose in enumerate(purposes):
            identifier = f"evolution-{index}"
            self.start(identifier, stream=False, metadata={"purpose": purpose, "candidate_attempt": 1})
            self.response({"prompt_tokens": 10 + index, "completion_tokens": 2}, identifier, stream=False)
            self.finish(identifier, stream=False)
        by_purpose = self.proxy()["by_purpose"]
        self.assertEqual(set(purposes), set(by_purpose) & set(purposes))
        self.assertTrue(all(by_purpose[purpose]["fields"]["total_tokens"]["total"] is None
                            for purpose in purposes))
        self.assertEqual(self.proxy()["summary"]["fields"]["input_tokens"]["total"], 33)

    def test_repeated_harness_snapshots_enrich_not_double_count(self):
        self.start(metadata={"client_request_id": "client1", "purpose": "harness_action"})
        self.response({"prompt_tokens": 10})
        self.finish()
        for status in ("started", "completed", "completed"):
            self.ledger.native({"type": "state", "task": {"token_usage": {"requests": [
                {"id": "client1", "status": status, "tokens": {"prompt_tokens": 10}}]}}})
        self.assertEqual(self.proxy()["summary"]["requests"], 1)
        self.assertEqual(self.proxy()["requests"][0]["harness_receipt"]["status"], "completed")
        self.assertEqual(self.ledger.report()["sources"]["native"]["summary"]["requests"], 0)

    def test_codex_reasoning_and_missing_cache_write_remain_native(self):
        ledger = UsageLedger("codex_mcp")
        event = {"type": "turn.completed", "turn_id": "a", "usage": {"input_tokens": 100,
                 "cached_input_tokens": 80, "output_tokens": 9, "reasoning_output_tokens": 6}}
        ledger.native(event)
        ledger.native(event)
        metrics = resource_metrics(ledger.report())
        self.assertEqual(metrics["input_tokens"], 100)
        self.assertEqual(metrics["reasoning_tokens"], 6)
        self.assertIsNone(metrics["cache_write_tokens"])
        ledger.native({"type": "turn.failed", "turn_id": "b"})
        self.assertIsNone(resource_metrics(ledger.report())["input_tokens"])

    def test_stderr_failed_native_turn_counts_but_diagnostic_completion_does_not(self):
        ledger = UsageLedger("codex")
        for channel, event in (("stdout", {"type": "turn.completed", "turn_id": "a", "usage": {"input_tokens": 10}}),
                               ("stderr", {"type": "turn.failed", "turn_id": "b"}),
                               ("stderr", {"type": "turn.completed", "turn_id": "diagnostic", "usage": {"input_tokens": 99}})):
            ledger.consume(channel, {"raw_base64": chunk(json.dumps(event).encode()), "episode": "e1"})
        self.assertEqual(ledger.report()["sources"]["native"]["summary"]["requests"], 2)
        self.assertIsNone(resource_metrics(ledger.report())["input_tokens"])

    def test_opencode_preserves_disjoint_native_categories_but_proxy_is_authority(self):
        ledger = UsageLedger("opencode")
        ledger.native({"type": "step_finish", "part": {"id": "one", "tokens": {
            "input": 10, "output": 4, "reasoning": 2, "cache": {"read": 20, "write": 5}}}})
        request = ledger.report()["sources"]["native"]["requests"][0]
        self.assertEqual(request["tokens"]["input_tokens"], 10)
        self.assertEqual(request["tokens"]["cache_write_tokens"], 5)
        self.assertIsNone(resource_metrics(ledger.report())["input_tokens"])

    def test_invalid_numbers_unknown_and_headers_allowlisted(self):
        self.assertIsNone(normalize({"prompt_tokens": True}, "openai_chat")["input_tokens"])
        headers = {"x-moosedev-request-id": "id-123", "x-moosedev-purpose": "harness_action",
                   "x-moosedev-candidate": "2", "Authorization": "SECRET", "x-moosedev-decision-id": "bad\nvalue"}
        self.assertEqual(usage_metadata(headers), {"client_request_id": "id-123", "purpose": "harness_action", "candidate_attempt": 2})

    def test_multiline_sse_and_json_response_to_stream_request(self):
        self.start()
        self.model("response_chunk", raw_base64=chunk(
            b'data: {"usage":\n' b'data: {"prompt_tokens": 12, "total_tokens": 15}}\n\n'))
        self.finish()
        self.assertEqual(resource_metrics(self.ledger.report())["total_tokens"], 15)
        self.start("json")
        self.model("response_headers", "json", status=200, content_type="application/json; charset=utf-8")
        self.response({"prompt_tokens": 4}, "json", stream=False)
        self.finish("json", stream=False)
        self.assertEqual(resource_metrics(self.ledger.report())["input_tokens"], 16)

    def test_cache_write_native_and_provider_fields_are_not_zero_filled(self):
        self.assertEqual(normalize({"cache_write_input_tokens": 0}, "codex")["cache_write_tokens"], 0)
        self.assertEqual(normalize({"cache_creation_input_tokens": 7}, "openai_chat")["cache_write_tokens"], 7)
        self.assertIsNone(normalize({}, "codex")["cache_write_tokens"])

    def test_unfinished_native_turn_prevents_complete_totals(self):
        ledger = UsageLedger("codex")
        ledger.native({"type": "turn.started"})
        ledger.native({"type": "turn.completed", "usage": {"input_tokens": 10}})
        ledger.native({"type": "turn.started"})
        self.assertEqual(ledger.report()["sources"]["native"]["summary"]["requests"], 2)
        self.assertIsNone(resource_metrics(ledger.report())["input_tokens"])

    def test_native_journal_gap_is_disclosed_without_invalidating_observed_proxy_cost(self):
        self.start()
        self.response({"prompt_tokens": 10})
        self.finish()
        self.ledger.native({"type": "state", "task": {"id": "task1", "token_usage": {
            "requests": [], "legacy_gap": True, "persistence_errors": ["test disk failure"]}}})
        report = self.ledger.report()
        self.assertEqual(resource_metrics(report)["input_tokens"], 10)
        self.assertEqual(report["reconciliation"]["harness_journals"][0]["persistence_errors"], ["test disk failure"])

    def test_harness_receipt_without_proxy_prevents_false_complete_total(self):
        self.start()
        self.response({"prompt_tokens": 10})
        self.finish()
        self.ledger.native({"type": "state", "task": {"token_usage": {"requests": [
            {"id": "failed-before-proxy", "status": "failed"}]}}})
        self.assertIsNone(resource_metrics(self.ledger.report())["input_tokens"])
        self.assertEqual(self.ledger.report()["reconciliation"]["harness_receipts_without_proxy"], ["failed-before-proxy"])

    def test_sealed_offline_report_preserves_original_outcome_and_rejects_tamper(self):
        from unittest.mock import patch
        from bench.harness_study.artifacts import ArtifactStore, canonical_json
        from bench.harness_study.usage import report_store
        self.start(stream=False)
        self.response({"prompt_tokens": 10}, stream=False)
        self.finish(stream=False)
        with tempfile.TemporaryDirectory() as directory:
            store = ArtifactStore(Path(directory).resolve() / "evidence")
            run = store.create_run({"backend": "harness"})
            for event in self.events:
                store.append_event(run, event["channel"], event["payload"])
            old = canonical_json({"status": "success", "episodes": [{"id": "e1", "metrics": {"input_tokens": None}}]})
            store.put_bytes(run, "outcome.json", old)
            store.seal_run(run)
            with patch("socket.socket", side_effect=AssertionError("network prohibited")), \
                    patch("subprocess.Popen", side_effect=AssertionError("execution prohibited")):
                result = report_store(store.root)
                self.assertEqual(result, report_store(store.root))
            self.assertEqual(result["runs"][0]["integrity"], "sealed")
            self.assertEqual(resource_metrics(result["runs"][0]["request_usage"])["input_tokens"], 10)
            self.assertEqual((run / "outcome.json").read_bytes(), old)
            (run / "events.jsonl").write_text("tampered")
            self.assertEqual(report_store(store.root)["runs"][0]["integrity"], "invalid")

    def test_offline_replay_is_exact(self):
        self.start()
        self.response({"prompt_tokens": 10})
        self.finish()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text("".join(json.dumps(event) + "\n" for event in self.events))
            self.assertEqual(replay(path, "harness").report(), self.ledger.report())


if __name__ == "__main__":
    unittest.main()
