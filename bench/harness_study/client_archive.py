"""Deduplicated, portable private CLI runtime evidence for offline exports."""

import hashlib
import os
from pathlib import Path
import tarfile
import uuid

from .artifacts import (_directory, _read_json, _regular_open, _sync_directory,
                        canonical_json, sha256_file)
from .clients import verify_client


def _entries(root):
    manifest = _read_json(root / "manifest.json")
    entries = dict(manifest["files"])
    entries["manifest.json"] = {"type": "file", "sha256": sha256_file(root / "manifest.json"), "mode": 0o400}
    return entries


def _create_archive(root, destination, entries):
    with os.fdopen(_regular_open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL), "wb") as output:
        with tarfile.open(fileobj=output, mode="w|", format=tarfile.PAX_FORMAT) as archive:
            for name, entry in sorted(entries.items()):
                member = tarfile.TarInfo(name)
                path = root / name
                if entry["type"] == "directory":
                    member.type, member.mode = tarfile.DIRTYPE, 0o755
                    archive.addfile(member)
                elif entry["type"] == "symlink":
                    if not path.resolve(strict=True).is_relative_to(root):
                        raise ValueError("client archive refuses an escaping symbolic link")
                    member.type, member.mode, member.linkname = tarfile.SYMTYPE, 0o777, entry["target"]
                    archive.addfile(member)
                else:
                    _directory(path.parent)
                    with os.fdopen(_regular_open(path), "rb") as source:
                        member.mode, member.size = entry["mode"], os.fstat(source.fileno()).st_size
                        archive.addfile(member, source)
        output.flush()
        os.fsync(output.fileno())


def _verify_archive(path, entries):
    """Check contents without extraction before reusing a shared runtime archive."""
    seen = set()
    with os.fdopen(_regular_open(path), "rb") as stream, tarfile.open(fileobj=stream, mode="r|") as archive:
        for member in archive:
            name = member.name
            if name not in entries or name in seen:
                raise ValueError("client archive has an unexpected or duplicate member")
            seen.add(name)
            entry = entries[name]
            expected_mode = entry.get("mode", 0o755 if entry["type"] == "directory" else 0o777)
            if (member.mtime != 0 or member.uid != 0 or member.gid != 0 or member.uname
                    or member.gname or member.mode != expected_mode):
                raise ValueError("client archive metadata is not canonical")
            if entry["type"] == "directory":
                valid = member.isdir()
            elif entry["type"] == "symlink":
                valid = member.issym() and member.linkname == entry["target"]
            else:
                valid = member.isfile()
                if valid:
                    digest = hashlib.sha256()
                    with archive.extractfile(member) as source:
                        for chunk in iter(lambda: source.read(1024 * 1024), b""):
                            digest.update(chunk)
                    valid = digest.hexdigest() == entry["sha256"]
            if not valid:
                raise ValueError(f"client archive content mismatch: {name}")
    if seen != set(entries):
        raise ValueError("client archive is missing runtime files")


def archive_clients(store, run, identities: list[dict]) -> None:
    """Archive each runtime once; bind its immutable digest into the run bundle."""
    selected = {}
    for identity in identities:
        verify_client(identity)
        selected.setdefault(identity["runtime_sha256"], identity)
    references = []
    with store.locked():
        store._writable_run(run)
        assets = _directory(store.root / "assets", create=True)
        for runtime_sha256, identity in sorted(selected.items()):
            if len(runtime_sha256) != 64 or any(char not in "0123456789abcdef" for char in runtime_sha256):
                raise ValueError("runtime digest must be a SHA-256 value")
            root = Path(identity["runtime_root"])
            entries = _entries(root)
            destination = assets / f"{runtime_sha256}.tar"
            if not os.path.lexists(destination):
                pending = assets / f".pending-{uuid.uuid4()}"
                _create_archive(root, pending, entries)
                verify_client(identity)
                _verify_archive(pending, entries)
                # Keep interrupted files on failure; publish exclusively.
                os.link(pending, destination, follow_symlinks=False)
                _sync_directory(assets)
                pending.unlink()
                _sync_directory(assets)
            else:
                if destination.lstat().st_nlink != 1:
                    raise ValueError("shared client archive must not have hardlink aliases")
                _verify_archive(destination, entries)
                verify_client(identity)
            references.append({"runtime_sha256": runtime_sha256,
                               "path": destination.relative_to(store.root).as_posix(),
                               "archive_sha256": sha256_file(destination), "size": destination.stat().st_size})
    store.put_bytes(run, "runtime-assets.json", canonical_json({"schema_version": 1, "assets": references}))
