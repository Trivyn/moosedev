import hashlib
from pathlib import Path
import signal
import tempfile
import unittest
from unittest import mock

from bench.harness_study.daemon import OwnedDaemon, _NotReady


class DaemonTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        patcher = mock.patch("bench.harness_study.daemon.REPO", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.workspace = self.root / "work"
        (self.workspace / ".moosedev").mkdir(parents=True)
        (self.workspace / ".moosedev" / "kg.nq").write_text("seed\n")
        self.runtime = self.root / "runtime"
        self.runtime.mkdir()
        self.assets = self.root / "assets"
        for child in ("ontologies", "models", "skills"):
            (self.assets / child).mkdir(parents=True)
        (self.root / "target").mkdir()
        self.executable = self.root / "target" / "moosedev"
        self.executable.write_bytes(b"frozen executable")
        self.executable.chmod(0o500)

    def daemon(self, **overrides):
        values = dict(executable=self.executable, expected_sha256=hashlib.sha256(b"frozen executable").hexdigest(),
                      workspace=self.workspace, runtime=self.runtime, assets=self.assets,
                      helper_model="exact-helper", helper_endpoint="http://127.0.0.1:1234/v1",
                      log_path=self.root / "daemon.log")
        return OwnedDaemon(**{**values, **overrides})

    def test_changed_binary_and_preexisting_rendezvous_fail_before_spawn(self):
        with mock.patch("bench.harness_study.daemon.subprocess.Popen") as popen:
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                self.daemon(expected_sha256="wrong").__enter__()
            (self.runtime / "daemon.sock").touch()
            with self.assertRaisesRegex(ValueError, "preexisting"):
                self.daemon().__enter__()
            popen.assert_not_called()

    def test_project_configuration_files_fail_before_spawn(self):
        for name, message in ((".env", "dotenv"), ("moosedev.toml", "moosedev.toml")):
            with self.subTest(name=name), mock.patch("bench.harness_study.daemon.subprocess.Popen") as popen:
                (self.workspace / name).touch()
                with self.assertRaisesRegex(ValueError, message):
                    self.daemon().__enter__()
                (self.workspace / name).unlink()
                popen.assert_not_called()

    def test_environment_is_explicit_and_never_inherits_credentials(self):
        with mock.patch.dict("os.environ", {"OPENAI_API_KEY": "secret", "MOOSEDEV_LLM_MODEL": "wrong"}):
            env = self.daemon()._controlled_environment("127.0.0.1:12345")
        self.assertNotIn("OPENAI_API_KEY", env)
        self.assertEqual(env["MOOSEDEV_LLM_MODEL"], "exact-helper")
        self.assertEqual(env["MOOSEDEV_NO_AUTOSPAWN"], "1")
        self.assertEqual(env["MOOSEDEV_MODEL_DIR"], str(self.assets))
        self.assertEqual(env["HOME"], str(self.runtime / "home"))

    def test_owned_pid_and_listener_are_both_required(self):
        daemon = self.daemon()
        daemon.process = mock.Mock(pid=4321)
        daemon.process.poll.return_value = None
        with mock.patch("bench.harness_study.daemon.subprocess.check_output", return_value="/foreign/moosedev\n"):
            with self.assertRaisesRegex(ValueError, "unexpected image"):
                daemon._process_identity(12345)
        with mock.patch("bench.harness_study.daemon.subprocess.check_output", return_value=str(self.executable)), \
                mock.patch("bench.harness_study.daemon.subprocess.run", return_value=mock.Mock(returncode=1, stdout="")):
            with self.assertRaises(_NotReady):
                daemon._process_identity(12345)

    def test_stop_signals_group_only_once_and_never_after_reap(self):
        daemon = self.daemon()
        daemon.process = mock.Mock(pid=4321)
        daemon.process.poll.return_value = None
        with mock.patch("bench.harness_study.daemon.os.killpg") as kill:
            daemon.stop()
            daemon.stop()
            kill.assert_called_once_with(4321, signal.SIGKILL)
            daemon.process.wait.assert_called_once_with(timeout=5)
        daemon = self.daemon()
        daemon.process = mock.Mock(pid=1234)
        daemon.process.poll.return_value = 0
        with mock.patch("bench.harness_study.daemon.os.killpg") as kill:
            daemon.stop()
            kill.assert_not_called()

    def test_checkpoint_requires_durable_publication_and_preserves_nonconformance(self):
        daemon = self.daemon()
        daemon.url = "http://127.0.0.1:12345"
        checkpoint = {"durable": True, "conforms": False, "revision": "abc", "pending": []}
        with mock.patch.object(daemon, "_process_identity"), mock.patch.object(daemon, "_alive"), \
                mock.patch.object(daemon, "_request", return_value=checkpoint) as request:
            self.assertEqual(daemon.checkpoint(), checkpoint)
            request.assert_called_once_with("/api/v1/harness/checkpoint", {})
        with mock.patch.object(daemon, "_process_identity"), \
                mock.patch.object(daemon, "_request", return_value={**checkpoint, "durable": False}):
            with self.assertRaisesRegex(ValueError, "durable"):
                daemon.checkpoint()


if __name__ == "__main__":
    unittest.main()
