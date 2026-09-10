import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import client_archive
from bench.harness_study.artifacts import ArtifactStore, canonical_json


class ClientArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.store = ArtifactStore(self.root / "evidence")
        self.run = self.store.create_run({"model": "fixture"})
        self.runtime = self.root / "frozen-runtime"
        (self.runtime / "bin").mkdir(parents=True)
        (self.runtime / "bin/client").write_bytes(b"portable CLI runtime")
        (self.runtime / "bin/client").chmod(0o500)
        (self.runtime / "bin/alias").symlink_to("client")
        manifest = {"files": {
            "bin": {"type": "directory"},
            "bin/client": {"type": "file", "sha256": hashlib.sha256(b"portable CLI runtime").hexdigest(), "mode": 0o500},
            "bin/alias": {"type": "symlink", "target": "client"},
        }}
        (self.runtime / "manifest.json").write_bytes(canonical_json(manifest))
        (self.runtime / "manifest.json").chmod(0o400)
        self.identity = {"runtime_root": str(self.runtime), "runtime_sha256": "a" * 64}
        verify = patch.object(client_archive, "verify_client", side_effect=lambda identity: identity)
        self.verify = verify.start()
        self.addCleanup(verify.stop)

    def archive(self, run=None):
        client_archive.archive_clients(self.store, run or self.run, [self.identity, self.identity])
        return self.store.root / "assets" / ("a" * 64 + ".tar")

    def test_deduplicates_runtime_across_runs_and_retains_internal_link_without_following(self):
        path = self.archive()
        original = path.stat()
        references = json.loads((self.run / "runtime-assets.json").read_bytes())
        self.assertEqual(len(references["assets"]), 1)
        self.store.seal_run(self.run)
        self.store.verify_run(self.run)
        another = self.store.create_run({"model": "another"})
        with patch.object(client_archive, "_create_archive", side_effect=AssertionError("must reuse archive")):
            self.archive(another)
        self.assertEqual(path.stat().st_ino, original.st_ino)
        self.assertEqual(path.stat().st_mtime_ns, original.st_mtime_ns)
        with tarfile.open(path) as archive:
            self.assertEqual(set(archive.getnames()), {"bin", "bin/client", "bin/alias", "manifest.json"})
            self.assertTrue(archive.getmember("bin/alias").issym())
            self.assertEqual(archive.getmember("bin/alias").linkname, "client")
            self.assertEqual(archive.extractfile("bin/client").read(), b"portable CLI runtime")

    def test_exported_store_verifies_offline_after_original_runtime_is_removed(self):
        self.archive()
        self.store.seal_run(self.run)
        exported = self.root / "exported-evidence"
        shutil.copytree(self.store.root, exported)
        shutil.rmtree(self.runtime)
        with patch("socket.socket", side_effect=AssertionError("no network")), \
                patch("subprocess.Popen", side_effect=AssertionError("no execution")):
            restored = ArtifactStore(exported)
            restored.verify_run(exported / "runs" / self.run.name)

    def test_archive_tamper_and_missing_assets_invalidate_sealed_run(self):
        path = self.archive()
        self.store.seal_run(self.run)
        with path.open("r+b") as stream:
            stream.seek(1024)
            stream.write(b"tamper")
        with self.assertRaisesRegex(ValueError, "archive size or hash mismatch"):
            self.store.verify_run(self.run)
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            self.store.verify_run(self.run)

    def test_archive_aliases_and_reference_traversal_are_rejected(self):
        path = self.archive()
        self.store.seal_run(self.run)
        outside = self.root / "archive-copy"
        shutil.copyfile(path, outside)
        path.unlink()
        path.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "without aliases"):
            self.store.verify_run(self.run)
        another = self.store.create_run({"model": "fixture"})
        self.store.put_bytes(another, "runtime-assets.json", canonical_json({"schema_version": 1, "assets": [
            {"runtime_sha256": "a" * 64, "path": "../archive-copy", "archive_sha256": "b" * 64, "size": 0},
        ]}))
        self.store.seal_run(another)
        with self.assertRaisesRegex(ValueError, "canonical asset path"):
            self.store.verify_run(another)

    def test_external_runtime_link_is_never_archived(self):
        (self.runtime / "bin/alias").unlink()
        external = self.root / "private"
        external.write_text("private data")
        (self.runtime / "bin/alias").symlink_to(external)
        with self.assertRaisesRegex(ValueError, "escaping symbolic link"):
            self.archive()
        self.assertFalse((self.run / "runtime-assets.json").exists())

    def test_interrupted_publication_retains_pending_archive(self):
        with patch.object(client_archive.os, "link", side_effect=OSError("publication interrupted")):
            with self.assertRaises(OSError):
                self.archive()
        pending = list((self.store.root / "assets").glob(".pending-*"))
        self.assertEqual(len(pending), 1)
        self.assertGreater(pending[0].stat().st_size, 0)
        self.assertFalse((self.run / "runtime-assets.json").exists())

    def test_source_change_during_archiving_does_not_publish_a_completed_asset(self):
        self.verify.side_effect = [self.identity, self.identity, RuntimeError("runtime changed")]
        with self.assertRaisesRegex(RuntimeError, "runtime changed"):
            self.archive()
        self.assertFalse((self.store.root / "assets" / ("a" * 64 + ".tar")).exists())


if __name__ == "__main__":
    unittest.main()
