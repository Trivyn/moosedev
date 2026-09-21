"""Own and identify a confined study daemon; never discover or reuse one.

Health is insufficient identity: the owned PID must execute the frozen image and
hold both listeners. Reserving an HTTP port has a release/bind race; ownership
checks make a losing race a startup failure, never attachment to another server.
Call checkpoint before leaving the context when a durable graph is needed.
"""

import json
import os
from pathlib import Path
import signal
import socket
import stat
import subprocess
import time
from urllib.error import URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from .artifacts import sha256_file
from .binaries import REPO
from .isolation import _checked_path, sandbox_command
from .seed import GRAPH


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        raise ValueError("study daemon redirected its identity endpoint")


class _NotReady(Exception):
    pass


def _read_text(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor) as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError(f"daemon publication is not a regular file: {path}")
        return stream.read(4096).strip()


class OwnedDaemon:
    def __init__(self, *, executable: Path, expected_sha256: str, workspace: Path,
                 runtime: Path, assets: Path, helper_model: str, helper_endpoint: str,
                 helper_context_tokens: int = 32768, log_path: Path,
                 timeout_seconds: int = 90, indexer=None):
        self.executable = _checked_path(executable)
        if not self.executable.is_file() or not os.access(self.executable, os.X_OK):
            raise ValueError("study daemon must be an executable regular file")
        if not self.executable.is_relative_to(REPO / "target"):
            raise ValueError("study daemon must originate from this repository's target tree")
        self.expected_sha256 = expected_sha256
        self.workspace = _checked_path(workspace, directory=True)
        self.runtime = _checked_path(runtime, directory=True)
        self.assets = _checked_path(assets, directory=True)
        self.data = _checked_path(self.workspace / ".moosedev", directory=True)
        for child in ("ontologies", "models", "skills"):
            _checked_path(self.assets / child, directory=True)
        if not helper_model.strip() or helper_context_tokens < 4096 or timeout_seconds <= 0:
            raise ValueError("explicit helper model, valid context, and positive timeout are required")
        endpoint = urlsplit(helper_endpoint)
        if (endpoint.scheme != "http" or endpoint.hostname not in ("localhost", "127.0.0.1", "::1")
                or not endpoint.port or endpoint.username or endpoint.password
                or endpoint.query or endpoint.fragment):
            raise ValueError("daemon helper must use an explicit unauthenticated loopback HTTP endpoint")
        self.helper_model = helper_model
        self.helper_endpoint = helper_endpoint
        self.helper_context_tokens = helper_context_tokens
        self.log_path = Path(log_path)
        _checked_path(self.log_path.parent, directory=True)
        if self.log_path.is_relative_to(self.workspace) or self.log_path.is_relative_to(self.runtime):
            raise ValueError("daemon evidence log must be outside agent-writable storage")
        self.timeout_seconds = timeout_seconds
        self.socket = self.runtime / "daemon.sock"
        if len(os.fsencode(self.socket)) >= 100:
            raise ValueError("study runtime is too long for the macOS daemon socket")
        self.url = None
        self.env = {}
        self.identity = {}
        self.command = []
        self.process = None
        self._log = None
        self._stopped = False
        self._entered = False
        self._http = build_opener(ProxyHandler({}), _NoRedirect())
        self.indexer = indexer
        if indexer is not None:
            from .indexing import verify_indexer
            verify_indexer(indexer)

    def _check_binary(self):
        if sha256_file(self.executable) != self.expected_sha256:
            raise ValueError("frozen daemon executable hash mismatch")

    def _controlled_environment(self, address):
        home, temporary = self.runtime / "home", self.runtime / "tmp"
        for path in (home, temporary):
            path.mkdir(mode=0o700, exist_ok=True)
            _checked_path(path, directory=True)
        environment = {
            "HOME": str(home), "TMPDIR": str(temporary), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8",
            "XDG_CONFIG_HOME": str(home / ".config"), "XDG_CACHE_HOME": str(home / ".cache"),
            "MOOSEDEV_DATA_DIR": str(self.data), "MOOSEDEV_SOCKET": str(self.socket),
            "MOOSEDEV_HTTP_ADDR": address, "MOOSEDEV_NO_HTTP": "0",
            "MOOSEDEV_NO_AUTOSPAWN": "1", "MOOSEDEV_NO_LSP": "1",
            "MOOSEDEV_ONTOLOGY_DIR": str(self.assets / "ontologies"),
            "MOOSE_ONTOLOGY_DIR": str(self.assets / "ontologies"),
            "MOOSEDEV_SKILLS_DIR": str(self.assets / "skills"),
            "MOOSEDEV_MODEL_DIR": str(self.assets), "MOOSE_MODEL_DIR": str(self.assets),
            "MOOSEDEV_LLM_BASE_URL": self.helper_endpoint,
            "MOOSEDEV_LLM_MODEL": self.helper_model,
            "MOOSEDEV_LLM_API_KEY": "study-local",
            "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS": str(self.helper_context_tokens),
            "MOOSEDEV_LLM_STRUCTURED_OUTPUT": "auto",
            "RUST_LOG": "moosedev=info,moose=warn,rmcp=warn",
        }
        if self.indexer is not None:
            environment["MOOSEDEV_SCIP_PYTHON"] = self.indexer["launcher"]["path"]
        return environment

    def __enter__(self):
        if self._entered:
            raise RuntimeError("an OwnedDaemon instance cannot be restarted or reused")
        self._entered = True
        self._check_binary()
        for path in (self.socket, self.data / "http.addr", self.data / "moosedev-serve.pid"):
            if os.path.lexists(path):
                raise ValueError(f"refusing preexisting daemon rendezvous state: {path}")
        if os.path.lexists(self.workspace / ".env"):
            raise ValueError("study daemon refuses project dotenv overrides; use its controlled environment")
        # The study session adapter never reads the harness's local model
        # configuration; its presence still means the workspace is not the frozen one.
        if os.path.lexists(self.workspace / "moosedev.toml"):
            raise ValueError("study daemon refuses a project moosedev.toml; one frozen model answers every role")
        reservation = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
            address = f"127.0.0.1:{port}"
            self.url = f"http://{address}"
            self.env = self._controlled_environment(address)
            self.command = sandbox_command(
                [str(self.executable), "--serve", str(self.socket)],
                workspace=self.workspace, runtime=self.runtime,
                readable_paths=[self.executable, self.assets]
                + ([Path(self.indexer["directory"])] if self.indexer is not None else []),
                network_endpoints=[self.helper_endpoint],
                listening_endpoints=[self.url, f"unix://{self.socket}"],
            )
            descriptor = os.open(self.log_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            self._log = os.fdopen(descriptor, "wb", buffering=0)
            reservation.close()
            self.process = subprocess.Popen(
                self.command, cwd=self.workspace, env=self.env, stdin=subprocess.DEVNULL,
                stdout=self._log, stderr=subprocess.STDOUT, close_fds=True, start_new_session=True,
            )
            self._wait_ready(address, port)
            return self
        except BaseException:
            self.stop()
            raise
        finally:
            reservation.close()

    def _alive(self):
        if self.process is None or self.process.poll() is not None:
            raise RuntimeError("owned study daemon exited; inspect its retained log")

    def _process_identity(self, port):
        self._alive()
        command = subprocess.check_output(
            ["/bin/ps", "-p", str(self.process.pid), "-o", "comm="],
            text=True, timeout=5, env={"PATH": "/usr/bin:/bin"},
        ).strip()
        if command == "/usr/bin/sandbox-exec":
            raise _NotReady("sandbox wrapper has not executed the daemon")
        if command != str(self.executable):
            raise ValueError(f"owned PID executes an unexpected image: {command}")
        self._check_binary()
        tcp = subprocess.run(
            ["/usr/sbin/lsof", "-nP", "-a", "-p", str(self.process.pid),
             f"-iTCP@127.0.0.1:{port}", "-sTCP:LISTEN", "-Fn"],
            capture_output=True, text=True, timeout=5, env={"PATH": "/usr/bin:/bin"},
        )
        unix = subprocess.run(
            ["/usr/sbin/lsof", "-nP", "-a", "-p", str(self.process.pid), "-U", "-Fn"],
            capture_output=True, text=True, timeout=5, env={"PATH": "/usr/bin:/bin"},
        )
        if tcp.returncode or f"n127.0.0.1:{port}" not in tcp.stdout.splitlines():
            raise _NotReady("owned PID does not hold the expected HTTP listener")
        if unix.returncode or f"n{self.socket}" not in unix.stdout.splitlines():
            raise _NotReady("owned PID does not hold the expected Unix listener")
        self._alive()
        return {"pid": self.process.pid, "executable": command,
                "sha256": self.expected_sha256, "tcp_listener": tcp.stdout,
                "unix_listener": unix.stdout, "url": self.url, "socket": str(self.socket)}

    def _request(self, path, body=None):
        request = Request(self.url + path,
                          data=None if body is None else json.dumps(body).encode(),
                          headers={"Content-Type": "application/json"})
        with self._http.open(request, timeout=180 if path.startswith("/api/v1/harness/intent/") else 5) as response:
            if response.status != 200:
                raise ValueError("daemon returned a non-success status")
            payload = response.read(4 * 1024 * 1024 + 1)
            if len(payload) > 4 * 1024 * 1024:
                raise ValueError("daemon identity response exceeds expected bounds")
            value = json.loads(payload)
            if not isinstance(value, dict):
                raise ValueError("daemon response is not an object")
            return value

    def _wait_ready(self, address, port):
        deadline = time.monotonic() + self.timeout_seconds
        pending = "daemon has not published its listeners"
        while time.monotonic() < deadline:
            self._alive()
            try:
                published = _read_text(self.data / "http.addr")
                if published != address:
                    raise ValueError("owned daemon published an unexpected HTTP address")
                if _read_text(self.data / "moosedev-serve.pid") != str(self.process.pid):
                    raise ValueError("daemon pidfile does not identify the owned child")
                if not stat.S_ISSOCK(self.socket.lstat().st_mode):
                    raise ValueError("daemon socket publication is not a real socket")
                identity = self._process_identity(port)
                health = self._request("/api/v1/health")
                if (health.get("status") != "ok" or health.get("project_graph") != GRAPH
                        or health.get("project_root") != str(self.workspace)
                        or health.get("data_dir") != str(self.data)
                        or health.get("llm_configured") is not True):
                    raise ValueError("owned daemon health does not match project/data/helper configuration")
                context = self._request("/api/v1/harness/context", {"topic": "harness startup", "files": []})
                if (context.get("project_root") != str(self.workspace)
                        or not isinstance(context.get("revision"), str)
                        or not context["revision"] or not isinstance(context.get("context"), str)
                        or not isinstance(context.get("files"), list)):
                    raise ValueError("owned daemon lacks compatible harness context")
                log = self.log_path.read_text(errors="replace")
                if "alignment index unavailable" in log or "instance dense index unavailable" in log:
                    raise RuntimeError("study daemon degraded embedding retrieval; inspect retained log")
                self._alive()
                self.identity = {**identity, "health": health, "context": context,
                                 "helper_model": self.helper_model, "helper_endpoint": self.helper_endpoint}
                return
            except (FileNotFoundError, _NotReady, URLError, TimeoutError) as error:
                pending = str(error)
                time.sleep(0.1)
        raise TimeoutError(f"owned daemon startup timed out: {pending}; inspect {self.log_path}")

    def checkpoint(self):
        self._process_identity(urlsplit(self.url).port)
        checkpoint = self._request("/api/v1/harness/checkpoint", {})
        if (checkpoint.get("durable") is not True
                or not isinstance(checkpoint.get("conforms"), bool)
                or not isinstance(checkpoint.get("revision"), str) or not checkpoint["revision"]
                or not isinstance(checkpoint.get("pending"), list)
                or any(not isinstance(item, str) for item in checkpoint["pending"])):
            raise ValueError("daemon did not return a durable compatible checkpoint")
        self._alive()
        sha256_file(self.data / "kg.nq")  # Require a real persisted canonical graph.
        return checkpoint

    def stop(self):
        if self._stopped:
            return
        self._stopped = True
        try:
            if self.process is not None and self.process.poll() is None:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=5)
        finally:
            if self._log is not None:
                self._log.close()

    def __exit__(self, exc_type, exc_value, traceback):
        self.stop()
