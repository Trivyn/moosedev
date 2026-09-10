from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import clients


class ClientFreezeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.install = self.root / ".nvm/versions/node/v-fixture"
        self.node = self.make_file(self.install / "bin/node", b"fixture Node executable", executable=True)
        self.codex = self.make_file(self.install / "lib/node_modules/@openai/codex/bin/codex.js",
                                    b"fixture Codex launcher", executable=True)
        self.native = self.make_file(self.install / "lib/node_modules/@openai/codex-darwin/native/codex",
                                     b"fixture native Codex", executable=True)
        self.opencode = self.make_file(self.install / "lib/node_modules/opencode-ai/bin/opencode",
                                       b"fixture OpenCode launcher", executable=True)
        self.other = self.make_file(self.install / "lib/node_modules/unrelated/private.txt", b"unrelated package")
        self.make_file(self.install / "private-configuration.json", b"private installation config")
        (self.install / "bin/codex").symlink_to("../lib/node_modules/@openai/codex/bin/codex.js")
        self.repo_patch = patch.object(clients, "REPO", self.repo)
        self.repo_patch.start()
        self.addCleanup(self.repo_patch.stop)
        self.probe = patch.object(clients.subprocess, "run",
                                 return_value=subprocess.CompletedProcess([], 0, b"fixture 1.0\n", b""))
        self.launch = self.probe.start()
        self.addCleanup(self.probe.stop)

    def make_file(self, path, data, *, executable=False):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o755 if executable else 0o644)
        return path

    def test_freezes_selected_packages_and_node_preserving_native_layout(self):
        identity = clients.freeze_client(self.install / "bin/codex")
        frozen = Path(identity["runtime_root"])
        self.assertTrue(frozen.is_relative_to(self.repo / "target/harness-study/clients"))
        self.assertEqual(Path(identity["path"]).relative_to(frozen), self.codex.relative_to(self.install))
        self.assertEqual(identity["source_path"], str(self.codex))
        self.assertEqual((frozen / "bin/node").read_bytes(), self.node.read_bytes())
        self.assertEqual((frozen / self.native.relative_to(self.install)).read_bytes(), self.native.read_bytes())
        self.assertTrue((frozen / self.opencode.relative_to(self.install)).exists())
        self.assertFalse((frozen / "lib/node_modules/unrelated").exists())
        self.assertFalse((frozen / "private-configuration.json").exists())
        self.assertEqual(self.launch.call_args.args[0], [identity["path"], "--version"])
        self.assertEqual(self.launch.call_args.kwargs["env"]["PATH"].split(":")[0], str(frozen / "bin"))
        self.assertNotEqual(self.launch.call_args.kwargs["env"]["HOME"], str(Path.home()))
        self.assertEqual(clients.verify_client(identity), identity)

    def test_clients_share_identical_frozen_runtime_and_source_updates_do_not_replace_it(self):
        codex = clients.freeze_client(self.codex)
        opencode = clients.freeze_client(self.opencode)
        self.assertEqual(codex["runtime_root"], opencode["runtime_root"])
        self.codex.write_bytes(b"updated Codex launcher")
        self.assertEqual(clients.verify_client(codex), codex)
        replacement = clients.freeze_client(self.codex)
        self.assertNotEqual(replacement["runtime_root"], codex["runtime_root"])
        self.assertEqual(Path(codex["path"]).read_bytes(), b"fixture Codex launcher")

    def test_standalone_client_copies_only_the_selected_executable(self):
        executable = self.make_file(self.root / ".lmstudio/bin/lms", b"standalone LMS", executable=True)
        self.make_file(executable.parent / "credentials.json", b"private credentials")
        identity = clients.freeze_client(executable)
        frozen = Path(identity["runtime_root"])
        self.assertEqual(Path(identity["path"]), frozen / "bin/lms")
        self.assertEqual(set(identity["runtime_files"]), {str(frozen / "bin/lms")})
        self.assertFalse((frozen / "credentials.json").exists())

    def test_relative_internal_link_retained_and_external_link_rejected(self):
        internal = self.codex.parent / "native-link"
        internal.symlink_to("../../codex-darwin/native/codex")
        identity = clients.freeze_client(self.codex)
        frozen_link = Path(identity["runtime_root"]) / internal.relative_to(self.install)
        self.assertTrue(frozen_link.is_symlink())
        self.assertTrue(frozen_link.resolve().is_relative_to(identity["runtime_root"]))
        self.assertEqual(clients.verify_client(identity), identity)
        external = self.codex.parent / "external-link"
        external.symlink_to(self.other)
        with self.assertRaisesRegex(ValueError, "escapes frozen assets"):
            clients.freeze_client(self.codex)

    def test_binary_and_symlink_tampering_fail_verification(self):
        link = self.codex.parent / "native-link"
        link.symlink_to("../../codex-darwin/native/codex")
        identity = clients.freeze_client(self.codex)
        executable = Path(identity["path"])
        executable.chmod(0o700)
        original = executable.read_bytes()
        executable.write_bytes(b"tampered launcher")
        with self.assertRaisesRegex(ValueError, "runtime changed"):
            clients.verify_client(identity)
        executable.write_bytes(original)
        frozen_link = Path(identity["runtime_root"]) / link.relative_to(self.install)
        frozen_link.unlink()
        frozen_link.symlink_to(self.codex)
        with self.assertRaisesRegex(ValueError, "escapes frozen assets"):
            clients.verify_client(identity)

    def test_source_change_during_copy_rejects_ambiguous_runtime(self):
        original = clients.shutil.copytree

        def changing_copy(source, destination, *args, **kwargs):
            result = original(source, destination, *args, **kwargs)
            self.codex.write_bytes(b"changed while copying")
            return result

        with patch.object(clients.shutil, "copytree", side_effect=changing_copy):
            with self.assertRaisesRegex(RuntimeError, "changed during freezing"):
                clients.freeze_client(self.codex)

    def test_missing_node_and_failed_frozen_probe_do_not_fall_back(self):
        self.node.unlink()
        with self.assertRaisesRegex(ValueError, "installation's Node"):
            clients.freeze_client(self.codex)
        self.launch.assert_not_called()
        executable = self.make_file(self.root / "native", b"native fixture", executable=True)
        self.launch.return_value = subprocess.CompletedProcess([], 1, b"", b"missing runtime dependency")
        with self.assertRaisesRegex(ValueError, "missing runtime dependency"):
            clients.freeze_client(executable)
        self.assertNotEqual(self.launch.call_args.args[0][0], str(executable))


if __name__ == "__main__":
    unittest.main()
