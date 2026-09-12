import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import binaries
from bench.harness_study.artifacts import canonical_json


class BinaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "moosedev"
        self.repo.mkdir()
        self.source = {"commit": "fixture-commit", "files": {},
                       "tree_sha256": hashlib.sha256(canonical_json({})).hexdigest()}
        for role, name in binaries.BINARIES.items():
            path = self.repo / "target" / "release" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"fixture executable {role}".encode())
            path.chmod(0o700)

    def freeze(self):
        return binaries.freeze_binaries(self.repo, self.source)

    def test_ignores_conflicting_path_and_freeze_survives_source_rebuild(self):
        homebrew = self.root / "homebrew"
        homebrew.mkdir()
        (homebrew / "moosedev").write_bytes(b"wrong installed binary")
        (homebrew / "moosedev").chmod(0o700)
        with patch.dict(os.environ, {"PATH": str(homebrew)}):
            first = self.freeze()
        self.assertEqual(Path(first["binaries"]["daemon"]).read_bytes(), b"fixture executable daemon")
        source = self.repo / "target/release/moosedev"
        source.write_bytes(b"new repository build")
        self.assertEqual(binaries.verify_binaries(Path(first["directory"]) / "manifest.json", self.repo), first)
        second = self.freeze()
        self.assertNotEqual(first["build_id"], second["build_id"])
        self.assertEqual(Path(first["binaries"]["daemon"]).read_bytes(), b"fixture executable daemon")

    def test_missing_repository_binary_never_falls_back_to_path(self):
        (self.repo / "target/release/moosedev").unlink()
        with patch.object(binaries.shutil, "which", side_effect=AssertionError("no PATH lookup")):
            with self.assertRaises(FileNotFoundError):
                self.freeze()

    def test_freezes_owned_isolated_target_without_reading_shared_outputs(self):
        target = self.repo / "target/isolated-build"
        for role, name in binaries.BINARIES.items():
            path = target / "release" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"isolated executable {role}".encode())
            path.chmod(0o700)
        manifest = binaries.freeze_binaries(self.repo, self.source, artifact_target=target)
        self.assertEqual(Path(manifest["binaries"]["daemon"]).read_bytes(), b"isolated executable daemon")
        self.assertEqual(binaries.verify_binaries(Path(manifest["directory"]) / "manifest.json", self.repo), manifest)

    def test_isolated_target_cannot_escape_repository_target(self):
        outside = self.root / "outside"
        outside.mkdir()
        with self.assertRaisesRegex(ValueError, "repository's target"):
            binaries.freeze_binaries(self.repo, self.source, artifact_target=outside)
        alias = self.repo / "target/alias"
        alias.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            binaries.freeze_binaries(self.repo, self.source, artifact_target=alias)

    def test_rejects_source_binary_and_parent_directory_aliases(self):
        path = self.repo / "target/release/moosedev"
        original = path.read_bytes()
        path.unlink()
        external = self.root / "external"
        external.write_bytes(original)
        external.chmod(0o700)
        path.symlink_to(external)
        with self.assertRaises(ValueError):
            self.freeze()
        path.unlink()
        path.write_bytes(original)
        path.chmod(0o700)
        examples = self.repo / "target/release/examples"
        moved = self.root / "examples"
        examples.rename(moved)
        examples.symlink_to(moved, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.freeze()

    def test_detects_binary_manifest_and_unexpected_file_tampering(self):
        manifest = self.freeze()
        directory = Path(manifest["directory"])
        executable = Path(manifest["binaries"]["daemon"])
        original = executable.read_bytes()
        executable.chmod(0o700)
        executable.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            binaries.verify_binaries(directory / "manifest.json", self.repo)
        executable.write_bytes(original)
        (directory / "extra").write_text("unexpected")
        with self.assertRaisesRegex(ValueError, "unexpected or missing"):
            binaries.verify_binaries(directory / "manifest.json", self.repo)
        (directory / "extra").unlink()
        path = directory / "manifest.json"
        path.chmod(0o600)
        manifest["source"]["commit"] = "another commit"
        path.write_bytes(canonical_json(manifest))
        with self.assertRaisesRegex(ValueError, "identity mismatch"):
            binaries.verify_binaries(path, self.repo)

    def test_manifest_and_frozen_executable_aliases_rejected(self):
        manifest = self.freeze()
        directory = Path(manifest["directory"])
        alias = self.root / "manifest.json"
        alias.symlink_to(directory / "manifest.json")
        with self.assertRaises((OSError, ValueError)):
            binaries.verify_binaries(alias, self.repo)
        executable = Path(manifest["binaries"]["daemon"])
        moved = self.root / "original-executable"
        executable.rename(moved)
        executable.symlink_to(moved)
        with self.assertRaises(ValueError):
            binaries.verify_binaries(directory / "manifest.json", self.repo)

    def test_archives_untracked_source_and_link_metadata_without_dereferencing(self):
        (self.repo / "tracked.rs").write_text("tracked source")
        (self.repo / "new.rs").write_text("untracked implementation")
        private = self.root / "private"
        private.write_text("must not be archived")
        (self.repo / "reference-link").symlink_to("../private")

        def git(repo, *args):
            if args[0] == "ls-files":
                return b"tracked.rs\0new.rs\0reference-link\0deleted.rs\0"
            return b"fixture-commit\n"

        with patch.object(binaries, "git_output", side_effect=git):
            identity = binaries.source_identity(self.repo)
        archive = self.root / "source.tar.gz"
        binaries._source_archive(self.repo, identity, archive)
        with tarfile.open(archive) as source:
            self.assertEqual(set(source.getnames()), {"tracked.rs", "new.rs", "reference-link"})
            self.assertEqual(source.extractfile("new.rs").read(), b"untracked implementation")
            self.assertTrue(source.getmember("reference-link").issym())
            self.assertEqual(source.getmember("reference-link").linkname, "../private")
        self.assertTrue(identity["files"]["deleted.rs"]["deleted"])

    def test_build_receipt_binds_private_source_engine_and_logs(self):
        engine = self.root / "moose"
        (engine / ".git").mkdir(parents=True)
        (engine / "engine.rs").write_text("private engine source")
        (self.repo / "new.rs").write_text("new untracked implementation")

        def git(repo, *args):
            if args[0] == "ls-files":
                return b"engine.rs\0" if repo == engine else b"new.rs\0"
            return b"fixture-commit\n" if args[0] == "rev-parse" else b"tracked patch"

        completed = subprocess.CompletedProcess(["cargo"], 0, b"build output", b"compiler diagnostics")
        with patch.object(binaries, "git_output", side_effect=git), \
                patch.object(binaries.shutil, "which", return_value="/usr/bin/true"), \
                patch.object(binaries.subprocess, "run", return_value=completed) as launch, \
                patch.dict(os.environ, {"CARGO_TARGET_DIR": "/wrong/shared/target"}):
            manifest = binaries.build_and_freeze(self.repo)
        self.assertEqual(launch.call_args.kwargs["env"]["CARGO_TARGET_DIR"], str(self.repo / "target"))
        directory = Path(manifest["directory"])
        self.assertIn("source.tar.gz", manifest["build_receipt"]["files"])
        self.assertIn("engine-source.tar.gz", manifest["build_receipt"]["files"])
        with tarfile.open(directory / "engine-source.tar.gz") as source:
            self.assertEqual(source.extractfile("engine.rs").read(), b"private engine source")
        with tarfile.open(directory / "source.tar.gz") as source:
            self.assertEqual(source.extractfile("new.rs").read(), b"new untracked implementation")
        log = directory / "build.stdout"
        log.chmod(0o600)
        log.write_bytes(b"changed log")
        with self.assertRaisesRegex(ValueError, "build evidence hash mismatch"):
            binaries.verify_binaries(directory / "manifest.json", self.repo)

    def test_source_change_during_build_refuses_identity_and_retains_attempt(self):
        changed = dict(self.source, commit="changed")
        completed = subprocess.CompletedProcess(["cargo"], 0, b"output before refusal", b"")
        with patch.object(binaries, "source_identity", side_effect=[self.source, changed]), \
                patch.object(binaries, "git_output", return_value=b""), \
                patch.object(binaries.shutil, "which", return_value="/usr/bin/true"), \
                patch.object(binaries.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "source changed during the build"):
                binaries.build_and_freeze(self.repo)
        attempts = list((self.repo / "target/harness-study/build-attempts").iterdir())
        self.assertEqual(len(attempts), 1)
        self.assertEqual((attempts[0] / "build.stdout").read_bytes(), b"output before refusal")
        self.assertTrue((attempts[0] / "source.tar.gz").is_file())


if __name__ == "__main__":
    unittest.main()
