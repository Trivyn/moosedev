import json
from pathlib import Path
import ssl
import tempfile
import tomllib
import unittest
from unittest.mock import patch

from bench.harness_study import adapters


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        self.runtime = self.root / "runtime"
        self.binary = self.root / "native-client"
        self.binary.write_text("#!/bin/sh\nexit 0\n")
        self.binary.chmod(0o700)
        self.repo = self.root / "repository"
        self.daemon = self.repo / "target/harness-study/bin/frozen/moosedev"
        self.daemon.parent.mkdir(parents=True)
        self.daemon.write_text("#!/bin/sh\nexit 0\n")
        self.daemon.chmod(0o700)
        self.patch = patch.object(adapters, "REPOSITORY", self.repo)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def build(self, backend, **kwargs):
        values = dict(executable=self.binary, model="exact/model", workspace=self.workspace,
                      runtime=self.runtime, prompt="Keep $() and `text` literal")
        values.update(kwargs)
        return adapters.build_command(backend, **values)

    def test_codex_has_isolation_flags_and_no_bypass(self):
        command, env = self.build("codex")
        for flag in ("--ignore-user-config", "--ignore-rules", "--ephemeral", "--json"):
            self.assertIn(flag, command)
        self.assertEqual(command[command.index("--sandbox") + 1], "danger-full-access")
        self.assertEqual(command[command.index("--model") + 1], "exact/model")
        self.assertFalse(any("dangerously" in arg for arg in command))
        self.assertEqual(command[-2:], ["--", "Keep $() and `text` literal"])
        self.assertNotIn("PATH", env)
        self.assertNotIn("OPENAI_API_KEY", env)
        for key in ("HOME", "XDG_CONFIG_HOME", "CODEX_HOME", "TMPDIR"):
            self.assertTrue(Path(env[key]).is_relative_to(self.runtime))
        config = json.loads((self.runtime / "codex-overrides.json").read_text())
        self.assertEqual(config["mcp_servers"], {})
        self.assertEqual(config["approval_policy"], "never")
        self.assertEqual(config["model_reasoning_effort"], "medium")
        self.assertFalse(config["sandbox_workspace_write.network_access"])

    def test_codex_auth_is_preserved_without_copying_real_home(self):
        auth = self.runtime / "codex/auth.json"
        auth.parent.mkdir(parents=True)
        auth.write_text("opaque caller-managed credential")
        self.build("codex")
        self.assertEqual(auth.read_text(), "opaque caller-managed credential")
        self.assertFalse((self.runtime / "codex/config.toml").exists())

    def test_codex_copies_explicit_ca_into_private_runtime(self):
        public_ca = Path(ssl.get_default_verify_paths().cafile)
        for backend in ("codex", "codex_mcp"):
            with self.subTest(backend=backend):
                runtime = self.root / backend
                kwargs = {"daemon_exe": self.daemon, "daemon_socket": runtime / "daemon.sock"}
                _, env = self.build(backend, runtime=runtime, ca_bundle=public_ca, **kwargs)
                copied = Path(env["CODEX_CA_CERTIFICATE"])
                self.assertEqual(copied, runtime / "codex-ca.pem")
                self.assertEqual(copied.read_bytes(), public_ca.read_bytes())
                self.assertEqual(copied.stat().st_mode & 0o777, 0o400)
                self.build(backend, runtime=runtime, ca_bundle=public_ca, **kwargs)

    def test_codex_rejects_private_invalid_and_replaced_ca_files(self):
        source = self.root / "ca.pem"
        for content in (b"not a certificate", b"-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----",
                        b"-----BEGIN PRIVATE KEY-----\nsecret"):
            source.write_bytes(content)
            with self.assertRaisesRegex(ValueError, "CA bundle"):
                self.build("codex", ca_bundle=source)
        source.write_bytes(Path(ssl.get_default_verify_paths().cafile).read_bytes())
        destination = self.runtime / "codex-ca.pem"
        destination.symlink_to(source)
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.build("codex", ca_bundle=source)
        destination.unlink()
        destination.write_text("modified certificate")
        with self.assertRaisesRegex(ValueError, "already differs"):
            self.build("codex", ca_bundle=source)

    def test_mcp_uses_only_explicit_frozen_proxy_and_socket(self):
        socket = self.runtime / "daemon.sock"
        command, _ = self.build("codex_mcp", daemon_exe=self.daemon, daemon_socket=socket)
        overrides = [command[n + 1] for n, arg in enumerate(command) if arg == "-c"]
        parsed = tomllib.loads("\n".join(overrides))
        server = parsed["mcp_servers"]["moosedev"]
        self.assertEqual(server["command"], str(self.daemon))
        self.assertEqual(server["args"], ["--connect", str(socket)])
        self.assertEqual(server["env"]["MOOSEDEV_NO_AUTOSPAWN"], "1")
        self.assertEqual(server["env"]["MOOSEDEV_DATA_DIR"], str(self.workspace / ".moosedev"))
        self.assertEqual(set(parsed["mcp_servers"]), {"moosedev"})

    def test_no_path_or_homebrew_fallback(self):
        with self.assertRaises(ValueError):
            self.build("codex", executable=Path("codex"))
        with self.assertRaises(ValueError):
            self.build("codex_mcp", daemon_exe=self.binary, daemon_socket=self.runtime / "s")
        with self.assertRaises(ValueError):
            self.build("codex_mcp", daemon_exe=self.daemon)
        with self.assertRaises(FileNotFoundError):
            self.build("codex", executable=self.root / "missing")

    def test_opencode_pins_provider_and_disables_external_discovery(self):
        command, env = self.build("opencode", endpoint="http://127.0.0.1:1234/v1")
        config = json.loads(Path(env["OPENCODE_CONFIG"]).read_text())
        self.assertIn("--pure", command)
        self.assertEqual(command[command.index("--model") + 1], "study/exact/model")
        self.assertEqual(config["provider"]["study"]["models"]["exact/model"]["id"], "exact/model")
        self.assertEqual(config["small_model"], config["model"])
        self.assertEqual(config["default_agent"], "build")
        for name in ("build", "title", "summary", "compaction"):
            self.assertEqual(config["agent"][name]["temperature"], 0.0)
        self.assertIs(config["provider"]["study"]["models"]["exact/model"]["temperature"], True)
        self.assertEqual(config["mcp"], {})
        self.assertEqual(config["plugin"], [])
        self.assertEqual(config["permission"]["external_directory"], "deny")
        for key in ("OPENCODE_DISABLE_PROJECT_CONFIG", "OPENCODE_DISABLE_CLAUDE_CODE",
                    "OPENCODE_DISABLE_EXTERNAL_SKILLS", "OPENCODE_DISABLE_DEFAULT_PLUGINS"):
            self.assertEqual(env[key], "1")
        self.assertEqual(json.loads(env["OPENCODE_CONFIG_CONTENT"]), config)

    def test_bridge_configures_agent_separately_and_leaves_stdin_to_driver(self):
        command, env = self.build("harness", executable=self.daemon, daemon_exe=self.daemon,
                                  daemon_url="http://127.0.0.1:8000", endpoint="http://127.0.0.1:1234/v1",
                                  context_tokens=65536)
        self.assertEqual(command[command.index("--project") + 1], str(self.workspace))
        self.assertEqual(command[command.index("--model") + 1], "exact/model")
        self.assertEqual(command[command.index("--daemon-exe") + 1], str(self.daemon))
        self.assertNotIn("Keep $() and `text` literal", command)
        self.assertEqual(env["MOOSEDEV_LLM_MODEL"], "exact/model")
        self.assertEqual(env["MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS"], "65536")
        self.assertEqual(env["MOOSEDEV_HARNESS_RESPONSE_POLICY"], "auto")

    def test_harness_response_policy_is_explicit_and_validated(self):
        _, env = self.build("harness", executable=self.daemon, daemon_exe=self.daemon,
                            daemon_url="http://127.0.0.1:8000", endpoint="http://127.0.0.1:1234/v1",
                            harness_response_policy="reasoning-off")
        self.assertEqual(env["MOOSEDEV_HARNESS_RESPONSE_POLICY"], "reasoning-off")
        with self.assertRaisesRegex(ValueError, "response policy"):
            self.build("harness", harness_response_policy="unknown")

    def test_rejects_mutated_config_and_symlinks(self):
        self.build("codex")
        with self.assertRaises(ValueError):
            self.build("codex_mcp", daemon_exe=self.daemon, daemon_socket=self.runtime / "s")
        alias = self.root / "alias"
        alias.symlink_to(self.runtime, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.build("codex", runtime=alias)

    def test_rejects_credentials_and_invalid_configuration(self):
        with self.assertRaises(ValueError):
            self.build("opencode", endpoint="http://user:secret@localhost:1234/v1")
        with self.assertRaises(ValueError):
            self.build("opencode")
        with self.assertRaises(ValueError):
            self.build("codex", context_tokens=True)
        with self.assertRaises(ValueError):
            self.build("codex", runtime=self.workspace / "runtime")


class EventTests(unittest.TestCase):
    def test_unknown_fields_remain_unknown(self):
        result = adapters.normalize_event("codex", {"type": "thread.started"})
        for key in ("tokens", "read", "retrieval", "capture", "command", "assistant", "error"):
            self.assertIsNone(result[key])
        usage = adapters.normalize_event("codex", {"type": "turn.completed", "usage": {"input_tokens": 100}})
        self.assertEqual(usage["tokens"]["input"], 100)
        self.assertIsNone(usage["tokens"]["output"])
        self.assertIsNone(usage["tokens"]["cache_read"])

    def test_no_double_counting_cache_or_inferred_reads(self):
        usage = adapters.normalize_event("codex", {"type": "turn.completed", "usage": {
            "input_tokens": 100, "cached_input_tokens": 80, "output_tokens": 7}})
        self.assertEqual(usage["tokens"]["input"], 100)
        event = {"type": "item.completed", "item": {"type": "command_execution", "command": "cat a.py"}}
        result = adapters.normalize_event("codex", event)
        self.assertIsNotNone(result["command"])
        self.assertIsNone(result["read"])

    def test_only_observed_moosedev_tools_are_classified(self):
        event = {"type": "item.completed", "item": {"type": "mcp_tool_call", "tool": "query", "server": "other"}}
        self.assertIsNone(adapters.normalize_event("codex_mcp", event)["retrieval"])
        event["item"]["server"] = "moosedev"
        self.assertIsNotNone(adapters.normalize_event("codex_mcp", event)["retrieval"])
        event["item"]["tool"] = "record_important_decision"
        self.assertIsNotNone(adapters.normalize_event("codex_mcp", event)["capture"])

    def test_opencode_observations(self):
        read = adapters.normalize_event("opencode", {"type": "tool_use", "part": {"tool": "read"}})
        self.assertEqual(read["read"], {"tool": "read"})
        tokens = adapters.normalize_event("opencode", {"type": "step_finish", "part": {"tokens": {
            "input": 4, "output": 2, "cache": {"read": 3, "write": 1}}}})
        self.assertEqual(tokens["tokens"]["cache_write"], 1)
        self.assertIsNone(tokens["tokens"]["reasoning"])
        failure = adapters.normalize_event("opencode", {"type": "tool_use", "part": {
            "tool": "bash", "state": {"status": "error", "error": "failed"}}})
        self.assertEqual(failure["error"], "failed")

    def test_bridge_deltas_do_not_count_repeated_task_snapshots(self):
        state = adapters.normalize_event("harness", {"type": "state", "task": {"events": ["read"]}})
        self.assertIsNone(state["read"])
        result = adapters.normalize_event("harness", {"type": "progress", "kind": "assistant_delta", "text": "Hi"})
        self.assertEqual(result["assistant"], "Hi")
        error = adapters.normalize_event("harness", {"type": "error", "message": "failed"})
        self.assertEqual(error["error"], "failed")


if __name__ == "__main__":
    unittest.main()
