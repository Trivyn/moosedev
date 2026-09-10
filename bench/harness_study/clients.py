"""Copy selected CLI runtimes into target so agent grants never expose installs."""

import fcntl
import hashlib
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

from .artifacts import (_directory, _publish, _read_json, _regular_open,
                        _sync_directory, canonical_json, sha256_file)
from .binaries import REPO


def _entry(path):
    mode = path.lstat().st_mode
    if stat.S_ISLNK(mode):
        return {"type": "symlink", "target": os.readlink(path)}
    if stat.S_ISDIR(mode):
        return {"type": "directory"}
    if stat.S_ISREG(mode):
        return {"type": "file", "sha256": sha256_file(path), "mode": stat.S_IMODE(mode) & ~0o222}
    raise ValueError(f"client runtime contains nonregular entry: {path}")


def _tree(root, *, validate_links=False, exclude_manifest=False):
    entries = {}
    for parent, directories, files in os.walk(root, followlinks=False):
        for name in sorted(directories + files):
            path = Path(parent) / name
            relative = path.relative_to(root).as_posix()
            if exclude_manifest and relative == "manifest.json":
                continue
            entry = _entry(path)
            if validate_links and entry["type"] == "symlink":
                if not path.resolve(strict=True).is_relative_to(root):
                    raise ValueError(f"client runtime link escapes frozen assets: {relative}")
            entries[relative] = entry
    return entries


def _assets(executable):
    for ancestor in executable.parents:
        if ancestor.name == "node_modules" and ancestor.parent.name == "lib":
            installation = ancestor.parent.parent
            node = installation / "bin/node"
            if not node.is_file() or not os.access(node, os.X_OK):
                raise ValueError("selected npm CLI requires its installation's Node executable")
            assets = {"bin/node": node.resolve(strict=True)}
            for package in ("@openai", "opencode-ai"):
                path = ancestor / package
                if path.exists():
                    _directory(path)
                    assets[f"lib/node_modules/{package}"] = path
            relative = executable.relative_to(installation)
            if not any(relative.is_relative_to(name) for name in assets):
                raise ValueError("only the selected OpenAI/OpenCode npm runtimes may be frozen")
            return relative, assets
    return Path("bin") / executable.name, {f"bin/{executable.name}": executable}


def _asset_tree(assets):
    entries = {}
    for name, path in assets.items():
        relative = Path(name)
        for parent in relative.parents:
            if parent.parts:
                entries[parent.as_posix()] = {"type": "directory"}
        entries[name] = _entry(path)
        if path.is_dir():
            entries.update({f"{name}/{key}": value for key, value in _tree(path).items()})
    return entries


def _verify_runtime(directory):
    directory = _directory(directory)
    parent = _directory(REPO / "target/harness-study/clients")
    if directory.parent != parent:
        raise ValueError("client runtime must be a direct child of repository target/harness-study/clients")
    manifest = _read_json(directory / "manifest.json")
    identity = {"schema_version": manifest["schema_version"], "files": manifest["files"]}
    digest = hashlib.sha256(canonical_json(identity)).hexdigest()
    if manifest["schema_version"] != 1 or manifest.get("runtime_sha256") != digest or directory.name != digest:
        raise ValueError("client runtime manifest identity mismatch")
    if _tree(directory, validate_links=True, exclude_manifest=True) != manifest["files"]:
        raise ValueError("frozen client runtime changed")
    return manifest


def verify_client(identity):
    """Verify complete runtime, including symlink targets, without running code."""
    directory = Path(identity["runtime_root"])
    manifest = _verify_runtime(directory)
    path = Path(identity["path"])
    if path.resolve(strict=True) != path or not path.is_relative_to(directory):
        raise ValueError("frozen client executable must be a canonical runtime file")
    if not path.is_file() or not os.access(path, os.X_OK) or sha256_file(path) != identity["sha256"]:
        raise ValueError("frozen client executable identity mismatch")
    expected = {str(directory / name): entry["sha256"] for name, entry in manifest["files"].items()
                if entry["type"] == "file"}
    if identity["runtime_files"] != expected or identity.get("runtime_sha256") != manifest["runtime_sha256"]:
        raise ValueError("frozen client runtime inventory mismatch")
    return identity


def freeze_client(executable: Path) -> dict:
    requested = Path(executable)
    if not requested.is_absolute():
        raise ValueError("client executable must be an explicit absolute path")
    source = requested.resolve(strict=True)
    if not stat.S_ISREG(source.lstat().st_mode) or not os.access(source, os.X_OK):
        raise ValueError("client executable must be an executable regular file")
    relative, assets = _assets(source)
    before = _asset_tree(assets)
    identity = {"schema_version": 1, "files": before}
    digest = hashlib.sha256(canonical_json(identity)).hexdigest()
    parent = _directory(REPO / "target/harness-study/clients", create=True)
    directory = parent / digest
    with os.fdopen(_regular_open(parent / ".freeze.lock", os.O_RDWR | os.O_CREAT), "r+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not os.path.lexists(directory):
            staging = Path(tempfile.mkdtemp(prefix=".freeze-", dir=parent))
            for name, asset in assets.items():
                destination = staging / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                if asset.is_dir():
                    shutil.copytree(asset, destination, symlinks=True)
                else:
                    shutil.copyfile(asset, destination)
                    destination.chmod(stat.S_IMODE(asset.stat().st_mode))
            copied = _tree(staging, validate_links=True)
            if before != copied or before != _asset_tree(assets):
                raise RuntimeError("client installation changed during freezing")
            for name, entry in copied.items():
                if entry["type"] == "file":
                    (staging / name).chmod(entry["mode"])
                    with (staging / name).open("rb") as stream:
                        os.fsync(stream.fileno())
            _publish(staging / "manifest.json", canonical_json(dict(identity, runtime_sha256=digest)))
            (staging / "manifest.json").chmod(0o400)
            _sync_directory(staging)
            os.rename(staging, directory)
            _sync_directory(parent)
        manifest = _verify_runtime(directory)
    frozen = directory / relative
    if frozen.resolve(strict=True) != frozen:
        raise ValueError("frozen executable must not be an alias")
    with tempfile.TemporaryDirectory(prefix="md-client-version-") as temporary:
        private = Path(temporary).resolve()
        environment = {"PATH": str(directory / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin",
                       "HOME": str(private), "TMPDIR": str(private),
                       "OPENCODE_DISABLE_AUTOUPDATE": "1", "OPENCODE_DISABLE_MODELS_FETCH": "1"}
        result = subprocess.run([str(frozen), "--version"], cwd=private, env=environment,
                                capture_output=True, timeout=30)
    version = result.stdout.decode(errors="replace").strip()
    if result.returncode or not version:
        raise ValueError(f"frozen client version probe failed: {frozen}: " + result.stderr.decode(errors="replace"))
    return verify_client({"path": str(frozen), "source_path": str(source), "sha256": sha256_file(frozen),
                          "version": version, "runtime_root": str(directory), "runtime_sha256": digest,
                          "runtime_files": {str(directory / name): entry["sha256"]
                                            for name, entry in manifest["files"].items() if entry["type"] == "file"}})
