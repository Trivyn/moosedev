"""Freeze identified repository builds; never resolve MOOSEDev through PATH."""
from __future__ import annotations

import fcntl
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile
import uuid

from .artifacts import (_directory, _publish, _regular_open, _sync_directory,
                        canonical_json, sha256_file)

REPO = Path(__file__).resolve().parents[2]
BINARIES = {"daemon": "moosedev", "harness": "moosedev-harness",
            "session": "examples/harness_study_session"}
IDENTITY_FIELDS = ("schema_version", "profile", "features", "source", "engine_source",
                   "binary_hashes", "build_receipt")


def git_output(repo: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(repo), *args])


def _relative(name):
    path = Path(name)
    if not path.parts or path.is_absolute() or ".." in path.parts:
        raise ValueError(f"unsafe source or receipt path: {name}")
    return path


def _read_file(path):
    _directory(path.parent)
    with os.fdopen(_regular_open(path), "rb") as source:
        return source.read()


def source_identity(repo: Path) -> dict:
    """Bind tracked AND new inputs; symlinks are recorded, never dereferenced."""
    repo = _directory(repo)
    raw = git_output(repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    files = {}
    for name in sorted(set(raw.decode().split("\0")) - {""}):
        path = repo / _relative(name)
        try:
            _directory(path.parent)
            metadata = path.lstat()
        except FileNotFoundError:
            files[name] = {"deleted": True}
            continue
        if stat.S_ISLNK(metadata.st_mode):
            files[name] = {"symlink": os.readlink(path)}
        elif stat.S_ISREG(metadata.st_mode):
            files[name] = {"sha256": sha256_file(path), "mode": stat.S_IMODE(metadata.st_mode)}
        else:
            raise ValueError(f"source input must be a regular file or explicit symlink: {name}")
    return {"commit": git_output(repo, "rev-parse", "HEAD").decode().strip(),
            "files": files, "tree_sha256": hashlib.sha256(canonical_json(files)).hexdigest()}


def _source_archive(repo, identity, destination):
    """Canonical metadata avoids host timestamps/users in reproducible snapshots."""
    with destination.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as archive:
                for name, entry in sorted(identity["files"].items()):
                    if entry.get("deleted"):
                        continue
                    path = repo / _relative(name)
                    _directory(path.parent)
                    member = tarfile.TarInfo(name)
                    if "symlink" in entry:
                        if not path.is_symlink() or os.readlink(path) != entry["symlink"]:
                            raise RuntimeError("source changed while archiving")
                        member.type, member.linkname, member.mode = tarfile.SYMTYPE, entry["symlink"], 0o777
                        archive.addfile(member)
                    else:
                        data = _read_file(path)
                        if (hashlib.sha256(data).hexdigest() != entry["sha256"]
                                or stat.S_IMODE(path.lstat().st_mode) != entry["mode"]):
                            raise RuntimeError("source changed while archiving")
                        member.mode, member.size = entry["mode"], len(data)
                        archive.addfile(member, io.BytesIO(data))
        raw.flush()
        os.fsync(raw.fileno())


def build_and_freeze(repo: Path = REPO) -> dict:
    """Own the build and retain complete private inputs, even on build failure."""
    repo = _directory(repo)
    target = _directory(repo / "target", create=True)
    before = source_identity(repo)
    engine = repo.parent / "moose"
    dependency = source_identity(engine) if (engine / ".git").exists() else None
    attempts = _directory(target / "harness-study" / "build-attempts", create=True)
    attempt = _directory(attempts / str(uuid.uuid4()), create=True)
    _publish(attempt / "source-identity.json", canonical_json({"source": before, "engine_source": dependency}))
    _source_archive(repo, before, attempt / "source.tar.gz")
    if dependency:
        _source_archive(engine, dependency, attempt / "engine-source.tar.gz")
    _publish(attempt / "source.patch", git_output(repo, "diff", "--binary", "HEAD"))
    if dependency:
        _publish(attempt / "engine-source.patch", git_output(engine, "diff", "--binary", "HEAD"))
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError(f"cargo is unavailable; source evidence retained at {attempt}")
    # Rustup dispatches on argv[0]; resolving the cargo symlink invokes rustup
    # itself, which interprets "build" as an unknown rustup subcommand.
    command = [str(Path(cargo).absolute()), "build", "--release", "--locked", "--features", "harness",
               "--bins", "--example", "harness_study_session"]
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target)
    _publish(attempt / "build-command.json", canonical_json({"command": command, "cwd": str(repo),
             "environment": {key: env.get(key) for key in ("CARGO_TARGET_DIR", "RUSTFLAGS",
                  "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET")}}))
    result = subprocess.run(command, cwd=repo, env=env, capture_output=True)
    for name, data in (("build.stdout", result.stdout), ("build.stderr", result.stderr)):
        _publish(attempt / name, data)
    _publish(attempt / "build-result.json", canonical_json({"returncode": result.returncode}))
    if result.returncode:
        raise RuntimeError(f"release build failed; evidence retained at {attempt}:\n"
                           + result.stderr.decode(errors="replace"))
    if before != source_identity(repo) or (dependency and dependency != source_identity(engine)):
        raise RuntimeError(f"source changed during the build; evidence retained at {attempt}")
    evidence = {path.name: path for path in sorted(attempt.iterdir())}
    return freeze_binaries(repo, before, dependency, evidence=evidence)


def freeze_binaries(repo: Path, source: dict, dependency: dict | None = None, *, evidence=None) -> dict:
    repo = _directory(repo)
    target = _directory(repo / "target")
    source_paths, hashes = {}, {}
    for role, name in BINARIES.items():
        path = target / "release" / name
        _directory(path.parent)
        if not stat.S_ISREG(path.lstat().st_mode) or not os.access(path, os.X_OK):
            raise ValueError(f"missing real executable repository artifact: {path}")
        source_paths[role], hashes[role] = path, sha256_file(path)
    receipt_files = {}
    for name, path in (evidence or {}).items():
        if _relative(name).name != name or name in {"manifest.json", *[Path(p).name for p in BINARIES.values()]}:
            raise ValueError("receipt files must have unique plain filenames")
        data = _read_file(Path(path))
        receipt_files[name] = {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data)}
    receipt = {"schema_version": 1, "files": receipt_files} if evidence else None
    identity = {"schema_version": 1, "profile": "release", "features": ["harness"],
                "source": source, "engine_source": dependency, "binary_hashes": hashes, "build_receipt": receipt}
    build_id = hashlib.sha256(canonical_json(identity)).hexdigest()
    parent = _directory(target / "harness-study" / "bin", create=True)
    destination = parent / build_id
    manifest = {**identity, "build_id": build_id, "directory": str(destination),
                "binaries": {role: str(destination / Path(BINARIES[role]).name) for role in hashes}}
    with os.fdopen(_regular_open(parent / ".freeze.lock", os.O_RDWR | os.O_CREAT), "r+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if os.path.lexists(destination):
            return verify_binaries(destination / "manifest.json", repo)
        staging = Path(tempfile.mkdtemp(prefix=".freeze-", dir=parent))
        # On interruption retain staging evidence, never erase an ambiguous build.
        for role, path in source_paths.items():
            copy = staging / Path(BINARIES[role]).name
            _publish(copy, _read_file(path))
            copy.chmod(0o500)
            if sha256_file(copy) != hashes[role] or sha256_file(path) != hashes[role]:
                raise RuntimeError("binary changed while freezing")
        for name, path in (evidence or {}).items():
            copy = staging / name
            _publish(copy, _read_file(Path(path)))
            if sha256_file(copy) != receipt_files[name]["sha256"]:
                raise RuntimeError("build evidence changed while freezing")
            copy.chmod(0o400)
        _publish(staging / "manifest.json", canonical_json(manifest))
        (staging / "manifest.json").chmod(0o400)
        _sync_directory(staging)
        os.rename(staging, destination)
        _sync_directory(parent)
    return verify_binaries(destination / "manifest.json", repo)


def verify_binaries(manifest_path: Path, repo: Path = REPO) -> dict:
    repo = _directory(repo)
    root = _directory(repo / "target" / "harness-study" / "bin")
    manifest_path = Path(manifest_path).absolute()
    manifest = json.loads(_read_file(manifest_path))
    directory = Path(manifest["directory"])
    if directory.parent != root:
        raise ValueError("frozen binaries must be direct children of repository target/harness-study/bin")
    _directory(directory)
    identity = {key: manifest[key] for key in IDENTITY_FIELDS}
    if identity["schema_version"] != 1 or identity["profile"] != "release" or identity["features"] != ["harness"]:
        raise ValueError("unexpected binary build configuration")
    if hashlib.sha256(canonical_json(identity)).hexdigest() != manifest["build_id"]:
        raise ValueError("build manifest identity mismatch")
    if directory.name != manifest["build_id"] or manifest_path != directory / "manifest.json":
        raise ValueError("build manifest location mismatch")
    if set(manifest["binaries"]) != set(BINARIES) or set(manifest["binary_hashes"]) != set(BINARIES):
        raise ValueError("build manifest must identify every study executable")
    expected_files = {"manifest.json"}
    for role, name in manifest["binaries"].items():
        path = Path(name)
        expected = directory / Path(BINARIES[role]).name
        if path != expected or not stat.S_ISREG(path.lstat().st_mode) or not os.access(path, os.X_OK):
            raise ValueError("frozen executable path mismatch")
        if sha256_file(path) != manifest["binary_hashes"][role]:
            raise ValueError(f"frozen binary hash mismatch: {role}")
        expected_files.add(path.name)
    receipt = manifest["build_receipt"]
    if receipt is not None:
        if receipt.get("schema_version") != 1:
            raise ValueError("unexpected build receipt schema")
        for name, entry in receipt["files"].items():
            if _relative(name).name != name or name in expected_files:
                raise ValueError("invalid build receipt filename")
            data = _read_file(directory / name)
            if hashlib.sha256(data).hexdigest() != entry["sha256"] or len(data) != entry["size"]:
                raise ValueError(f"build evidence hash mismatch: {name}")
            expected_files.add(name)
    if {path.name for path in directory.iterdir()} != expected_files:
        raise ValueError("unexpected or missing frozen build evidence")
    return manifest
