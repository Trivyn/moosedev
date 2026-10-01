"""Save a harness project's whole state at a point, and restore it elsewhere
to replay from there.

A harness project's state is one directory: the working tree and its
`.moosedev/` (the graph's `kg.nq` and RocksDB store, the vector DBs, the code
index under `substrate/`, and the task journals under `harness/tasks/`). A
snapshot is a copy of it, made with APFS clonefile (`cp -c`) where available:
instant, and sharing blocks until a file changes.

Layout: `<root>/<name>/project/` is the copy and `<root>/<name>/snapshot.json`
its metadata. The metadata stays outside the copy, because the harness lists
the workspace's files and an extra file would change its prompts.

Save only while nothing writes: between harness CLI commands, with the
daemon idle. `kg.nq` stays the graph's source of truth either way; the daemon
rehydrates from it at start if its store disagrees.

Restore copies the snapshot to a new path and makes it live there:
- each task journal's `root` is rewritten to the new path (`Runner::load`
  refuses a journal whose root is not the workspace); the history inside is
  left as it was, so a rendered prompt keeps the text the original run saw;
- the daemon port in `moosedev.toml` is set;
- the code index (`substrate/`) and the build caches under
  `.moosedev/harness/{lsp,scratch}` hold absolute paths (the SCIP index is
  length-prefixed protobuf, so it cannot be patched): they are dropped, and
  the index is rebuilt with `moosedev index` before the daemon starts.
"""
from __future__ import annotations

import datetime
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import time
import urllib.request
from pathlib import Path

DEFAULT_ROOT = Path.home() / "code" / "badciv-snapshots"
# Live process files: never part of a snapshot.
EXCLUDED = ("moosedev.sock", "moosedev-lsp.sock", "http.addr", "moosedev-serve.pid")
TASKS = Path(".moosedev/harness/tasks")
# A Unix socket path must fit sun_path (104 bytes on macOS, 108 on Linux).
SOCKET_PATH_LIMIT = 104


def clone_tree(source, dest):
    """Copy `source` to `dest` (which must not exist) by clonefile when the
    filesystem supports it, else by an ordinary copy."""
    source, dest = Path(source), Path(dest)
    if dest.exists():
        raise FileExistsError(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    cloned = subprocess.run(["cp", "-c", "-R", str(source), str(dest)], capture_output=True)
    if cloned.returncode != 0:
        if dest.exists():
            shutil.rmtree(dest)
        shutil.copytree(source, dest, symlinks=True)


def drop_live_files(project):
    """Process files, and a git worktree's `.git` pointer file: a copy that
    kept it would share the original worktree's index (a full clone's `.git`
    directory is copied whole and stays independent)."""
    git = Path(project) / ".git"
    if git.is_file():
        git.unlink()
    data = Path(project) / ".moosedev"
    for name in EXCLUDED:
        path = data / name
        if path.exists() or path.is_symlink():
            path.unlink()
    lock = Path(project) / TASKS / "runner.lock"
    if lock.exists():
        lock.unlink()


def tasks(project):
    """(path, journal) for every task journal in the project."""
    found = []
    for path in sorted((Path(project) / TASKS).glob("*.json")):
        try:
            found.append((path, json.loads(path.read_text())))
        except (OSError, ValueError):
            continue
    return found


def task_summary(journal):
    return {
        "id": journal.get("id"),
        "objective": journal.get("objective"),
        "phase": journal.get("phase"),
        "mode": journal.get("mode"),
        "events": len(journal.get("events") or []),
        "model_requests": len(journal.get("model_requests") or []),
    }


def git_state(directory):
    """HEAD, and a hash of the uncommitted diff, of a git checkout; None when
    it is not one."""
    directory = Path(directory)
    head = subprocess.run(["git", "-C", str(directory), "rev-parse", "HEAD"], capture_output=True, text=True)
    if head.returncode != 0:
        return None
    diff = subprocess.run(["git", "-C", str(directory), "diff", "HEAD"], capture_output=True)
    status = subprocess.run(["git", "-C", str(directory), "status", "--short"], capture_output=True, text=True)
    return {
        "path": str(directory),
        "head": head.stdout.strip(),
        "dirty_files": len([line for line in status.stdout.splitlines() if line.strip()]),
        "diff_sha256": hashlib.sha256(diff.stdout).hexdigest()[:16],
    }


def save(project, name, root=DEFAULT_ROOT, meta=None, git_dirs=()):
    """Snapshot `project` as `root/name`; returns the metadata written."""
    project = Path(project).resolve()
    target = Path(root) / name
    if target.exists():
        raise FileExistsError(f"snapshot {name} already exists at {target}")
    clone_tree(project, target / "project")
    drop_live_files(target / "project")
    metadata = {
        "name": name,
        "source": str(project),
        "saved_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "tasks": [task_summary(journal) for _, journal in tasks(target / "project")],
        "git": [state for state in (git_state(d) for d in git_dirs) if state],
        "meta": dict(meta or {}),
    }
    (target / "snapshot.json").write_text(json.dumps(metadata, indent=1) + "\n")
    return metadata


def rewrite_roots(project, old, new):
    """Point every task journal whose root is `old` (however it was spelled:
    macOS /var is /private/var) at `new`; the rest of the journal is kept byte
    for byte where possible."""
    changed = 0
    for path, journal in tasks(project):
        recorded = journal.get("root")
        if not isinstance(recorded, str) or Path(recorded).resolve() != Path(old).resolve():
            continue
        replaced = path.read_text().replace(json.dumps(recorded), json.dumps(new), 1)
        if json.loads(replaced).get("root") != new:
            journal["root"] = new
            replaced = json.dumps(journal)
        path.write_text(replaced)
        changed += 1
    return changed


def set_port(project, port):
    """Point `[daemon].http_addr` at the port, adding the setting (or the
    table) when the project has none."""
    config = Path(project) / "moosedev.toml"
    text = config.read_text() if config.exists() else ""
    address = f'http_addr = "127.0.0.1:{port}"'
    updated, count = re.subn(r'(?m)^\s*http_addr\s*=\s*"[^"]*"', address, text)
    if not count:
        if re.search(r"(?m)^\[daemon\]\s*$", text):
            updated = re.sub(r"(?m)^\[daemon\]\s*$", "[daemon]\n" + address, text, count=1)
        else:
            updated = text.rstrip("\n") + ("\n\n" if text.strip() else "") + "[daemon]\n" + address + "\n"
    config.write_text(updated)


def drop_path_caches(project):
    """The code index and build caches hold absolute paths of the old root."""
    data = Path(project) / ".moosedev"
    shutil.rmtree(data / "substrate", ignore_errors=True)
    for cache in ("lsp", "scratch"):
        shutil.rmtree(data / "harness" / cache, ignore_errors=True)


def restore(name, dest, port, root=DEFAULT_ROOT, exe=None, index=True, start=True, log=None, wait=120):
    """Copy snapshot `name` to `dest` and make it live there. Returns the
    daemon's pid when started, else None."""
    snapshot = Path(root) / name
    metadata = json.loads((snapshot / "snapshot.json").read_text())
    dest = Path(dest).resolve()
    socket = dest / ".moosedev/moosedev.sock"
    if len(str(socket).encode()) >= SOCKET_PATH_LIMIT:
        raise ValueError(f"{dest} is too deep for the daemon's Unix socket ({len(str(socket))} bytes; "
                         f"under {SOCKET_PATH_LIMIT} needed): restore to a shorter path")
    clone_tree(snapshot / "project", dest)
    rewrite_roots(dest, metadata["source"], str(dest))
    set_port(dest, port)
    if str(dest) != metadata["source"]:
        drop_path_caches(dest)
    if exe is None:
        if index or start:
            raise ValueError("restoring with an index or a daemon needs --exe")
        return None
    # The restored daemon takes its address from moosedev.toml; an inherited
    # MOOSEDEV_HTTP_ADDR would override it.
    exe = Path(exe).resolve()  # the commands below run inside the restored copy
    env = {key: value for key, value in os.environ.items() if key != "MOOSEDEV_HTTP_ADDR"}
    env["MOOSEDEV_DATA_DIR"] = ".moosedev"
    if index:
        built = subprocess.run([str(exe), "index"], cwd=dest, env=env, capture_output=True, text=True)
        if built.returncode != 0:
            raise RuntimeError(f"moosedev index failed: {built.stderr[-800:]}")
    if not start:
        return None
    log = Path(log) if log else dest.parent / f"{dest.name}.daemon.log"
    with open(log, "ab") as stream:
        daemon = subprocess.Popen([str(exe), "--serve", str(dest / ".moosedev/moosedev.sock")], cwd=dest, env=env,
                                  stdout=stream, stderr=stream, stdin=subprocess.DEVNULL, start_new_session=True)
    # Ready once its health endpoint answers: the address file is written
    # before the HTTP server accepts connections.
    address = dest / ".moosedev/http.addr"
    deadline = time.monotonic() + wait
    while True:
        if daemon.poll() is not None:
            raise RuntimeError(f"the daemon exited with {daemon.returncode}; see {log}")
        if time.monotonic() > deadline:
            stop(daemon.pid)
            raise TimeoutError(f"the daemon was not ready after {wait}s; see {log}")
        if address.exists() and healthy(address.read_text().strip()):
            return daemon.pid
        time.sleep(0.5)


def healthy(address):
    try:
        with urllib.request.urlopen(f"http://{address}/api/v1/health", timeout=2) as response:
            return response.status == 200
    except (OSError, ValueError):
        return False


def stop(pid, wait=15):
    """Stop a daemon and wait for it to exit (its port and store are then
    free); kill it if it has not exited in `wait` seconds."""
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + wait
    while time.monotonic() < deadline:
        try:
            done, _ = os.waitpid(pid, os.WNOHANG)
            if done:
                return
        except ChildProcessError:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
        time.sleep(0.2)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def listing(root=DEFAULT_ROOT):
    rows = []
    for meta in sorted(Path(root).glob("*/snapshot.json")):
        data = json.loads(meta.read_text())
        rows.append({"name": data["name"], "saved_at": data.get("saved_at"), "source": data.get("source"),
                     "tasks": [(t.get("phase"), t.get("events")) for t in data.get("tasks", [])],
                     "meta": data.get("meta", {})})
    return rows
