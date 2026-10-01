"""Snapshot save and restore, on a scripted project only (no daemon, no index)."""
import json
import tempfile
import unittest
from pathlib import Path

from bench.harness_study import snapshot


def project(root):
    """A harness project with a task journal, live process files and caches."""
    root = Path(root)
    tasks = root / ".moosedev/harness/tasks"
    tasks.mkdir(parents=True)
    journal = {"schema": 2, "id": "t1", "root": str(root), "objective": "o", "phase": "AwaitingInput",
               "mode": "Auto", "events": [{"message": f"Command: cargo test\nran in {root}/x"}],
               "model_requests": [{"prompt": "p"}]}
    (tasks / "t1.json").write_text(json.dumps(journal, indent=2))
    (tasks / "t1.usage.jsonl").write_text("{}\n")
    (tasks / "runner.lock").write_text("")
    for live in snapshot.EXCLUDED:
        (root / ".moosedev" / live).write_text("live")
    (root / ".moosedev/substrate/generations").mkdir(parents=True)
    (root / ".moosedev/harness/lsp/t1/build").mkdir(parents=True)
    (root / ".moosedev/kg.nq").write_text("<a> <b> <c> .\n")
    (root / "moosedev.toml").write_text('[daemon]\nhttp_addr = "127.0.0.1:7491"\n')
    (root / "src.rs").write_text("fn main() {}\n")
    return root


class SnapshotTest(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.source = project(self.dir / "src-project")
        self.store = self.dir / "snapshots"

    def test_save_copies_the_state_without_live_files_and_records_the_tasks(self):
        meta = snapshot.save(self.source, "s1", root=self.store, meta={"kind": "park"})
        copy = self.store / "s1/project"
        self.assertEqual((copy / "src.rs").read_text(), "fn main() {}\n")
        self.assertTrue((copy / ".moosedev/kg.nq").exists())
        for live in snapshot.EXCLUDED:
            self.assertFalse((copy / ".moosedev" / live).exists(), live)
        self.assertFalse((copy / ".moosedev/harness/tasks/runner.lock").exists())
        # The metadata sits beside the copy: an extra workspace file would change prompts.
        self.assertFalse((copy / "snapshot.json").exists())
        self.assertEqual(meta["tasks"], [{"id": "t1", "objective": "o", "phase": "AwaitingInput", "mode": "Auto",
                                          "events": 1, "model_requests": 1}])
        self.assertEqual(meta["meta"], {"kind": "park"})
        self.assertEqual(meta["source"], str(self.source.resolve()))
        with self.assertRaises(FileExistsError):
            snapshot.save(self.source, "s1", root=self.store)

    def test_restore_moves_the_root_and_port_and_drops_path_caches_only(self):
        snapshot.save(self.source, "s1", root=self.store)
        dest = self.dir / "restored"
        self.assertIsNone(snapshot.restore("s1", dest, 7499, root=self.store, index=False, start=False))
        journal = json.loads((dest / ".moosedev/harness/tasks/t1.json").read_text())
        self.assertEqual(journal["root"], str(dest.resolve()))
        # History keeps the text the original run saw.
        self.assertIn(str(self.source), journal["events"][0]["message"])
        self.assertIn('http_addr = "127.0.0.1:7499"', (dest / "moosedev.toml").read_text())
        self.assertFalse((dest / ".moosedev/substrate").exists())
        self.assertFalse((dest / ".moosedev/harness/lsp").exists())
        self.assertTrue((dest / ".moosedev/kg.nq").exists())
        # The snapshot itself is untouched, so it can be restored again.
        kept = json.loads((self.store / "s1/project/.moosedev/harness/tasks/t1.json").read_text())
        self.assertEqual(kept["root"], str(self.source))
        self.assertEqual([row["name"] for row in snapshot.listing(self.store)], ["s1"])

    def test_a_worktree_pointer_is_dropped_and_a_missing_port_is_added(self):
        (self.source / ".git").write_text("gitdir: /elsewhere/.git/worktrees/x\n")
        (self.source / "moosedev.toml").write_text("[harness.model]\nmodel = \"m\"\n")
        snapshot.save(self.source, "s2", root=self.store)
        self.assertFalse((self.store / "s2/project/.git").exists())
        dest = self.dir / "restored2"
        snapshot.restore("s2", dest, 7498, root=self.store, index=False, start=False)
        config = (dest / "moosedev.toml").read_text()
        self.assertIn('[daemon]\nhttp_addr = "127.0.0.1:7498"', config)
        self.assertIn('model = "m"', config)

    def test_restoring_with_a_daemon_or_index_needs_the_binary(self):
        snapshot.save(self.source, "s1", root=self.store)
        with self.assertRaises(ValueError):
            snapshot.restore("s1", self.dir / "r2", 7499, root=self.store)


if __name__ == "__main__":
    unittest.main()
