import base64
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import validation


class ValidationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.workspace = self.root / "candidate"
        self.workspace.mkdir()
        (self.workspace / "service.py").write_text("VALUE = 1\n")
        self.hidden = self.root / "gold" / "test_hidden.py"
        self.hidden.parent.mkdir()
        self.hidden.write_text("import unittest\nif __name__ == '__main__': unittest.main()\n")

    def execute_fake(self, *, returncode=0, stderr=b"Ran 2 tests in 0.001s\n\nOK\n",
                     stdout=b"", timed_out=False, inspect=None):
        calls = {}

        def confinement(command, **kwargs):
            calls["command"] = command
            calls["sandbox"] = kwargs
            if inspect:
                inspect(command, kwargs)
            return command

        class FakeProcess:
            pid = 43210

            def __init__(self, command, **kwargs):
                calls["launch"] = kwargs
                kwargs["stdout"].write(stdout)
                kwargs["stderr"].write(stderr)
                self.waits = 0

            def wait(self, timeout=None):
                self.waits += 1
                if timed_out and self.waits == 1:
                    raise subprocess.TimeoutExpired("fixture", timeout)
                return -9 if timed_out else returncode

        with patch.object(validation, "sandbox_command", side_effect=confinement), \
                patch.object(validation, "_interpreter", return_value="/usr/bin/python3"), \
                patch.object(validation.subprocess, "Popen", FakeProcess), \
                patch.object(validation.os, "killpg") as kill:
            result = validation.execute_check(self.workspace, self.hidden)
            calls["kills"] = kill.call_count
        return result, calls

    def test_grades_only_source_copy_without_original_gold_or_credentials(self):
        (self.workspace / ".moosedev").mkdir()
        (self.workspace / ".moosedev" / "private.py").write_text("SECRET = True")
        (self.workspace / "PROJECT_NOTES.md").write_text("notes")
        (self.workspace / "__pycache__").mkdir()
        (self.workspace / "__pycache__" / "cache.py").write_text("cached")
        (self.workspace / "tests").mkdir()
        (self.workspace / "tests" / "test_visible.py").write_text("import unittest")

        def inspect(command, sandbox):
            workspace, runtime = sandbox["workspace"], sandbox["runtime"]
            self.assertNotEqual(workspace, self.workspace)
            self.assertEqual(sorted(p.relative_to(workspace).as_posix() for p in workspace.rglob("*.py")),
                             ["service.py", "tests/test_visible.py"])
            self.assertFalse((workspace / "PROJECT_NOTES.md").exists())
            self.assertEqual((runtime / "hidden_test.py").read_bytes(), self.hidden.read_bytes())
            self.assertEqual(sandbox["network_endpoints"], [])
            self.assertEqual(sandbox["readable_paths"], [])
            self.assertEqual(command[:4], ["/usr/bin/python3", "-I", "-S", "-B"])
            self.assertNotIn(str(self.hidden.parent), command)

        result, calls = self.execute_fake(inspect=inspect)
        self.assertEqual(result["status"], "success")
        self.assertTrue(result["passed"])
        self.assertEqual(result["tests_run"], 2)
        self.assertTrue(calls["launch"]["close_fds"])
        self.assertTrue(calls["launch"]["start_new_session"])
        self.assertEqual(set(calls["launch"]["env"]), {"PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"})
        self.assertEqual(calls["kills"], 1)
        self.assertEqual((self.workspace / "service.py").read_text(), "VALUE = 1\n")

    def test_retains_exact_bytes_and_distinguishes_failure_timeout_and_infrastructure(self):
        result, _ = self.execute_fake(returncode=1, stdout=b"partial\xff", stderr=b"Ran 2 tests in 0.1s\nFAILED (failures=1)\n")
        self.assertEqual(result["status"], "agent_failure")
        self.assertEqual(base64.b64decode(result["stdout_base64"]), b"partial\xff")
        timeout, calls = self.execute_fake(timed_out=True, stdout=b"before timeout")
        self.assertEqual(timeout["status"], "agent_failure")
        self.assertTrue(timeout["timed_out"])
        self.assertEqual(timeout["stdout"], "before timeout")
        self.assertEqual(calls["kills"], 1)
        infrastructure, _ = self.execute_fake(returncode=71, stderr=b"sandbox-exec: sandbox_apply: denied\n")
        self.assertEqual(infrastructure["status"], "infrastructure_failure")

    def test_zero_tests_and_silent_exit_are_not_success(self):
        for stderr in (b"", b"Ran 0 tests in 0.001s\nOK\n"):
            result, _ = self.execute_fake(stderr=stderr)
            self.assertFalse(result["passed"])
            self.assertEqual(result["status"], "infrastructure_failure")

    def test_missing_or_linked_inputs_fail_before_execution(self):
        link = self.workspace / "aliased.py"
        link.symlink_to(self.hidden)
        with patch.object(validation.subprocess, "Popen") as launch:
            result = validation.execute_check(self.workspace, self.hidden)
            launch.assert_not_called()
        self.assertEqual(result["status"], "infrastructure_failure")
        self.assertIn("symlink", result["error"])
        link.unlink()
        self.hidden.unlink()
        with patch.object(validation.subprocess, "Popen") as launch:
            result = validation.execute_check(self.workspace, self.hidden)
            launch.assert_not_called()
        self.assertEqual(result["status"], "infrastructure_failure")

    def test_no_sandbox_means_no_unconfined_fallback(self):
        with patch.object(validation, "sandbox_command", side_effect=RuntimeError("unsupported platform")), \
                patch.object(validation.subprocess, "Popen") as launch:
            result = validation.execute_check(self.workspace, self.hidden)
            launch.assert_not_called()
        self.assertEqual(result["status"], "infrastructure_failure")

    def test_all_reference_and_negative_cases_reported_without_running_models(self):
        def result(workspace, **kwargs):
            negative = "moosedev-negative-" in str(workspace)
            return {"status": "agent_failure" if negative else "success", "passed": not negative,
                    "tests_run": 2, "timed_out": False, "returncode": 1 if negative else 0,
                    "stdout": "", "stderr": "retained result"}

        with patch.object(validation, "_execute", side_effect=result), \
                patch.object(validation, "execute_check", side_effect=lambda workspace, hidden: result(workspace)):
            report = validation.validate_fixtures()
        self.assertTrue(report["passed"])
        self.assertEqual(report["case_count"], 18)
        self.assertEqual(sum(case["kind"] == "negative_hidden" for case in report["cases"]), 6)
        self.assertTrue(all(case["result"]["stderr"] == "retained result" for case in report["cases"]))

    def test_infrastructure_failure_does_not_count_as_a_working_negative_control(self):
        failure = {"status": "infrastructure_failure", "tests_run": None, "timed_out": False,
                   "passed": False, "returncode": None, "stdout": "", "stderr": "denied"}
        with patch.object(validation, "_execute", return_value=failure), \
                patch.object(validation, "execute_check", return_value=failure):
            report = validation.validate_fixtures()
        self.assertFalse(report["passed"])
        self.assertTrue(all(not case["passed"] for case in report["cases"]))


if __name__ == "__main__":
    unittest.main()
