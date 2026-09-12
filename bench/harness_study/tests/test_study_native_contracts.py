import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import native_contracts
from bench.harness_study.artifacts import sha256_file


def receipt():
    return {"passed": True, "attempts": [
        {"name": name, "passed": True} for name in (
            "association_associate", "association_none_applies", "purpose_select", "purpose_done",
            "purpose_missing_empty")]}


class QuietProxy:
    def __init__(self, upstream, model, record, role, **kwargs):
        self.url = "http://127.0.0.1:49123/v1"
        self.failures = []
        self.failure_details = []
        self.record = record

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


class NativeContractPreflightTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.binary = self.root / "session"
        self.binary.write_bytes(b"frozen executable")
        self.binaries = {"binaries": {"session": str(self.binary)},
                         "binary_hashes": {"session": sha256_file(self.binary)}, "build_id": "frozen"}
        self.config = {"endpoint": "http://127.0.0.1:1234/v1", "context_tokens": 32768,
                       "harness_response_policy": "auto", "local_models": [{"id": "first"}, {"id": "second"}]}

    def test_requires_all_successful_distinct_contracts(self):
        native_contracts.validate_receipt(receipt())
        for mutate in (
            lambda value: value["attempts"].pop(),
            lambda value: value["attempts"][0].update(passed=False),
            lambda value: value["attempts"][0].update(name=value["attempts"][1]["name"]),
            lambda value: value["attempts"][0].update(name="unrelated_schema"),
        ):
            value = receipt()
            mutate(value)
            with self.assertRaises(ValueError):
                native_contracts.validate_receipt(value)

    @patch.object(native_contracts, "ModelProxy", QuietProxy)
    def test_preserves_failure_bytes_and_does_not_continue_to_second_model(self):
        failure = subprocess.CompletedProcess([], 1, b'{"passed":false,"attempts":[]}',
                                              b'provider rejects schema keyword')
        with patch.object(native_contracts.subprocess, "run", return_value=failure) as run:
            with self.assertRaisesRegex(ValueError, "evidence:"):
                native_contracts.probe_native_contracts(self.config, self.binaries,
                                                       output_root=self.root / "probes")
        self.assertEqual(run.call_count, 1)
        path = next((self.root / "probes").glob("*/receipt.json"))
        evidence = json.loads(path.read_text())
        self.assertFalse(evidence["passed"])
        self.assertEqual((path.parent / "0/stderr.txt").read_bytes(), failure.stderr)
        self.assertEqual((path.parent / "0/stdout.json").read_bytes(), failure.stdout)
        self.assertFalse((path.parent / "1").exists())

    @patch.object(native_contracts, "ModelProxy", QuietProxy)
    def test_uses_frozen_binary_explicit_models_and_clean_environment(self):
        complete = subprocess.CompletedProcess([], 0, json.dumps(receipt()).encode(), b"")
        with patch.object(native_contracts.subprocess, "run", return_value=complete) as run:
            result = native_contracts.probe_native_contracts(self.config, self.binaries,
                                                            output_root=self.root / "probes")
        self.assertTrue(result["passed"])
        self.assertEqual(run.call_count, 2)
        for call, model in zip(run.call_args_list, ("first", "second")):
            self.assertEqual(call.args[0][:4], [str(self.binary), "--probe-intent-contracts", "--model", model])
            self.assertEqual(call.kwargs["env"]["MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS"], "32768")
            self.assertNotIn("MOOSEDEV_DATA_DIR", call.kwargs["env"])
            self.assertEqual(call.kwargs["timeout"], 570)

    def test_changed_executable_is_rejected_before_any_generation(self):
        self.binary.write_bytes(b"changed")
        with patch.object(native_contracts.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "executable changed"):
                native_contracts.probe_native_contracts(self.config, self.binaries,
                                                       output_root=self.root / "probes")
        run.assert_not_called()

    @patch.object(native_contracts, "ModelProxy", QuietProxy)
    def test_valid_contract_with_semantic_mismatch_is_retained_without_excluding_model(self):
        value = receipt()
        value["attempts"][2].update(semantic_match=False, semantic_error="expected select, observed done")
        complete = subprocess.CompletedProcess([], 0, json.dumps(value).encode(), b"")
        with patch.object(native_contracts.subprocess, "run", return_value=complete) as run:
            result = native_contracts.probe_native_contracts(self.config, self.binaries,
                                                            output_root=self.root / "probes")
        self.assertTrue(result["passed"])
        self.assertEqual(run.call_count, 2)
        for model in result["models"]:
            self.assertFalse(model["receipt"]["attempts"][2]["semantic_match"])


if __name__ == "__main__":
    unittest.main()
