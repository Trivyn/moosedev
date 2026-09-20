from contextlib import ExitStack
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import run as runner
from bench.harness_study.artifacts import ArtifactStore, sha256_file
from bench.harness_study.grading import report
from bench.harness_study.scenario import tree_manifest


class RunTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "repo"
        (self.repo / "bench/harness_study").mkdir(parents=True)
        (self.repo / "bench/harness_study/driver.py").write_text("# frozen driver fixture\n")
        (self.repo / "spec").mkdir()
        (self.repo / "spec/harness_evaluation_protocol.md").write_text("frozen fixture protocol\n")
        self.scenarios = self.root / "scenarios"
        project = self.scenarios / "fixture/project"
        project.mkdir(parents=True)
        (project / "service.py").write_text("EPISODE = 0\n")
        hidden = self.scenarios / "fixture/hidden"
        hidden.mkdir()
        for episode in ("e1", "e2", "e3"):
            (hidden / f"{episode}.py").write_text("# grading is mocked\n")
        self.scenario = {"id": "fixture", "track": "accumulation", "gold_sha256": "a" * 64,
                         "package_sha256": "b" * 64, "initial_facts": [],
                         "episodes": [{"id": f"e{n}", "prompt": f"Implement episode {n}",
                                       "clarifications": {"scope": "fixture only"},
                                       "visible_checks": ["python3 -m unittest discover -s tests"],
                                       "hidden_test": f"hidden/e{n}.py"} for n in (1, 2, 3)]}
        self.build = self.root / "private-build"
        self.build.mkdir()
        binaries = {}
        for role in ("daemon", "harness", "session"):
            path = self.build / role
            path.write_text(f"fixture {role}")
            binaries[role] = str(path)
        (self.build / "engine-source.tar.gz").write_bytes(b"private engine archive")
        self.binary_manifest = {"directory": str(self.build), "binaries": binaries, "build_id": "fixture-build",
                                "binary_hashes": {role: sha256_file(Path(path)) for role, path in binaries.items()}}
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.config = {"study_id": "test-study", "endpoint": "http://127.0.0.1:1234/v1",
                       "helper_model": "helper-exact", "context_tokens": 32768,
                       "episode_seconds": 1200, "local_models": [], "hosted_endpoints": [],
                       "gold_approval": str(self.root / "approval.json"),
                       "binary_manifest": str(self.build / "manifest.json")}
        self.frozen = {"config": self.config, "ready": True, "binaries": self.binary_manifest,
                       "assets": {"directory": str(self.assets), "files": {}}}
        for role in ("codex", "opencode", "lms"):
            path = self.root / role
            path.write_text(f"fixture {role}")
            self.config[role] = str(path)
            self.frozen[role] = {"path": str(path), "sha256": sha256_file(path)}
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        self.cell = {"scenario_id": "fixture", "model": "agent-exact", "backend": "harness",
                     "condition": "harness", "repetition": 1}
        self.executions, self.grants, self.observed = [], [], []
        self.observed_guards = []
        self.archive_calls = 0
        self.model_load_calls = 0
        self.command_arguments = []

    def test_development_runs_refuse_original_store_and_non_harness_cells(self):
        self.config["evaluation_mode"] = "local-harness-development"
        with patch.object(runner, "REPO", self.repo):
            with self.assertRaisesRegex(ValueError, "separate evidence store"):
                runner.run_cell(self.repo / "target/harness-study/evidence", self.frozen, self.cell)
            self.assertFalse((self.repo / "target/harness-study/evidence").exists())
            with self.assertRaisesRegex(ValueError, "local harness cells"):
                runner.run_cell(self.root / "separate", self.frozen, dict(self.cell, backend="opencode"))

    def test_development_run_archives_addendum_and_works_without_frontier_clients(self):
        self.config.update(evaluation_mode="local-harness-development", harness_response_policy="auto",
                           parent_pilot={"study_id": "parent", "config_sha256": "parent-config"})
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        for role in ("codex", "opencode"):
            self.frozen.pop(role)
        (self.repo / "bench/harness_study/DEVELOPMENT.md").write_text("development-only protocol\n")
        result = self._run()
        self.assertEqual(result["status"], "success")
        retained = Path(result["path"])
        self.assertEqual((retained / "development-protocol.md").read_text(), "development-only protocol\n")
        manifest = json.loads((retained / "manifest.json").read_text())
        self.assertEqual(manifest["evaluation_mode"], "local-harness-development")
        self.assertEqual(manifest["parent_pilot"]["study_id"], "parent")
        self.assertEqual(self.command_arguments[0][1]["harness_response_policy"], "auto")

    def _run(self, *, fail_episode=None, approval_error=None, snapshot_failure=False,
             fail_setup_episode=None, interrupt_episode=None, fail_checkpoint_episode=None):
        create_temp = tempfile.mkdtemp
        original_snapshot = runner.snapshot
        verified_clients, phases = set(), []

        def temporary(**kwargs):
            kwargs["dir"] = self.root
            path = create_temp(**kwargs)
            self.executions.append(Path(path))
            return path

        class Proxy:
            def __init__(self, *args, **kwargs):
                self.url, self.failures = "http://127.0.0.1:4567/v1", []

            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

        class Daemon:
            def __init__(self, **kwargs):
                self.url = "http://127.0.0.1:7654"
                self.socket = kwargs["runtime"] / "daemon.sock"
                self.identity = {"pid": 12345, "sha256": kwargs["expected_sha256"]}
                self.log = kwargs["log_path"]
                self.episode_id = kwargs["runtime"].name

            def __enter__(self):
                if self.episode_id == fail_setup_episode:
                    raise RuntimeError("fixture daemon setup failure")
                self.log.parent.mkdir(parents=True, exist_ok=True)
                self.log.write_text("owned daemon fixture\n")
                return self

            def __exit__(self, *args):
                return False

            def checkpoint(self):
                if self.episode_id == fail_checkpoint_episode:
                    raise RuntimeError("fixture checkpoint failure")
                return {"published": True}

        def command(backend, **kwargs):
            self.command_arguments.append((backend, kwargs))
            if backend.startswith("codex"):
                (kwargs["runtime"] / "codex").mkdir()
            return [str(kwargs["executable"]), "fixture"], {}

        def load_models(config, cell, record, *, executable):
            self.model_load_calls += 1
            self.assertEqual(executable, self.frozen["lms"]["path"])
            self.assertEqual(phases[-1], "archived")

        def fingerprint(path):
            verified_clients.add(str(path))
            return sha256_file(path)

        def archive_clients(store, run, identities):
            expected = [self.frozen[role] for role in runner.required_clients(self.config)]
            self.assertEqual(identities, expected)
            self.assertTrue({identity["path"] for identity in expected} <= verified_clients)
            self.archive_calls += 1
            phases.append("archived")

        def sandbox(command, **kwargs):
            self.grants.append(kwargs)
            return command

        def observe(command, **kwargs):
            workspace, episode = kwargs["workspace"], kwargs["episode"]
            number = int(episode["id"][1:])
            self.assertEqual((workspace / "service.py").read_text(), f"EPISODE = {number - 1}\n")
            self.assertFalse((workspace / ".moosedev/harness").exists(), "native history leaked into fresh episode")
            if number > 1:
                self.assertEqual((workspace / "PROJECT_NOTES.md").read_text(), f"remember episode {number - 1}\n")
                self.assertIn(f"graph episode {number - 1}", (workspace / ".moosedev/kg.nq").read_text())
            self.observed.append(episode["id"])
            self.observed_guards.append((kwargs["evidence_byte_limit"], kwargs["reject_loop_limit"]))
            (workspace / "service.py").write_text(f"EPISODE = {number}\n")
            (workspace / "PROJECT_NOTES.md").write_text(f"remember episode {number}\n")
            (workspace / ".moosedev").mkdir(exist_ok=True)
            with (workspace / ".moosedev/kg.nq").open("a") as graph:
                graph.write(f"# graph episode {number}\n")
            history = workspace / ".moosedev/harness/conversations"
            history.mkdir(parents=True)
            (history / "session.json").write_text(json.dumps({"episode": number}))
            if episode["id"] == interrupt_episode:
                kwargs["record"]("native", {"event": "progress", "episode_number": number})
                raise KeyboardInterrupt("fixture native observation interrupted")
            kwargs["record"]("native", {"event": "completed", "episode_number": number})
            return {"status": "agent_failure" if episode["id"] == fail_episode else "success",
                    "metrics": {"elapsed_seconds": 1, "output_tokens": None}}

        def snapshot(*args, **kwargs):
            prefix = args[3]
            if snapshot_failure and prefix in {"episodes/e1/workspace", "interrupted-workspace"}:
                raise ValueError("fixture snapshot failure; do not discard execution")
            return original_snapshot(*args, **kwargs)

        with ExitStack() as stack:
            replacements = {"SCENARIOS": self.scenarios, "REPO": self.repo,
                            "load_scenario": lambda name: self.scenario,
                            "verify_binaries": lambda path: self.binary_manifest,
                            "sha256_file": fingerprint, "archive_clients": archive_clients,
                            "load_models": load_models, "ModelProxy": Proxy, "DomainProxy": Proxy, "OwnedDaemon": Daemon,
                            "build_command": command, "sandbox_command": sandbox, "observe": observe,
                            "snapshot": snapshot,
                            "execute_check": lambda *args: {"passed": True, "status": "success", "tests_run": 1,
                                                           "stdout": "", "stderr": "", "returncode": 0}}
            for name, value in replacements.items():
                stack.enter_context(patch.object(runner, name, value))
            stack.enter_context(patch.object(runner, "verify_approval", side_effect=approval_error))
            stack.enter_context(patch.object(runner.tempfile, "mkdtemp", side_effect=temporary))
            stack.enter_context(patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"", b"")))
            result = runner.run_cell(self.root / "evidence", self.frozen, self.cell)
        ArtifactStore(self.root / "evidence").verify_run(Path(result["path"]))
        return result

    def test_baseline_native_cell_runs_without_overlay_index_or_daemon(self):
        from bench.harness_study import evolution, intent
        self.config.update(evaluation_mode=evolution.STAGE2_BASELINE_MODE, episode_limit=1,
                           scenario_ids=list(intent.SCENARIOS), intent_policies=["current", "change-level-v2"],
                           local_models=[{"id": model} for model in intent.MODELS],
                           evidence_byte_limit=evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                           reject_loop_limit=evolution.BASELINE_REJECT_LOOP_LIMIT,
                           seed=1, evolution_design=evolution.design_identity(evolution.STAGE2_BASELINE_MODE))
        # A baseline preflight fingerprints opencode and lms only; no codex identity exists.
        self.config.pop("codex")
        self.frozen.pop("codex")
        for index in range(len(self.config["local_models"])):
            weights = self.root / f"weights-{index}"
            weights.mkdir()
            (weights / "model.safetensors").write_text("fixture weights")
            self.frozen[f"local_model_{index}"] = {"weights": str(weights), "files": tree_manifest(weights)}
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        self.frozen["schedule"] = runner.schedule(self.config)
        self.cell = next(cell for cell in self.frozen["schedule"] if cell["backend"] == "opencode")
        self.assertEqual((self.cell["condition"], self.cell["intent_policy"]), ("without", None))
        self.scenario["id"] = self.cell["scenario_id"]
        (self.scenarios / self.cell["scenario_id"]).symlink_to(self.scenarios / "fixture", target_is_directory=True)
        for name in ("verify_indexer", "apply_overlay", "index_workspace", "ready_dossiers"):
            self.assertNotIn(name, dir(runner))
        with patch.dict("sys.modules", {"bench.harness_study.indexing": None}):
            result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(self.observed, ["e1"])
        # The frozen baseline guards reach the observer from the configuration.
        self.assertEqual(self.observed_guards, [(8 * 1024 ** 3, 5)])
        self.assertEqual([(episode["status"], episode.get("reason")) for episode in result["episodes"]],
                         [("success", None), ("unattempted", "episode_limit"), ("unattempted", "episode_limit")])
        self.assertIn("evolution_constituents", result["episodes"][0])
        self.assertNotIn("intent_primary", result["episodes"][0])
        backend, arguments = self.command_arguments[0]
        self.assertEqual(backend, "opencode")
        self.assertNotIn("harness_intent_policy", arguments)
        self.assertTrue(arguments["postedit_association_contract"])
        self.assertEqual(len(self.grants), 1)
        saved = Path(result["path"])
        self.assertTrue((saved / "evolution-design.json").is_file())
        self.assertFalse((saved / "intent-design.json").exists())
        self.assertFalse((saved / "episodes/e1/prepared").exists())
        self.assertFalse((saved / "episodes/e1/daemon.log").exists())
        self.assertIn("PROJECT_NOTES.md", {path.name for path in (saved / "initial").iterdir()})
        manifest = json.loads((saved / "manifest.json").read_text())
        self.assertIsNone(manifest["intent_policy"])
        self.assertEqual(manifest["evaluation_mode"], evolution.STAGE2_BASELINE_MODE)
        with self.assertRaisesRegex(ValueError, "exact frozen schedule cell"):
            runner.run_cell(self.root / "other", self.frozen, dict(self.cell, model="substitute"))

    def _symbolic_config(self):
        from bench.harness_study import evolution, intent
        self.config.update(evaluation_mode=evolution.SYMBOLIC_BASELINE_MODE, episode_limit=1,
                           scenario_ids=list(intent.SCENARIOS), intent_policies=["symbolic"],
                           local_models=[{"id": model} for model in intent.MODELS],
                           evidence_byte_limit=evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                           reject_loop_limit=evolution.BASELINE_REJECT_LOOP_LIMIT,
                           seed=1, evolution_design=evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE))
        self.config.pop("codex")
        self.frozen.pop("codex")
        for index in range(len(self.config["local_models"])):
            weights = self.root / f"weights-{index}"
            weights.mkdir()
            (weights / "model.safetensors").write_text("fixture weights")
            self.frozen[f"local_model_{index}"] = {"weights": str(weights), "files": tree_manifest(weights)}
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        self.frozen["schedule"] = runner.schedule(self.config)

    def test_symbolic_native_cell_runs_without_overlay_index_or_daemon(self):
        from bench.harness_study import evolution
        self._symbolic_config()
        self.cell = next(cell for cell in self.frozen["schedule"] if cell["backend"] == "opencode")
        self.assertEqual((self.cell["condition"], self.cell["intent_policy"]), ("without", None))
        self.scenario["id"] = self.cell["scenario_id"]
        (self.scenarios / self.cell["scenario_id"]).symlink_to(self.scenarios / "fixture", target_is_directory=True)
        with patch.dict("sys.modules", {"bench.harness_study.indexing": None}):
            result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertEqual(self.observed_guards, [(8 * 1024 ** 3, 5)])
        self.assertEqual([(episode["status"], episode.get("reason")) for episode in result["episodes"]],
                         [("success", None), ("unattempted", "episode_limit"), ("unattempted", "episode_limit")])
        self.assertIn("evolution_constituents", result["episodes"][0])
        backend, arguments = self.command_arguments[0]
        self.assertEqual(backend, "opencode")
        self.assertNotIn("harness_intent_policy", arguments)
        # No post-edit probe contract exists under the symbolic harness.
        self.assertFalse(arguments["postedit_association_contract"])
        saved = Path(result["path"])
        self.assertTrue((saved / "evolution-design.json").is_file())
        self.assertFalse((saved / "intent-design.json").exists())
        self.assertFalse((saved / "episodes/e1/daemon.log").exists())
        self.assertIn("PROJECT_NOTES.md", {path.name for path in (saved / "initial").iterdir()})
        manifest = json.loads((saved / "manifest.json").read_text())
        self.assertIsNone(manifest["intent_policy"])
        self.assertEqual(manifest["evaluation_mode"], evolution.SYMBOLIC_BASELINE_MODE)
        with self.assertRaisesRegex(ValueError, "frozen arms"):
            runner.run_cell(self.root / "other", dict(self.frozen, schedule=self.frozen["schedule"]
                            + [dict(self.cell, condition="harness")]), dict(self.cell, condition="harness"))

    def test_symbolic_harness_cell_sends_the_symbolic_policy_without_a_postedit_contract(self):
        from types import SimpleNamespace
        from bench.harness_study import evolution, intent
        self._symbolic_config()
        self.cell = next(cell for cell in self.frozen["schedule"] if cell["backend"] == "harness")
        self.assertEqual((self.cell["condition"], self.cell["intent_policy"]), ("harness", "symbolic"))
        self.scenario["id"] = self.cell["scenario_id"]
        (self.scenarios / self.cell["scenario_id"]).symlink_to(self.scenarios / "fixture", target_is_directory=True)
        (self.repo / "spec/harness_intent_pilot.md").write_text("frozen fixture intent protocol\n")
        indexer_dir = self.root / "indexer"
        indexer_dir.mkdir()
        (indexer_dir / "scip-python").write_text("#!/bin/sh\n")
        indexer = {"directory": str(indexer_dir), "launcher": {"path": str(indexer_dir / "scip-python")}}
        self.config["indexer_manifest"] = str(indexer_dir / "manifest.json")
        self.binary_manifest["indexer"] = indexer
        self.frozen.update(indexer=indexer, intent_design=intent.design_identity(), indexer_probe={"ok": True},
                           indexer_system_python={"path": "/usr/bin/python3"})
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        self.frozen["schedule"] = runner.schedule(self.config)
        self.cell = next(cell for cell in self.frozen["schedule"] if cell["backend"] == "harness")
        calls = []
        fake_indexing = SimpleNamespace(
            verify_indexer=lambda manifest: indexer,
            apply_overlay=lambda workspace: calls.append("overlay"),
            index_workspace=lambda *args: {"indexed": True},
            ready_dossiers=lambda daemon, scenario, **kwargs: {"ready": True},
            system_python_identity=lambda frozen: frozen)
        with patch.dict("sys.modules", {"bench.harness_study.indexing": fake_indexing}):
            result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(calls, ["overlay"])
        backend, arguments = self.command_arguments[0]
        self.assertEqual(backend, "harness")
        self.assertEqual(arguments["harness_intent_policy"], "symbolic")
        self.assertFalse(arguments["postedit_association_contract"])
        saved = Path(result["path"])
        self.assertTrue((saved / "evolution-design.json").is_file())
        self.assertTrue((saved / "intent-design.json").is_file())
        self.assertTrue((saved / "episodes/e1/daemon.log").is_file())
        manifest = json.loads((saved / "manifest.json").read_text())
        self.assertEqual(manifest["intent_policy"], "symbolic")
        self.assertEqual(manifest["evaluation_mode"], evolution.SYMBOLIC_BASELINE_MODE)

    def test_capture_study_mcp_cell_is_indexed_and_served_but_never_overlaid(self):
        """The deferred third arm, end to end on stubs: no model, no GPU.

        The MCP arm must get the daemon and the code index -- without them
        get_entity_dossier and link_code resolve nothing -- but never the
        harness overlay, which is a property of that runner rather than of
        having memory.
        """
        from types import SimpleNamespace
        from bench.harness_study import capture_study, evolution, intent
        self._symbolic_config()
        design = capture_study.design_identity(["gemma-4-31b-it"], ["retry_ledger"], repetitions={"T1": 1})
        self.config.update(evaluation_mode=capture_study.MODE, scenario_ids=["retry_ledger"],
                           capture_study_design=design)
        (self.repo / "docs").mkdir(parents=True, exist_ok=True)
        capture_study.DOCUMENT.write_bytes(capture_study.DOCUMENT.read_bytes())
        indexer_dir = self.root / "indexer-capture"
        indexer_dir.mkdir()
        (indexer_dir / "scip-python").write_text("#!/bin/sh\n")
        indexer = {"directory": str(indexer_dir), "launcher": {"path": str(indexer_dir / "scip-python")}}
        self.config["indexer_manifest"] = str(indexer_dir / "manifest.json")
        self.binary_manifest["indexer"] = indexer
        self.frozen.update(indexer=indexer, intent_design=intent.design_identity(), indexer_probe={"ok": True},
                           indexer_system_python={"path": "/usr/bin/python3"})
        calls = []
        fake_indexing = SimpleNamespace(
            verify_indexer=lambda manifest: indexer,
            apply_overlay=lambda workspace: calls.append("overlay"),
            index_workspace=lambda *args: calls.append("index") or {"indexed": True},
            ready_dossiers=lambda daemon, scenario, **kwargs: calls.append("dossiers") or {"ready": True},
            system_python_identity=lambda frozen: frozen)
        with patch.object(capture_study, "verify_config", return_value=design):
            self.frozen["config_sha256"] = runner.configuration_hash(self.config)
            self.frozen["schedule"] = runner.schedule(self.config)
            self.cell = next(cell for cell in self.frozen["schedule"]
                             if cell["backend"] == "opencode_mcp")
            self.assertEqual((self.cell["condition"], self.cell["intent_policy"]),
                             ("opencode_mcp", None))
            self.scenario["id"] = self.cell["scenario_id"]
            (self.scenarios / self.cell["scenario_id"]).symlink_to(
                self.scenarios / "fixture", target_is_directory=True)
            with patch.dict("sys.modules", {"bench.harness_study.indexing": fake_indexing}):
                result = self._run()
        self.assertEqual(result["status"], "success", result)
        # Indexed and given dossiers, but never overlaid.
        self.assertIn("index", calls)
        self.assertIn("dossiers", calls)
        self.assertNotIn("overlay", calls)
        backend, arguments = self.command_arguments[0]
        self.assertEqual(backend, "opencode_mcp")
        self.assertIsNotNone(arguments["daemon_socket"])
        self.assertIsNotNone(arguments["daemon_exe"])
        # No harness-only policy is sent to a client that has no such concept.
        self.assertNotIn("harness_intent_policy", arguments)
        saved = Path(result["path"])
        self.assertTrue((saved / "episodes/e1/daemon.log").is_file())
        # The pre-registration travels with the evidence it decides.
        self.assertTrue((saved / "capture-study-design.json").is_file())
        self.assertTrue((saved / "capture-study-protocol.md").is_file())
        manifest = json.loads((saved / "manifest.json").read_text())
        self.assertEqual(manifest["evaluation_mode"], capture_study.MODE)
        self.assertEqual(manifest["backend"], "opencode_mcp")

    def _field_check_config(self):
        from bench.harness_study import evolution, field_check, model_table
        models, scenarios = ["gemma-4-31b-it"], ["retry_ledger"]
        self.config.update(evaluation_mode=field_check.MODE, coding_models=models,
                           lmstudio_index=str(self.root / ".lmstudio/.internal/model-index-cache.json"),
                           helper_model=model_table.HELPER,
                           local_models=model_table.config_entries([*models, model_table.HELPER],
                                                                   self.root / ".lmstudio/models"),
                           scenario_ids=scenarios, intent_policies=["symbolic"], episode_limit=1,
                           episode_seconds=1200, context_tokens=32768, generation_policy={"local_temperature": 0.0},
                           harness_response_policy="reasoning-off",
                           evidence_byte_limit=evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                           reject_loop_limit=evolution.BASELINE_REJECT_LOOP_LIMIT, seed=1,
                           field_check_design=field_check.design_identity(models, scenarios))
        self.config.pop("codex")
        self.frozen.pop("codex")
        for index, model in enumerate(self.config["local_models"]):
            weights = Path(model["weights"])
            weights.mkdir(parents=True)
            (weights / "model.safetensors").write_text("fixture weights")
            self.frozen[f"local_model_{index}"] = {"weights": str(weights), "files": tree_manifest(weights)}
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        self.frozen["schedule"] = runner.schedule(self.config)

    def test_field_check_native_cell_archives_its_design_and_no_evolution_outputs(self):
        from bench.harness_study import field_check
        from bench.harness_study.artifacts import canonical_json
        self._field_check_config()
        self.cell = self.frozen["schedule"][1]
        self.assertEqual((self.cell["backend"], self.cell["condition"], self.cell["intent_policy"]),
                         ("opencode", "without", None))
        self.scenario["id"] = self.cell["scenario_id"]
        (self.scenarios / self.cell["scenario_id"]).symlink_to(self.scenarios / "fixture", target_is_directory=True)
        with patch.dict("sys.modules", {"bench.harness_study.indexing": None}):
            result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertEqual(self.observed_guards, [(8 * 1024 ** 3, 5)])
        self.assertNotIn("evolution_constituents", result["episodes"][0])
        backend, arguments = self.command_arguments[0]
        self.assertEqual(backend, "opencode")
        self.assertFalse(arguments["postedit_association_contract"])
        saved = Path(result["path"])
        self.assertEqual((saved / "field-check-design.json").read_bytes(),
                         canonical_json(self.config["field_check_design"]))
        self.assertFalse((saved / "evolution-design.json").exists())
        manifest = json.loads((saved / "manifest.json").read_text())
        self.assertEqual(manifest["evaluation_mode"], field_check.MODE)
        with self.assertRaisesRegex(ValueError, "frozen arms"):
            runner.run_cell(self.root / "other", dict(self.frozen, schedule=self.frozen["schedule"]
                            + [dict(self.cell, condition="harness")]), dict(self.cell, condition="harness"))

    def test_field_check_harness_cell_sends_symbolic_reasoning_off_without_postedit_contract(self):
        from types import SimpleNamespace
        from bench.harness_study import field_check, intent
        self._field_check_config()
        (self.repo / "spec/harness_intent_pilot.md").write_text("frozen fixture intent protocol\n")
        indexer_dir = self.root / "indexer"
        indexer_dir.mkdir()
        (indexer_dir / "scip-python").write_text("#!/bin/sh\n")
        indexer = {"directory": str(indexer_dir), "launcher": {"path": str(indexer_dir / "scip-python")}}
        self.config["indexer_manifest"] = str(indexer_dir / "manifest.json")
        self.binary_manifest["indexer"] = indexer
        self.frozen.update(indexer=indexer, intent_design=intent.design_identity(), indexer_probe={"ok": True},
                           indexer_system_python={"path": "/usr/bin/python3"})
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        self.cell = self.frozen["schedule"][0]
        self.assertEqual((self.cell["backend"], self.cell["intent_policy"]), ("harness", "symbolic"))
        self.scenario["id"] = self.cell["scenario_id"]
        (self.scenarios / self.cell["scenario_id"]).symlink_to(self.scenarios / "fixture", target_is_directory=True)
        readiness = []
        fake_indexing = SimpleNamespace(
            verify_indexer=lambda manifest: indexer, apply_overlay=lambda workspace: None,
            index_workspace=lambda *args: {"indexed": True},
            ready_dossiers=lambda daemon, scenario, **kwargs: readiness.append(kwargs) or {"ready": True},
            system_python_identity=lambda frozen: frozen)
        self.scenario["track"] = "inherited"
        with patch.dict("sys.modules", {"bench.harness_study.indexing": fake_indexing}):
            result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(readiness, [{"seed": True, "require_empty": False}])
        backend, arguments = self.command_arguments[0]
        self.assertEqual(backend, "harness")
        self.assertEqual(arguments["harness_intent_policy"], "symbolic")
        self.assertEqual(arguments["harness_response_policy"], "reasoning-off")
        self.assertFalse(arguments["postedit_association_contract"])
        saved = Path(result["path"])
        self.assertTrue((saved / "field-check-design.json").is_file())
        self.assertTrue((saved / "intent-design.json").is_file())
        self.assertFalse((saved / "evolution-design.json").exists())
        self.assertEqual(json.loads((saved / "manifest.json").read_text())["evaluation_mode"], field_check.MODE)

    def test_field_check_mismatched_approval_is_a_preflight_failure_before_model_load(self):
        self._field_check_config()
        self.cell = self.frozen["schedule"][1]
        self.scenario["id"] = self.cell["scenario_id"]
        (self.scenarios / self.cell["scenario_id"]).symlink_to(self.scenarios / "fixture", target_is_directory=True)
        with patch.dict("sys.modules", {"bench.harness_study.indexing": None}):
            result = self._run(approval_error=ValueError("field-check approval does not match the current design"))
        self.assertEqual(result["status"], "preflight_failure")
        self.assertIn("field-check approval", result["error"])
        self.assertEqual(self.model_load_calls, 0)
        self.assertEqual(self.observed, [])

    def test_pending_approval_is_a_retained_preflight_attempt(self):
        result = self._run(approval_error=ValueError("scenario rubric approval is pending"))
        self.assertEqual(result["status"], "preflight_failure")
        self.assertIn("approval is pending", result["error"])
        self.assertEqual(self.observed, [])
        self.assertTrue((Path(result["path"]) / "preflight.json").is_file())
        self.assertTrue((Path(result["path"]) / "outcome.json").is_file())

    def _freeze_ca_bundle(self):
        bundle = self.root / "fixture-ca.pem"
        bundle.write_bytes(b"fixture public CA bundle; no TLS is performed\n")
        self.config["codex_ca_bundle"] = str(bundle)
        self.frozen["codex_ca_bundle"] = {"path": str(bundle), "sha256": sha256_file(bundle)}
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        return bundle

    def test_changed_ca_bundle_fails_before_model_setup(self):
        bundle = self._freeze_ca_bundle()
        bundle.write_bytes(b"changed after freezing\n")
        result = self._run()
        self.assertEqual(result["status"], "preflight_failure", result)
        self.assertIn("CA bundle changed", result["error"])
        self.assertEqual(self.model_load_calls, 0)
        self.assertEqual(self.command_arguments, [])
        self.assertEqual(self.observed, [])
        self.assertEqual([episode["status"] for episode in result["episodes"]], ["unattempted"] * 3)

    def test_verified_ca_bundle_is_archived_and_forwarded_only_to_codex_backends(self):
        auth = self.root / "fixture-auth.json"
        auth.write_text('{"OPENAI_API_KEY":"test-only-credential"}')
        self.config["codex_auth"] = str(auth)
        bundle = self._freeze_ca_bundle()
        for backend, condition in (("codex", "without"), ("codex_mcp", "codex_mcp"),
                                   ("opencode", "without"), ("opencode_mcp", "opencode_mcp"),
                                   ("harness", "harness")):
            with self.subTest(backend=backend):
                self.cell.update(backend=backend, condition=condition)
                self.command_arguments.clear()
                result = self._run()
                self.assertEqual(result["status"], "success", result)
                self.assertEqual((Path(result["path"]) / "hosted-ca-bundle.pem").read_bytes(), bundle.read_bytes())
                self.assertEqual(len(self.command_arguments), 3)
                for observed_backend, arguments in self.command_arguments:
                    self.assertEqual(observed_backend, backend)
                    self.assertEqual(arguments.get("ca_bundle"), bundle if backend.startswith("codex") else None)

    def test_three_episodes_preserve_code_notes_graph_but_reset_history(self):
        result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(self.observed, ["e1", "e2", "e3"])
        # Without frozen guard keys the evidence guard is off and the reject loop keeps its default.
        self.assertEqual(self.observed_guards, [(None, 5)] * 3)
        self.assertEqual(self.archive_calls, 1)
        self.assertEqual([episode["status"] for episode in result["episodes"]], ["success"] * 3)
        run = Path(result["path"])
        for number in (1, 2, 3):
            saved = run / f"episodes/e{number}/workspace"
            self.assertEqual((saved / "service.py").read_text(), f"EPISODE = {number}\n")
            self.assertEqual((saved / "PROJECT_NOTES.md").read_text(), f"remember episode {number}\n")
            self.assertIn(f"graph episode {number}", (saved / ".moosedev/kg.nq").read_text())
            self.assertTrue((saved / ".moosedev/harness/conversations/session.json").is_file())
        self.assertTrue(all(not execution.exists() for execution in self.executions))

    def test_episode_limit_attempts_one_episode_and_manifest_binds_build(self):
        self.config["episode_limit"] = 1
        self.frozen["config_sha256"] = runner.configuration_hash(self.config)
        result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertEqual([(episode["status"], episode.get("reason")) for episode in result["episodes"]],
                         [("success", None), ("unattempted", "episode_limit"), ("unattempted", "episode_limit")])
        manifest = json.loads((Path(result["path"]) / "manifest.json").read_text())
        self.assertEqual(manifest["build_id"], "fixture-build")

    def test_agent_failure_keeps_dependent_episodes_unattempted(self):
        result = self._run(fail_episode="e1")
        self.assertEqual(result["status"], "agent_failure", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertEqual([episode["status"] for episode in result["episodes"]],
                         ["agent_failure", "unattempted", "unattempted"])

    def test_failed_snapshot_retains_execution_for_recovery(self):
        result = self._run(snapshot_failure=True)
        self.assertEqual(result["status"], "infrastructure_failure", result)
        self.assertIn("snapshot failure", result["error"])
        self.assertTrue(all(execution.exists() for execution in self.executions))
        self.assertEqual((self.executions[0] / "work/service.py").read_text(), "EPISODE = 1\n")

    def test_interrupted_observation_preserves_attempted_episode_and_evidence(self):
        result = self._run(interrupt_episode="e1")
        self.assertEqual(result["status"], "infrastructure_failure", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertEqual([episode["status"] for episode in result["episodes"]],
                         ["infrastructure_failure", "unattempted", "unattempted"])
        episode = result["episodes"][0]
        self.assertIn("KeyboardInterrupt", episode["error"])
        self.assertTrue(all(value is None for value in episode["metrics"].values()))
        self.assertEqual(episode["request_usage"]["sources"]["proxy"]["summary"]["requests"], 0)
        self.assertEqual(episode["checks"], [])
        self.assertFalse(episode["observation_complete"])
        saved = Path(result["path"])
        self.assertEqual(json.loads((saved / "episodes/e1/outcome.json").read_text()), episode)
        self.assertEqual((saved / "interrupted-workspace/service.py").read_text(), "EPISODE = 1\n")
        self.assertTrue((saved / "seal.json").is_file())
        self.assertTrue(all(not execution.exists() for execution in self.executions))
        inventory = report(self.root / "evidence")
        self.assertEqual(inventory["integrity_counts"], {"sealed": 1})
        self.assertEqual(inventory["runs"][0]["status"], "infrastructure_failure")
        self.assertEqual(inventory["groups"][0]["episode_statuses"],
                         {"infrastructure_failure": 1, "unattempted": 2})

    def test_second_episode_setup_failure_preserves_first_success_without_claiming_run_success(self):
        result = self._run(fail_setup_episode="e2")
        self.assertEqual(result["status"], "infrastructure_failure", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertIn("daemon setup failure", result["error"])
        self.assertEqual([episode["status"] for episode in result["episodes"]],
                         ["success", "unattempted", "unattempted"])
        saved = Path(result["path"])
        self.assertEqual(json.loads((saved / "episodes/e1/outcome.json").read_text())["status"], "success")
        self.assertEqual((saved / "episodes/e1/workspace/service.py").read_text(), "EPISODE = 1\n")
        self.assertEqual((saved / "interrupted-workspace/PROJECT_NOTES.md").read_text(), "remember episode 1\n")

    def test_checkpoint_failure_preserves_completed_observation_as_attempted_episode(self):
        result = self._run(fail_checkpoint_episode="e1")
        self.assertEqual(result["status"], "infrastructure_failure", result)
        self.assertEqual(self.observed, ["e1"])
        self.assertEqual([episode["status"] for episode in result["episodes"]],
                         ["infrastructure_failure", "unattempted", "unattempted"])
        episode = result["episodes"][0]
        self.assertIn("checkpoint failure", episode["error"])
        self.assertTrue(episode["observation_complete"])
        saved = Path(result["path"])
        self.assertEqual(json.loads((saved / "episodes/e1/outcome.json").read_text()), episode)
        self.assertEqual((saved / "interrupted-workspace/service.py").read_text(), "EPISODE = 1\n")
        self.assertEqual(report(self.root / "evidence")["integrity_counts"], {"sealed": 1})

    def test_private_build_archives_are_saved_but_never_granted_to_agents(self):
        result = self._run()
        self.assertEqual(result["status"], "success", result)
        self.assertEqual((Path(result["path"]) / "build/engine-source.tar.gz").read_bytes(), b"private engine archive")
        expected = {Path(self.binary_manifest["binaries"][role]) for role in ("session", "daemon")}
        for grants in self.grants:
            self.assertEqual(set(grants["readable_paths"]), expected)
            self.assertNotIn(self.build, grants["readable_paths"])


class RunHelperTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()

    def test_runtime_snapshot_excludes_authentication_and_rejects_aliases(self):
        store = ArtifactStore(self.root / "evidence")
        run = store.create_run({"model": "fixture"})
        runtime = self.root / "runtime"
        (runtime / "codex").mkdir(parents=True)
        (runtime / "codex/auth.json").write_text('{"token":"private"}')
        (runtime / "native.json").write_text('{"event":"done"}')
        runner.snapshot(store, run, runtime, "runtime", runtime=True)
        self.assertFalse((run / "runtime/codex/auth.json").exists())
        self.assertTrue((run / "runtime/native.json").exists())
        (runtime / "leaked.json").symlink_to(runtime / "codex/auth.json")
        with self.assertRaises(ValueError):
            runner.snapshot(store, run, runtime, "runtime-again", runtime=True)

    def test_reset_preserves_durable_files_and_rejects_parent_alias(self):
        workspace = self.root / "work"
        history = workspace / ".moosedev/harness/conversations"
        history.mkdir(parents=True)
        (history / "old.json").write_text("old conversation")
        (workspace / ".moosedev/kg.nq").write_text("durable graph")
        (workspace / ".moosedev/http.addr").write_text("old address")
        (workspace / "notes.md").write_text("durable notes")
        runner.reset_episode(workspace)
        self.assertFalse(history.exists())
        self.assertEqual((workspace / ".moosedev/kg.nq").read_text(), "durable graph")
        self.assertEqual((workspace / "notes.md").read_text(), "durable notes")
        self.assertFalse((workspace / ".moosedev/http.addr").exists())
        outside = self.root / "external-knowledge"
        (outside / "harness").mkdir(parents=True)
        (outside / "harness/do-not-delete").write_text("external")
        alias_work = self.root / "aliased-work"
        alias_work.mkdir()
        (alias_work / ".moosedev").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            runner.reset_episode(alias_work)
        self.assertEqual((outside / "harness/do-not-delete").read_text(), "external")

    def test_field_check_models_load_with_table_contexts(self):
        from bench.harness_study import model_table
        models = ["gemma-4-31b-it"]
        config = {"endpoint": "http://127.0.0.1:1234/v1", "helper_model": model_table.HELPER,
                  "context_tokens": 32768, "lms": "/explicit/lms",
                  "local_models": model_table.config_entries([*models, model_table.HELPER], self.root)}
        cell = {"backend": "harness", "condition": "harness", "model": "gemma-4-31b-it"}

        def inventory(coding=262144, helper=131072):
            return {"models": [
                {"key": "gemma-4-31b-it",
                 "loaded_instances": [{"id": "gemma-4-31b-it", "config": {"context_length": coding}}]},
                {"key": model_table.HELPER,
                 "loaded_instances": [{"id": model_table.HELPER, "config": {"context_length": helper}}]}]}

        records = []
        with patch.object(runner, "inventory", return_value=inventory()), \
                patch.object(runner.subprocess, "run") as launch:
            runner.load_models(config, cell, lambda channel, event: records.append((channel, event)))
            launch.assert_not_called()
        self.assertEqual(dict(records)["model_context_policy"]["expected_runtime_context_tokens"],
                         {"gemma-4-31b-it": 262144, model_table.HELPER: 131072})
        for loaded in (inventory(coding=32768), inventory(helper=32768)):
            with self.subTest(loaded=loaded), patch.object(runner, "inventory", return_value=loaded), \
                    patch.object(runner.subprocess, "run"):
                with self.assertRaisesRegex(ValueError, "different context"):
                    runner.load_models(config, cell, lambda *args: None)

    def test_loaded_models_require_exact_identity_and_context(self):
        config = {"endpoint": "http://127.0.0.1:1234/v1", "helper_model": "helper",
                  "context_tokens": 32768, "lms": "/explicit/lms"}
        cell = {"backend": "harness", "condition": "harness", "model": "agent"}

        def inventory(context=32768, identifier="agent"):
            return {"models": [
                {"key": "agent", "loaded_instances": [{"id": identifier, "config": {"context_length": context}}]},
                {"key": "helper", "loaded_instances": [{"id": "helper", "config": {"context_length": 32768}}]},
            ]}

        with patch.object(runner, "inventory", return_value=inventory()), \
                patch.object(runner.subprocess, "run") as launch:
            runner.load_models(config, cell, lambda *args: None)
            launch.assert_not_called()
        for context in (8192, 262144):
            with self.subTest(context=context), \
                    patch.object(runner, "inventory", return_value=inventory(context=context)), \
                    patch.object(runner.subprocess, "run") as launch:
                with self.assertRaisesRegex(ValueError, "different context"):
                    runner.load_models(config, cell, lambda *args: None)
                launch.assert_not_called()
        with patch.object(runner, "inventory", return_value=inventory(identifier="alias")), \
                patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"", b"")) as launch:
            with self.assertRaisesRegex(ValueError, "exact requested identity"):
                runner.load_models(config, cell, lambda *args: None)
            argv = launch.call_args.args[0]
            self.assertEqual(argv[:5], ["/explicit/lms", "load", "agent", "--identifier", "agent"])

    def test_runtime_context_override_is_exact_and_preserves_requested_load_context(self):
        config = {"endpoint": "http://127.0.0.1:1234/v1", "helper_model": "helper",
                  "context_tokens": 32768, "lms": "/explicit/lms",
                  "local_models": [{"id": "agent", "runtime_context_tokens": 262144}]}
        cell = {"backend": "harness", "condition": "harness", "model": "agent"}

        def inventory(context):
            instances = [] if context is None else [{"id": "agent", "config": {"context_length": context}}]
            return {"models": [
                {"key": "agent", "loaded_instances": instances},
                {"key": "helper", "loaded_instances": [{"id": "helper", "config": {"context_length": 32768}}]},
            ]}

        for already_loaded in (False, True):
            for observed in (32768, 131072, 262144, 524288):
                with self.subTest(already_loaded=already_loaded, observed=observed):
                    before = inventory(observed if already_loaded else None)
                    after = inventory(observed)
                    events = []
                    with patch.object(runner, "inventory", side_effect=[before, after]), \
                            patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"", b"")) as launch:
                        if observed == 262144:
                            runner.load_models(config, cell, lambda *event: events.append(event))
                        else:
                            with self.assertRaises(ValueError):
                                runner.load_models(config, cell, lambda *event: events.append(event))
                    if already_loaded:
                        launch.assert_not_called()
                    else:
                        self.assertEqual(launch.call_args.args[0], ["/explicit/lms", "load", "agent",
                            "--identifier", "agent", "--context-length", "32768", "--parallel", "1"])
                    self.assertEqual(events[0], ("model_context_policy", {
                        "client_context_tokens": 32768, "requested_load_context_tokens": 32768,
                        "expected_runtime_context_tokens": {"agent": 262144, "helper": 32768}}))
                    self.assertIn(("model_inventory_before", before), events)
                    if not already_loaded or observed == 262144:
                        self.assertIn(("model_inventory", after), events)

    def test_malformed_runtime_context_override_fails_before_model_setup(self):
        cell = {"backend": "opencode", "condition": "without", "model": "agent"}
        for override in (True, False, None, "262144", 262144.0, -1, 0, 32767):
            with self.subTest(override=override):
                config = {"context_tokens": 32768,
                          "local_models": [{"id": "agent", "runtime_context_tokens": override}]}
                with patch.object(runner, "inventory") as inventory, \
                        patch.object(runner.subprocess, "run") as launch:
                    with self.assertRaisesRegex(ValueError, "runtime_context_tokens.*integer >= context_tokens"):
                        runner.load_models(config, cell, lambda *args: None)
                    inventory.assert_not_called()
                    launch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
