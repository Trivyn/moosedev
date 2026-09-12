"""Durable, append-only evidence storage. No model or candidate code is invoked."""

from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import uuid


def canonical_json(value):
    return (json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False,
                       separators=(",", ":")) + "\n").encode("utf-8")


def _now():
    return datetime.now(timezone.utc).isoformat()


def _regular_open(path, flags=os.O_RDONLY, mode=0o600):
    fd = os.open(path, flags | os.O_NOFOLLOW | os.O_NONBLOCK, mode)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError(f"not a regular artifact file: {path}")
    return fd


def sha256_file(path):
    digest = hashlib.sha256()
    with os.fdopen(_regular_open(path), "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _directory(path, create=False):
    """Reject symbolic links in every component, including existing ancestors."""
    path = Path(os.path.abspath(path))
    for ancestor in (*reversed(path.parents), path):
        if create:
            try:
                ancestor.mkdir(mode=0o700)
            except FileExistsError:
                pass
        if not stat.S_ISDIR(ancestor.lstat().st_mode):
            raise ValueError(f"artifact directory must be a real directory: {ancestor}")
    return path


def _sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _read_json(path):
    with os.fdopen(_regular_open(path), "rb") as stream:
        return json.load(stream)


def _iter_lines(path):
    """Yield one decoded record at a time; a streamed log is never materialised."""
    try:
        stream = os.fdopen(_regular_open(path), "rb")
    except FileNotFoundError:
        return
    with stream:
        # Check the tail first so an interrupted append is reported before any
        # record is decoded, exactly as the whole-file reader did.
        stream.seek(0, os.SEEK_END)
        position = stream.tell()
        if position:
            stream.seek(position - 1)
            if stream.read(1) != b"\n":
                raise ValueError(f"interrupted append retained in {path}; manual recovery required")
        stream.seek(0)
        for line in stream:
            yield json.loads(line)


def _read_lines(path):
    return list(_iter_lines(path))


def _append(path, value):
    # Validate an existing tail before appending; never truncate interrupted evidence.
    _last_entry(path)
    data = canonical_json(value)
    with os.fdopen(_regular_open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT), "ab") as out:
        out.write(data)
        out.flush()
        os.fsync(out.fileno())
    _sync_directory(path.parent)


def _last_entry(path):
    """Read only the final record so streamed logs do not grow quadratic in cost."""
    try:
        with os.fdopen(_regular_open(path), "rb") as stream:
            stream.seek(0, os.SEEK_END)
            position = stream.tell()
            if not position:
                return None
            stream.seek(position - 1)
            if stream.read(1) != b"\n":
                raise ValueError(f"interrupted append retained in {path}; manual recovery required")
            position -= 1
            chunks = []
            while position:
                length = min(position, 8192)
                position -= length
                stream.seek(position)
                data = stream.read(length)
                boundary = data.rfind(b"\n")
                chunks.append(data[boundary + 1:] if boundary >= 0 else data)
                if boundary >= 0:
                    break
            return json.loads(b"".join(reversed(chunks)))
    except FileNotFoundError:
        return None


def _publish(path, data):
    """Publish a complete file exclusively; a crash leaves its pending evidence."""
    try:
        with os.fdopen(_regular_open(path), "rb") as existing:
            if existing.read() == data:
                return
        raise FileExistsError(f"immutable artifact already has different contents: {path}")
    except FileNotFoundError:
        pass
    pending = path.parent / f".pending-{uuid.uuid4()}"
    with os.fdopen(_regular_open(pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL), "wb") as out:
        out.write(data)
        out.flush()
        os.fsync(out.fileno())
    # link() cannot replace an existing destination. Keep pending files on failure.
    os.link(pending, path, follow_symlinks=False)
    _sync_directory(path.parent)
    pending.unlink()
    _sync_directory(path.parent)


class ArtifactStore:
    """Caller must place this root outside all candidate-visible mounts."""

    def __init__(self, root):
        self.root = _directory(root, create=True)
        _directory(self.root / "runs", create=True)

    @contextmanager
    def locked(self):
        # Independent open descriptions make flock serialize threads and processes.
        with os.fdopen(_regular_open(self.root / ".lock", os.O_RDWR | os.O_CREAT), "r+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)

    def run_path(self, run_dir):
        path = Path(os.path.abspath(run_dir))
        if path.parent != self.root / "runs":
            raise ValueError("run must be a direct child of this store's runs directory")
        if str(uuid.UUID(path.name)) != path.name:
            raise ValueError("invalid run identity")
        return _directory(path)

    def create_run(self, manifest):
        if not isinstance(manifest, dict) or "run_id" in manifest or "created_at" in manifest:
            raise ValueError("manifest must be an object without generated identity fields")
        run_id = str(uuid.uuid4())
        manifest = dict(manifest, run_id=run_id, created_at=_now())
        data = canonical_json(manifest)
        with self.locked():
            run = self.root / "runs" / run_id
            run.mkdir(mode=0o700)
            _sync_directory(run.parent)
            _publish(run / "manifest.json", data)
            _append(self.root / "run_index.jsonl", {
                "event": "created", "run_id": run_id, "timestamp": manifest["created_at"],
                "manifest_sha256": hashlib.sha256(data).hexdigest(),
            })
        return run

    def _writable_run(self, run_dir):
        run = self.run_path(run_dir)
        if os.path.lexists(run / "seal.json"):
            raise ValueError("sealed runs are immutable")
        return run

    def append_event(self, run_dir, channel, payload):
        if not isinstance(channel, str) or not channel.strip():
            raise ValueError("event channel must be a nonempty string")
        with self.locked():
            run = self._writable_run(run_dir)
            path = run / "events.jsonl"
            last = _last_entry(path)
            if last is not None and not isinstance(last, dict):
                raise ValueError("event record is corrupt")
            sequence = last.get("sequence") if isinstance(last, dict) else 0
            if type(sequence) is not int or sequence < 0:
                raise ValueError("event sequence is corrupt")
            _append(path, {"sequence": sequence + 1, "timestamp": _now(),
                           "channel": channel, "payload": payload})

    def put_bytes(self, run_dir, relative_path, data):
        if not isinstance(data, bytes):
            raise TypeError("artifact data must be bytes")
        relative = Path(relative_path)
        if (relative.is_absolute() or not relative.parts or ".." in relative.parts
                or any(part.startswith(".pending-") for part in relative.parts)
                or relative.as_posix() in {"seal.json", "events.jsonl"}):
            raise ValueError("invalid or reserved artifact path")
        with self.locked():
            run = self._writable_run(run_dir)
            destination = run / relative
            _directory(destination.parent, create=True)
            _publish(destination, data)

    @staticmethod
    def _inventory(run):
        files = {}
        for parent, dirs, names in os.walk(run, followlinks=False):
            for name in sorted(dirs + names):
                path = Path(parent) / name
                mode = path.lstat().st_mode
                if name.startswith(".pending-"):
                    raise ValueError(f"interrupted publication retained: {path}")
                if stat.S_ISDIR(mode):
                    continue
                if not stat.S_ISREG(mode):
                    raise ValueError(f"nonregular evidence: {path}")
                relative = path.relative_to(run).as_posix()
                if relative == "seal.json":
                    continue
                files[relative] = {"sha256": sha256_file(path), "size": path.stat().st_size}
        return files

    def _verify(self, run):
        seal = _read_json(run / "seal.json")
        if seal.get("schema_version") != 1 or seal.get("run_id") != run.name:
            raise ValueError("invalid seal identity or version")
        files = self._inventory(run)
        if files != seal.get("files"):
            raise ValueError("run evidence changed: missing, unexpected, or modified artifact")
        digest = hashlib.sha256(canonical_json(files)).hexdigest()
        if digest != seal.get("evidence_sha256"):
            raise ValueError("invalid evidence digest")
        manifest = _read_json(run / "manifest.json")
        if manifest.get("run_id") != run.name:
            raise ValueError("manifest identity does not match run")
        entries = [entry for entry in _iter_lines(self.root / "run_index.jsonl")
                   if entry.get("run_id") == run.name]
        created = [entry for entry in entries if entry.get("event") == "created"]
        sealed = [entry for entry in entries if entry.get("event") == "sealed"]
        if (len(created) != 1 or len(sealed) != 1
                or created[0].get("manifest_sha256") != files["manifest.json"]["sha256"]
                or sealed[0].get("evidence_sha256") != digest):
            raise ValueError("run index does not match the sealed evidence")
        if "runtime-assets.json" in files:
            self._verify_runtime_assets(run)
        return seal

    def _verify_runtime_assets(self, run):
        references = _read_json(run / "runtime-assets.json")
        if (not isinstance(references, dict) or references.get("schema_version") != 1
                or not isinstance(references.get("assets"), list)):
            raise ValueError("invalid shared client archive references")
        seen = set()
        for reference in references["assets"]:
            if not isinstance(reference, dict):
                raise ValueError("shared client archive reference must be an object")
            digest = reference.get("runtime_sha256")
            if (not isinstance(digest, str) or len(digest) != 64
                    or any(char not in "0123456789abcdef" for char in digest) or digest in seen):
                raise ValueError("invalid or duplicate shared client runtime identity")
            seen.add(digest)
            if reference.get("path") != f"assets/{digest}.tar":
                raise ValueError("shared client archive must use its canonical asset path")
            path = self.root / reference["path"]
            _directory(path.parent)
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise ValueError("shared client archive must be a regular file without aliases")
            if (type(reference.get("size")) is not int or metadata.st_size != reference["size"]
                    or sha256_file(path) != reference.get("archive_sha256")):
                raise ValueError("shared client archive size or hash mismatch")

    def seal_run(self, run_dir):
        with self.locked():
            run = self.run_path(run_dir)
            if os.path.lexists(run / "seal.json"):
                return self._verify(run)
            manifest = _read_json(run / "manifest.json")
            if manifest.get("run_id") != run.name:
                raise ValueError("manifest identity does not match run")
            # Streamed: an event log can exceed memory; hold one record at a time.
            for index, event in enumerate(_iter_lines(run / "events.jsonl"), 1):
                if not isinstance(event, dict) or event.get("sequence") != index:
                    raise ValueError("event sequence is corrupt")
            files = self._inventory(run)
            seal = {"schema_version": 1, "run_id": run.name, "sealed_at": _now(),
                    "files": files, "evidence_sha256": hashlib.sha256(canonical_json(files)).hexdigest()}
            _publish(run / "seal.json", canonical_json(seal))
            _append(self.root / "run_index.jsonl", {"event": "sealed", "run_id": run.name,
                    "timestamp": seal["sealed_at"], "evidence_sha256": seal["evidence_sha256"]})
            return seal

    def verify_run(self, run_dir):
        with self.locked():
            return self._verify(self.run_path(run_dir))
