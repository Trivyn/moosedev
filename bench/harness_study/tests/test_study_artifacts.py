import concurrent.futures
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study.artifacts import ArtifactStore, canonical_json, sha256_file


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.store = ArtifactStore(self.root / "evidence")
        self.run = self.store.create_run({"model": "fixture"})

    def test_canonical_encoding_and_hash(self):
        expected = '{"a":"λ","b":2}\n'.encode()
        self.assertEqual(canonical_json({"b": 2, "a": "λ"}), expected)
        self.store.put_bytes(self.run, "nested/text", expected)
        self.assertEqual(sha256_file(self.run / "nested/text"), hashlib.sha256(expected).hexdigest())
        with self.assertRaises(ValueError):
            canonical_json({"missing": float("nan")})

    def test_publication_never_overwrites_and_seal_is_idempotent(self):
        self.store.put_bytes(self.run, "output.txt", b"first")
        self.store.put_bytes(self.run, "output.txt", b"first")
        with self.assertRaises(FileExistsError):
            self.store.put_bytes(self.run, "output.txt", b"replacement")
        seal = self.store.seal_run(self.run)
        self.assertEqual(self.store.seal_run(self.run), seal)
        self.assertEqual(self.store.verify_run(self.run), seal)
        with self.assertRaises(ValueError):
            self.store.append_event(self.run, "late", {})
        with self.assertRaises(ValueError):
            self.store.put_bytes(self.run, "late", b"data")

    def test_tamper_missing_and_unexpected_evidence_detected(self):
        self.store.put_bytes(self.run, "output", b"original")
        self.store.seal_run(self.run)
        path = self.run / "output"
        path.write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            self.store.verify_run(self.run)
        path.unlink()
        with self.assertRaises(ValueError):
            self.store.verify_run(self.run)
        path.write_bytes(b"original")
        (self.run / "extra").write_bytes(b"unexpected")
        with self.assertRaises(ValueError):
            self.store.verify_run(self.run)

    def test_rejects_traversal_symlink_and_foreign_run(self):
        for relative in ("../escape", "/tmp/escape", "nested/../../escape", "seal.json", "events.jsonl"):
            with self.subTest(relative=relative), self.assertRaises(ValueError):
                self.store.put_bytes(self.run, relative, b"escape")
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "file").write_bytes(b"safe")
        (self.run / "link").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.store.put_bytes(self.run, "link/file", b"unsafe")
        (self.run / "file-link").symlink_to(outside / "file")
        with self.assertRaises((OSError, ValueError)):
            self.store.put_bytes(self.run, "file-link", b"unsafe")
        self.assertEqual((outside / "file").read_bytes(), b"safe")
        with self.assertRaises(ValueError):
            self.store.seal_run(self.run)
        with self.assertRaises(ValueError):
            self.store.put_bytes(outside, "file", b"unsafe")
        alias = self.root / "alias"
        alias.symlink_to(self.store.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            ArtifactStore(alias)

    def test_interrupted_publication_retains_unfinished_bytes(self):
        with patch("bench.harness_study.artifacts.os.link", side_effect=OSError("power failure")):
            with self.assertRaises(OSError):
                self.store.put_bytes(self.run, "output", b"recoverable evidence")
        pending = list(self.run.glob(".pending-*"))
        self.assertEqual(len(pending), 1)
        self.assertEqual(pending[0].read_bytes(), b"recoverable evidence")
        self.assertFalse((self.run / "output").exists())
        with self.assertRaisesRegex(ValueError, "interrupted publication"):
            self.store.seal_run(self.run)
        reopened = ArtifactStore(self.store.root)
        self.assertTrue(reopened.run_path(self.run).exists())
        self.assertEqual(pending[0].read_bytes(), b"recoverable evidence")

    def test_interrupted_event_tail_is_never_truncated(self):
        self.store.append_event(self.run, "stdout", {"text": "first"})
        path = self.run / "events.jsonl"
        with path.open("ab") as out:
            out.write(b'{"sequence":2')
        before = path.read_bytes()
        with self.assertRaisesRegex(ValueError, "interrupted append"):
            self.store.append_event(self.run, "stdout", {"text": "second"})
        self.assertEqual(path.read_bytes(), before)
        with self.assertRaises(ValueError):
            self.store.seal_run(self.run)

    def test_seal_streams_the_event_log_and_matches_the_whole_file_digest(self):
        from bench.harness_study.artifacts import _iter_lines
        for number in range(3):
            self.store.append_event(self.run, "stdout", {"number": number, "text": "λ" * 900})
        self.store.put_bytes(self.run, "outcome.json", canonical_json({"status": "success"}))
        # The seal format is unchanged: the digest of the sorted per-file inventory.
        files = {path.relative_to(self.run).as_posix(): {"sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                                          "size": path.stat().st_size}
                 for path in sorted(self.run.rglob("*")) if path.is_file()}
        expected = hashlib.sha256(canonical_json(files)).hexdigest()
        with patch("bench.harness_study.artifacts._read_lines", side_effect=AssertionError("whole-file read")):
            seal = self.store.seal_run(self.run)
            self.assertEqual(self.store.verify_run(self.run), seal)
        self.assertEqual(seal["files"], files)
        self.assertEqual(seal["evidence_sha256"], expected)
        # The reader is lazy: records before a corrupt line are yielded before it fails.
        path = self.run / "events.jsonl"
        lazy = _iter_lines(path)
        self.assertEqual(next(lazy)["sequence"], 1)
        lazy.close()
        self.assertEqual(list(_iter_lines(self.run / "missing.jsonl")), [])

    def test_streaming_seal_reports_corrupt_or_interrupted_events_unchanged(self):
        self.store.append_event(self.run, "stdout", {"text": "first"})
        path = self.run / "events.jsonl"
        good = path.read_bytes()
        for tail, message in ((b'{"sequence":3,"channel":"stdout","payload":{}}\n', "event sequence is corrupt"),
                              (b"[]\n", "event sequence is corrupt"),
                              (b'{"sequence":2', "interrupted append")):
            with self.subTest(tail=tail):
                path.write_bytes(good + tail)
                with self.assertRaisesRegex(ValueError, message):
                    self.store.seal_run(self.run)
                self.assertFalse((self.run / "seal.json").exists())
        path.write_bytes(good)
        self.store.seal_run(self.run)
        self.store.verify_run(self.run)

    def test_concurrent_appends_preserve_all_complete_records(self):
        def append(number):
            ArtifactStore(self.store.root).append_event(self.run, "stdout", {"number": number, "text": "λ" * 9000})
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(append, range(32)))
        events = [json.loads(line) for line in (self.run / "events.jsonl").read_text().splitlines()]
        self.assertEqual([event["sequence"] for event in events], list(range(1, 33)))
        self.assertEqual({event["payload"]["number"] for event in events}, set(range(32)))
        self.store.seal_run(self.run)
        self.store.verify_run(self.run)


if __name__ == "__main__":
    unittest.main()
