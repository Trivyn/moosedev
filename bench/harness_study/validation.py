"""Execute tiny fixture checks in disposable, network-denied grading sandboxes."""

import base64
import os
from pathlib import Path
import platform
import re
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import time

from .isolation import sandbox_command
from .scenario import SCENARIOS, list_scenarios, load_scenario, relative_file


EXCLUDED = {".git", ".moosedev", "notes", "runtime", "__pycache__", ".cache",
            ".pytest_cache", ".mypy_cache", ".venv", "venv", "node_modules", "target"}
TEST_COUNT = re.compile(r"^Ran (\d+) tests? in .+$", re.MULTILINE)
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")
# Python 3.9: "test_x (__main__.Case) ... ok"; 3.11+: "test_x (__main__.Case.test_x) ... ok".
TEST_RESULT = re.compile(r"^(test\w*) \(([\w.]+)\) \.\.\. (ok|FAIL|ERROR|skipped\b.*|expected failure|unexpected success)$",
                         re.MULTILINE)


def test_results(output):
    """Per-test outcomes of a verbosity-2 unittest run, keyed "Case.test_method"."""
    results = {}
    for name, owner, status in TEST_RESULT.findall(ANSI_ESCAPE.sub("", output)):
        owner = owner.removeprefix("__main__.")
        if owner.endswith("." + name):
            owner = owner[:-len(name) - 1]
        key = f"{owner}.{name}"
        if key in results:
            raise ValueError(f"duplicate unittest result: {key}")
        results[key] = "ok" if status == "ok" else status.split()[0]
    return results


def _copy_sources(source, destination):
    source = Path(source).absolute()
    for path in (*reversed(source.parents), source):
        if not stat.S_ISDIR(path.lstat().st_mode):
            raise ValueError(f"source directory must not traverse a symlink: {path}")
    destination.mkdir(parents=True, exist_ok=True)
    copied = 0
    for parent, directories, files in os.walk(source, followlinks=False):
        for name in directories + files:
            path = Path(parent) / name
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                raise ValueError(f"candidate source contains symlink: {path.relative_to(source)}")
            if not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
                raise ValueError(f"candidate source contains a nonregular entry: {path.relative_to(source)}")
        directories[:] = sorted(name for name in directories if name not in EXCLUDED)
        for name in sorted(files):
            if not name.endswith(".py"):
                continue
            path = Path(parent) / name
            target = destination / path.relative_to(source)
            target.parent.mkdir(parents=True, exist_ok=True)
            # A fresh file avoids inheriting hardlinks, flags, or permissions.
            target.write_bytes(path.read_bytes())
            copied += 1
    if not copied:
        raise ValueError("candidate workspace contains no Python source files")


def _interpreter():
    # The macOS system Python includes SQLite and lives in the existing trusted
    # OS/CommandLineTools grants; Homebrew's dependency tree need not be exposed.
    if platform.system() == "Darwin":
        executable = Path("/usr/bin/python3")
        if not executable.is_file():
            raise RuntimeError("system Python is required for isolated fixture grading")
        return str(executable)
    return str(Path(sys.executable).resolve())


def _visible_arguments(command):
    arguments = shlex.split(command)
    if (len(arguments) < 4 or arguments[0] not in {"python", "python3"}
            or arguments[1:4] != ["-m", "unittest", "discover"]):
        raise ValueError("visible checks must use Python unittest discovery, without a shell")
    # Fixtures are authored inputs, but prohibit path traversal into their gold.
    if any(Path(arg).is_absolute() or ".." in Path(arg).parts for arg in arguments[4:]):
        raise ValueError("visible check arguments must stay within the fixture workspace")
    return arguments[1:]


def _captured(path):
    raw = path.read_bytes()
    return raw.decode("utf-8", errors="replace"), base64.b64encode(raw).decode("ascii")


def _execute(workspace, *, hidden_test=None, visible_command=None, timeout_seconds=60):
    started = time.monotonic()
    result = {"status": "infrastructure_failure", "passed": False, "returncode": None,
              "stdout": "", "stderr": "", "stdout_base64": "", "stderr_base64": "",
              "tests_run": None, "timed_out": False, "command": None}
    try:
        if type(timeout_seconds) is not int or timeout_seconds <= 0:
            raise ValueError("timeout_seconds must be a positive integer")
        with tempfile.TemporaryDirectory(prefix="moosedev-grade-") as temporary:
            root = Path(temporary).resolve()
            candidate, runtime = root / "workspace", root / "runtime"
            _copy_sources(workspace, candidate)
            runtime.mkdir()
            for name in ("home", "tmp"):
                (runtime / name).mkdir()
            command = [_interpreter(), "-I", "-S", "-B"]
            if hidden_test is not None:
                hidden_test = Path(hidden_test)
                if not stat.S_ISREG(hidden_test.lstat().st_mode):
                    raise ValueError("hidden test must be a regular file")
                script = runtime / "hidden_test.py"
                script.write_bytes(hidden_test.read_bytes())
                command.append(str(script))
            else:
                # -I deliberately removes cwd from sys.path. Restore only this
                # isolated candidate directory, as a normal unittest launch does.
                command.extend(["-c", "import os,runpy,sys; sys.path.insert(0, os.getcwd()); runpy.run_module('unittest', run_name='__main__')",
                                *_visible_arguments(visible_command)[2:]])
            command = sandbox_command(command, workspace=candidate, runtime=runtime,
                                      readable_paths=[], network_endpoints=[])
            result["command"] = command
            environment = {"PATH": "/usr/bin:/bin", "HOME": str(runtime / "home"),
                           "TMPDIR": str(runtime / "tmp"), "LANG": "en_US.UTF-8",
                           "LC_ALL": "en_US.UTF-8"}
            stdout, stderr = runtime / "stdout", runtime / "stderr"
            # Regular files avoid inherited pipes holding output draining open
            # after the process leader exits. The sandbox confines all children.
            with stdout.open("wb") as out, stderr.open("wb") as err:
                process = subprocess.Popen(command, cwd=candidate, env=environment,
                                           stdout=out, stderr=err, start_new_session=True,
                                           close_fds=True)
                try:
                    result["returncode"] = process.wait(timeout=timeout_seconds)
                except subprocess.TimeoutExpired:
                    result["timed_out"] = True
                finally:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    result["returncode"] = process.wait()
            result["stdout"], result["stdout_base64"] = _captured(stdout)
            result["stderr"], result["stderr_base64"] = _captured(stderr)
            summaries = TEST_COUNT.findall(result["stderr"])
            result["tests_run"] = int(summaries[-1]) if summaries else None
            if result["timed_out"]:
                result.update(status="agent_failure", error="candidate check exceeded its time budget")
            elif result["returncode"] != 0:
                result["status"] = "agent_failure"
                if "sandbox-exec:" in result["stderr"] or "dyld:" in result["stderr"]:
                    result.update(status="infrastructure_failure", error="grading process failed to initialize")
            elif not result["tests_run"]:
                result["error"] = "check exited successfully without running any unittest tests"
            else:
                result.update(status="success", passed=True)
    except (OSError, ValueError, RuntimeError) as error:
        result.update(status="infrastructure_failure", passed=False, error=str(error))
    result["elapsed_seconds"] = time.monotonic() - started
    return result


def execute_check(workspace: Path, hidden_test: Path, *, timeout_seconds: int = 60) -> dict:
    """Grade a source copy; original workspaces and gold directories are never granted."""
    return _execute(workspace, hidden_test=hidden_test, timeout_seconds=timeout_seconds)


def _observed(result):
    try:
        return test_results(result["stderr"])
    except ValueError as error:
        result["error"] = str(error)
        return None


def _long_cases(name, scenario, package, record):
    """Every probe is observed by name; negatives must fail exactly the probes they declare."""
    from . import long_horizon
    from .indexing import resolve_offline

    def offline(case_id, root):
        targets = long_horizon.RESOLUTION_TARGETS.get(name)
        for target in targets or [None]:
            try:
                if target is None:
                    raise RuntimeError("no reviewed resolution target")
                result = {"status": "success", "resolved": resolve_offline(root, target)}
            except (OSError, SyntaxError, ValueError, RuntimeError) as error:
                result = {"status": "infrastructure_failure", "error": str(error)}
            record(name, f"{case_id}/{target and target['name']}", "offline_target", "pass",
                   result["status"] == "success", result)

    episodes = scenario["episodes"]
    by_id = {episode["id"]: episode for episode in episodes}
    project = relative_file(package, "project")
    offline("project", project)
    for index, episode in enumerate(episodes):
        reference = relative_file(package, episode["reference"])
        declared = {probe["test"] for probe in episode["probes"]}
        offline(f"{episode['id']}/reference", reference)
        for number, command in enumerate(episode["visible_checks"]):
            result = _execute(reference, visible_command=command)
            record(name, f"{episode['id']}/visible/{number}", "reference_visible", "pass",
                   result["status"] == "success", result)
        hidden = relative_file(package, episode["hidden_test"])
        result = execute_check(reference, hidden)
        observed = _observed(result)
        record(name, f"{episode['id']}/hidden", "reference_probes", "pass",
               result["status"] == "success" and observed is not None and set(observed) == declared
               and all(value == "ok" for value in observed.values()), result, observed)
        if index == 0:
            result = execute_check(project, hidden)
            observed = _observed(result)
            record(name, f"{episode['id']}/project", "project_hidden", "fail",
                   result["status"] == "agent_failure" and not result["timed_out"] and observed is not None
                   and any(observed.get(test) != "ok" for test in declared), result, observed)
        else:
            previous = episodes[index - 1]
            earlier = {probe["test"] for probe in previous["probes"]}
            kept = earlier - set(episode["retired_tests"])
            result = execute_check(reference, relative_file(package, previous["hidden_test"]))
            observed = _observed(result)
            record(name, f"{episode['id']}/chain/{previous['id']}", "chain", "pass",
                   observed is not None and set(observed) == earlier
                   and all(observed[test] == "ok" for test in kept), result, observed)
    for negative in scenario["negative_checks"]:
        episode = by_id[negative["episode"]]
        probes = {probe["id"]: probe["test"] for probe in episode["probes"]}
        expected_failures = {probes[probe] for probe in negative["fails_probes"]}
        with tempfile.TemporaryDirectory(prefix="moosedev-negative-") as temporary:
            workspace = Path(temporary).resolve() / "workspace"
            _copy_sources(relative_file(package, negative["base_reference"]), workspace)
            _copy_sources(relative_file(package, negative["overlay"]), workspace)
            for number, command in enumerate(episode["visible_checks"]):
                result = _execute(workspace, visible_command=command)
                record(name, f"{negative['id']}/visible/{number}", "negative_visible", "pass",
                       result["status"] == "success", result)
            result = execute_check(workspace, relative_file(package, episode["hidden_test"]))
            observed = _observed(result)
            failing = None if observed is None else {test for test, value in observed.items() if value != "ok"}
            record(name, negative["id"], "negative_probes", "fail",
                   result["status"] == "agent_failure" and not result["timed_out"] and observed is not None
                   and set(observed) == set(probes.values()) and failing == expected_failures,
                   result, observed)


def validate_fixtures(scenarios=None) -> dict:
    """Validate each reference and negative overlay; retain every check's outputs."""
    cases = []

    def record(scenario_id, case_id, kind, expected, passed, result, probes=None):
        case = {"scenario_id": scenario_id, "id": case_id, "kind": kind,
                "expected": expected, "passed": passed, "result": result}
        if probes is not None:
            case["probes"] = probes
        cases.append(case)

    def add(scenario_id, case_id, kind, expected, result):
        passed = result["status"] == "success" if expected == "pass" else (
            result["status"] == "agent_failure" and (result["tests_run"] or 0) > 0
            and not result["timed_out"])
        record(scenario_id, case_id, kind, expected, passed, result)

    from .scenario import MAINTENANCE
    for name in scenarios if scenarios is not None else [name for name in list_scenarios() if name != MAINTENANCE]:
        scenario = load_scenario(name)
        package = SCENARIOS / name
        if scenario["schema_version"] == 2:
            _long_cases(name, scenario, package, record)
            continue
        episodes = {episode["id"]: episode for episode in scenario["episodes"]}
        for episode in episodes.values():
            reference = relative_file(package, episode["reference"])
            for index, command in enumerate(episode["visible_checks"]):
                add(name, f"{episode['id']}/visible/{index}", "reference_visible", "pass",
                    _execute(reference, visible_command=command))
            add(name, f"{episode['id']}/hidden", "reference_hidden", "pass",
                execute_check(reference, relative_file(package, episode["hidden_test"])))
        for negative in scenario.get("negative_checks", []):
            if negative.get("expected") != "fail":
                raise ValueError("negative fixture must expect failure")
            episode = episodes[negative["episode"]]
            with tempfile.TemporaryDirectory(prefix="moosedev-negative-") as temporary:
                workspace = Path(temporary).resolve() / "workspace"
                _copy_sources(relative_file(package, negative["base_reference"]), workspace)
                _copy_sources(relative_file(package, negative["overlay"]), workspace)
                add(name, negative["id"], "negative_hidden", "fail",
                    execute_check(workspace, relative_file(package, episode["hidden_test"])))
    return {"schema_version": 1, "passed": bool(cases) and all(case["passed"] for case in cases),
            "case_count": len(cases), "cases": cases}
