"""Explicit offline SCIP identity and matched indexed-dossier study readiness."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import tempfile
import uuid
from contextlib import contextmanager

from .artifacts import canonical_json, sha256_file
from .binaries import REPO
from .intent import OVERLAY, RESOLUTION_TARGETS, SEED_ASSOCIATIONS, SCENARIOS as INTENT_SCENARIOS
from .isolation import _checked_path, sandbox_command
from .scenario import SCENARIOS, MAINTENANCE, load_scenario, starts_empty, tree_manifest
from .seed import prepare_workspace, seed_iri

SYSTEM_PYTHON = Path("/usr/bin/python3")


def system_python_identity(expected=None):
    """Identify the OS interpreter selected by the indexer's controlled PATH."""
    command = [str(SYSTEM_PYTHON), "-c",
               "import json,os,sys; print(json.dumps({'executable':os.path.realpath(sys.executable),'version':sys.version}))"]
    result = json.loads(subprocess.check_output(command, text=True, timeout=10,
                                               env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}))
    executable = Path(result["executable"])
    if not executable.is_absolute() or executable.resolve(strict=True) != executable or not executable.is_file():
        raise ValueError("system Python did not report a resolved regular executable")
    identity = {"schema_version": 1, "launcher": str(SYSTEM_PYTHON),
                "launcher_sha256": sha256_file(SYSTEM_PYTHON), "executable": str(executable),
                "executable_sha256": sha256_file(executable), "version": result["version"]}
    if expected is not None and expected != identity:
        raise ValueError("system Python interpreter changed after intent preflight")
    return identity


def verify_indexer(value):
    manifest = json.loads(Path(value).read_text()) if isinstance(value, (str, Path)) else value
    if manifest.get("schema_version") != 1:
        raise ValueError("unknown frozen indexer schema")
    root = _checked_path(Path(manifest["directory"]), directory=True)
    if not root.is_relative_to(REPO / "target"):
        raise ValueError("frozen indexer must be inside repository target")
    def file(name, digest=None):
        path = _checked_path(Path(name))
        if not path.is_relative_to(root) or not path.is_file():
            raise ValueError("indexer input must be a regular file in frozen directory")
        if digest is not None and sha256_file(path) != digest:
            raise ValueError(f"indexer input hash mismatch: {path}")
        return path
    node = file(manifest["node"]["path"], manifest["node"]["sha256"])
    launcher = file(manifest["launcher"]["path"], manifest["launcher"]["sha256"])
    if not os.access(node, os.X_OK) or not os.access(launcher, os.X_OK):
        raise ValueError("frozen node and launcher must be executable")
    producer = manifest["producer"]
    package = _checked_path(Path(producer["package_root"]), directory=True)
    if not package.is_relative_to(root) or tree_manifest(package) != producer["files"]:
        raise ValueError("SCIP package/dependency tree differs from frozen identity")
    entry = file(producer["entry_point"])
    package_json = file(producer["package_json"])
    if not entry.is_relative_to(package) or not package_json.is_relative_to(package):
        raise ValueError("SCIP entry and package metadata must belong to dependency tree")
    if json.loads(package_json.read_text())["version"] != producer["version"]:
        raise ValueError("SCIP package version mismatch")
    expected = f'#!/bin/sh\nexec {shlex.quote(str(node))} {shlex.quote(str(entry))} "$@"\n'
    if launcher.read_text() != expected:
        raise ValueError("SCIP launcher must invoke exactly the frozen absolute Node and entry point")
    for path, digest in manifest.get("runtime_files", {}).items():
        file(path, digest)
    version = subprocess.check_output([str(node), "--version"], text=True, timeout=10,
                                     env={"PATH": "/usr/bin:/bin"}).strip()
    if version != manifest["node"]["version"]:
        raise ValueError("frozen Node version mismatch")
    return manifest


def apply_overlay(workspace):
    path = workspace / "pyproject.toml"
    # Maintenance already includes its approved manifest; existing cases need detection.
    if not path.exists():
        path.write_text(OVERLAY["python_manifest"])


def index_workspace(indexer, executable, workspace, runtime):
    verify_indexer(indexer)
    runtime.mkdir(parents=True, exist_ok=True)
    for child in ("home", "tmp"):
        (runtime / child).mkdir(exist_ok=True)
    environment = {"HOME": str(runtime / "home"), "TMPDIR": str(runtime / "tmp"),
                   "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
                   "MOOSEDEV_DATA_DIR": str(workspace / ".moosedev"),
                   "MOOSEDEV_SCIP_PYTHON": indexer["launcher"]["path"]}
    command = sandbox_command([str(executable), "index"], workspace=workspace, runtime=runtime,
                              readable_paths=[Path(executable), Path(indexer["directory"])], network_endpoints=[])
    try:
        result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, text=True, timeout=180)
        receipt = {"command": command, "environment": environment, "returncode": result.returncode,
                   "stdout": result.stdout, "stderr": result.stderr, "timed_out": False}
    except subprocess.TimeoutExpired as error:
        def text(value):
            return value.decode(errors="replace") if isinstance(value, bytes) else (value or "")
        receipt = {"command": command, "environment": environment, "returncode": None,
                   "stdout": text(error.stdout), "stderr": text(error.stderr), "timed_out": True}
    (runtime / "index-receipt.json").write_bytes(canonical_json(receipt))
    if receipt["returncode"] != 0:
        raise RuntimeError(f"confined indexing failed; inspect {runtime / 'index-receipt.json'}")
    substrate = workspace / ".moosedev/substrate"
    if not substrate.is_dir():
        raise RuntimeError("indexer reported success without substrate artifacts")
    receipt["substrate_files"] = tree_manifest(substrate)
    return receipt


def _entity(response, target):
    terminal = "/" + target["name"].replace(".", "#") + "()."
    matches = [entity for entity in response.get("entities", [])
               if entity.get("file") == target["file"] and (entity.get("name") == target["name"]
                   or entity.get("symbol", "").endswith(terminal))]
    if len(matches) != 1:
        raise RuntimeError(f"expected exactly one indexed entity: {target}; observed {len(matches)}")
    entity = matches[0]
    if not entity.get("symbol") or not entity.get("source_digest"):
        raise RuntimeError("resolved entity lacks indexed source proof")
    return entity


def resolution_tables(scenario_id):
    """Reviewed targets and seed associations: the sealed intent tables first, then long-horizon."""
    from . import long_horizon
    for targets, associations in ((RESOLUTION_TARGETS, SEED_ASSOCIATIONS),
                                  (long_horizon.RESOLUTION_TARGETS, long_horizon.SEED_ASSOCIATIONS)):
        if scenario_id in targets:
            return targets[scenario_id], associations[scenario_id]
    raise KeyError(f"no reviewed resolution targets for scenario: {scenario_id}")


def resolve_offline(root, target):
    """Without an indexer: a target must be exactly one module function or class method."""
    import ast
    from .scenario import relative_file
    path = relative_file(Path(root), target["file"])
    tree = ast.parse(path.read_text(), filename=str(path))
    owner, _, name = target["name"].rpartition(".")
    functions = (ast.FunctionDef, ast.AsyncFunctionDef)
    if owner:
        matches = [item for node in tree.body if isinstance(node, ast.ClassDef) and node.name == owner
                   for item in node.body if isinstance(item, functions) and item.name == name]
    else:
        matches = [node for node in tree.body if isinstance(node, functions) and node.name == name]
    if len(matches) != 1:
        raise RuntimeError(f"expected exactly one definition: {target}; observed {len(matches)}")
    return {"file": target["file"], "name": target["name"], "line": matches[0].lineno}


def ready_dossiers(daemon, scenario, *, seed=False, require_empty=False, targets=None, associations=None):
    default_targets, default_associations = resolution_tables(scenario["id"])
    targets = default_targets if targets is None else targets
    associations = default_associations if associations is None else associations
    files = sorted({target["file"] for target in targets})
    resolved = daemon._request("/api/v1/harness/intent/resolve", {"files": files, "refresh_index": False})
    for target in targets:
        _entity(resolved, target)
    operations = []
    if seed:
        bindings = []
        for association in associations:
            entity = _entity(resolved, association)
            bindings.append({"record_iri": seed_iri(scenario["id"] + "/fact/" + association["fact"]),
                             "file": entity["file"], "symbol": entity["symbol"],
                             "source_digest": entity["source_digest"]})
        if bindings:
            operation = str(uuid.uuid4())
            request = {"operation_id": operation, "revision": resolved["revision"], "bindings": bindings}
            staged = daemon._request("/api/v1/harness/intent/link", request)
            if staged.get("unresolved"):
                raise RuntimeError("reviewed seed associations failed indexed resolution")
            reviewed = daemon._request("/api/v1/harness/intent/review", {"operation_id": operation, "accept": True})
            if reviewed.get("conforms") is not True or reviewed.get("durable") is not True or reviewed.get("pending"):
                raise RuntimeError("reviewed seed associations did not reach a valid durable checkpoint")
            operations.append({"request": request, "staged": staged, "reviewed": reviewed})
            resolved = daemon._request("/api/v1/harness/intent/resolve", {"files": files, "refresh_index": False})
    if seed:
        for association in associations:
            entity = _entity(resolved, association)
            expected = seed_iri(scenario["id"] + "/fact/" + association["fact"])
            if expected not in entity.get("dossier_records", []):
                raise RuntimeError(f"seeded entity dossier lacks expected accepted record: {expected}")
    if require_empty:
        if resolved.get("records") or any(_entity(resolved, target).get("dossier_records") for target in targets):
            raise RuntimeError("accumulation ledger must start without knowledge or canary records")
    return {"resolution": resolved, "seed_operations": operations,
            "initial_knowledge": "expected_empty" if require_empty else "indexed_and_linked"}


@contextmanager
def short_probe_runtime(evidence):
    """Socket rendezvous stays short; useful regular-file evidence survives cleanup."""
    with tempfile.TemporaryDirectory(prefix="mdix-", dir="/private/tmp") as temporary:
        runtime = Path(temporary)
        try:
            yield runtime
        finally:
            for parent, directories, files in os.walk(runtime, followlinks=False):
                directories[:] = [name for name in directories if not (Path(parent) / name).is_symlink()]
                for name in files:
                    source = Path(parent) / name
                    if stat.S_ISREG(source.lstat().st_mode):
                        destination = evidence / source.relative_to(runtime)
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        shutil.copyfile(source, destination)


def probe_indexer(indexer, binaries, assets, *, evolution_contract=False, scenarios=INTENT_SCENARIOS):
    """Prove every initial source contract in disposable, disjoint workspaces."""
    from .daemon import OwnedDaemon
    directory = REPO / "target/harness-study/indexer-probes" / str(uuid.uuid4())
    directory.mkdir(parents=True)
    result = {"directory": str(directory), "scenarios": {}, "passed": False}
    try:
        for name in scenarios:
            evidence = directory / name
            workspace = evidence / "workspace"
            shutil.copytree(SCENARIOS / name / "project", workspace)
            subprocess.run(["/usr/bin/git", "init", "-q", str(workspace)], check=True, capture_output=True)
            scenario = load_scenario(name)
            prepare_workspace(workspace, scenario, "harness")
            apply_overlay(workspace)
            observed = result["scenarios"][name] = {}
            observed["index"] = index_workspace(indexer, binaries["binaries"]["daemon"],
                                                  workspace, evidence / "index-runtime")
            with short_probe_runtime(evidence / "daemon-runtime") as runtime:
                with OwnedDaemon(executable=Path(binaries["binaries"]["daemon"]),
                                 expected_sha256=binaries["binary_hashes"]["daemon"], workspace=workspace,
                                 runtime=runtime, assets=Path(assets["directory"]), helper_model="unused-no-inference",
                                 helper_endpoint="http://127.0.0.1:9/v1", log_path=evidence / "daemon.log",
                                 indexer=indexer) as daemon:
                    observed["daemon"] = daemon.identity
                    observed["dossiers"] = ready_dossiers(daemon, scenario, seed=True,
                                                          require_empty=starts_empty(scenario))
                    if evolution_contract and name == MAINTENANCE:
                        from .evolution_probe import probe_evolution
                        observed["evolution"] = {}
                        probe_evolution(daemon, workspace, scenario, observed["evolution"])
                    observed["checkpoint"] = daemon.checkpoint()
            observed["canary_graph_sha256"] = sha256_file(workspace / ".moosedev/kg.nq")
        result["passed"] = True
        return result
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        raise RuntimeError(f"confined indexing/dossier probe failed; inspect {directory / 'result.json'}: {error}") from error
    finally:
        (directory / "result.json").write_bytes(canonical_json(result))
