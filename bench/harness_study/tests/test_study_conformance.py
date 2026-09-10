"""Opt-in local conformance probes; these perform no model generation.

Run explicitly with MOOSEDEV_RUN_STUDY_CONFORMANCE=1 on macOS after building the
repository release binaries. Evidence remains under target/harness-study on both
success and failure. This checks transport/startup, not coding-task performance.
"""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tempfile
import threading
import time
import unittest
import uuid

from bench.harness_study.adapters import build_command
from bench.harness_study.artifacts import canonical_json, sha256_file
from bench.harness_study.binaries import REPO
from bench.harness_study.config import freeze_assets, template
from bench.harness_study.daemon import OwnedDaemon
from bench.harness_study.isolation import sandbox_command
from bench.harness_study.run import runtime_assets
from bench.harness_study.clients import freeze_client
from bench.harness_study.scenario import SCENARIOS, load_scenario
from bench.harness_study.seed import prepare_workspace


MODEL = "study-conformance-no-generation"


@contextmanager
def rejecting_helper(evidence):
    """Unexpected sensor/model requests fail and are retained, never answered."""
    posts = []
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            body = canonical_json({"data": [{"id": MODEL}]})
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            request = {"path": self.path, "body": body.decode(errors="replace")}
            with lock:
                posts.append(request)
                with (evidence / "unexpected-helper-requests.jsonl").open("ab") as stream:
                    stream.write(canonical_json(request))
            response = canonical_json({"error": "model generation forbidden in conformance probe"})
            self.send_response(503)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1", posts
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        (evidence / "helper-summary.json").write_bytes(canonical_json({"post_requests": len(posts)}))


@contextmanager
def recording_completion_stub(evidence):
    """Retain native requests and return fixed protocol responses, without a model."""
    posts = []
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            request = {"path": self.path, "body": json.loads(body)}
            with lock:
                posts.append(request)
                with (evidence / "completion-requests.jsonl").open("ab") as stream:
                    stream.write(canonical_json(request))
            chunks = [{"id": "stub-completion", "object": "chat.completion.chunk", "created": 0,
                       "model": MODEL, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                      for delta, finish in (({"role": "assistant", "content": "Conformance complete"}, None),
                                            ({}, "stop"))]
            response = b"".join(b"data: " + canonical_json(chunk) + b"\n" for chunk in chunks) + b"data: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1", posts
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


@unittest.skipUnless(platform.system() == "Darwin"
                     and os.environ.get("MOOSEDEV_RUN_STUDY_CONFORMANCE") == "1",
                     "explicit macOS repository-release conformance probe")
class ConformanceTests(unittest.TestCase):
    def setUp(self):
        self.evidence = REPO / "target/harness-study" / f"conformance-{uuid.uuid4()}"
        self.evidence.mkdir(parents=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="md-probe-", dir="/private/tmp")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace, self.runtime = self.root / "w", self.root / "r"
        self.workspace.mkdir()
        self.runtime.mkdir()
        self.addCleanup(self.archive_evidence)
        self.write_json("probe.json", {
            "test": self.id(), "platform": platform.platform(),
            "workspace": str(self.workspace), "runtime": str(self.runtime),
            "purpose": "startup/transport conformance only; no model generation",
        })

    def write_json(self, name, value):
        (self.evidence / name).write_bytes(canonical_json(value))

    def archive_evidence(self):
        # Archive canonical knowledge and native journals, not RocksDB-derived
        # stores, source repositories, or external reference/test packages.
        for base, label in ((self.workspace, "workspace"), (self.runtime, "runtime")):
            for path in sorted(base.rglob("*")):
                relative = path.relative_to(base)
                if path.is_symlink() or not path.is_file():
                    continue
                if ".moosedev" in relative.parts and not (
                        path.name == "kg.nq" or "harness" in relative.parts):
                    continue
                if path.suffix == ".lock":
                    continue
                destination = self.evidence / label / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, destination)
        files = {str(path.relative_to(self.evidence)): sha256_file(path)
                 for path in sorted(self.evidence.rglob("*")) if path.is_file()}
        self.write_json("evidence-checksums.json", files)

    def run_capture(self, command, environment, label, *, input_bytes=None, timeout=15):
        self.write_json(label + "-launch.json", {"command": command, "environment": environment})
        if len(command) >= 3 and command[1] == "-p":
            (self.evidence / (label + ".sb")).write_text(command[2])
        started = time.monotonic()
        stdout, stderr = b"", b""
        result = None
        error = None
        try:
            result = subprocess.run(command, cwd=self.workspace, env=environment,
                                    input=input_bytes, capture_output=True,
                                    close_fds=True, start_new_session=True, timeout=timeout)
            stdout, stderr = result.stdout, result.stderr
            return result
        except subprocess.TimeoutExpired as failure:
            stdout, stderr = failure.stdout or b"", failure.stderr or b""
            error = str(failure)
            raise
        except BaseException as failure:
            error = str(failure)
            raise
        finally:
            (self.evidence / (label + ".stdout")).write_bytes(stdout)
            (self.evidence / (label + ".stderr")).write_bytes(stderr)
            self.write_json(label + "-result.json", {
                "returncode": None if result is None else result.returncode,
                "elapsed_seconds": time.monotonic() - started, "error": error,
            })

    def test_owned_daemon_and_session_adapter_without_generation(self):
        binaries = {
            "daemon": REPO / "target/release/moosedev",
            "harness": REPO / "target/release/moosedev-harness",
            "session": REPO / "target/release/examples/harness_study_session",
        }
        identity = {}
        for role, binary in binaries.items():
            self.assertTrue(binary.is_file() and not binary.is_symlink(), f"build repository release {role}")
            identity[role] = {"path": str(binary), "sha256": sha256_file(binary)}
        self.write_json("binaries.json", identity)
        assets = freeze_assets({"config": template()})
        self.write_json("assets.json", assets)
        scenario = load_scenario("ruleset_cache")
        self.write_json("scenario.json", {"id": scenario["id"], "package_sha256": scenario["package_sha256"]})
        shutil.copytree(SCENARIOS / scenario["id"] / "project", self.workspace, dirs_exist_ok=True)
        prepare_workspace(self.workspace, scenario, "harness")
        shutil.copyfile(self.workspace / ".moosedev/kg.nq", self.evidence / "initial-kg.nq")
        with rejecting_helper(self.evidence) as (endpoint, posts):
            daemon = OwnedDaemon(
                executable=binaries["daemon"], expected_sha256=identity["daemon"]["sha256"],
                workspace=self.workspace, runtime=self.runtime, assets=Path(assets["directory"]),
                helper_model=MODEL, helper_endpoint=endpoint,
                log_path=self.evidence / "daemon.log", timeout_seconds=90,
            )
            try:
                with daemon:
                    self.write_json("daemon-identity.json", daemon.identity)
                    context = daemon._request("/api/v1/harness/context", {"topic": "ruleset cache identity and reuse", "files": []})
                    self.write_json("seed-context.json", context)
                    for fact in scenario["initial_facts"]:
                        self.assertIn(fact["title"], context["context"])
                    checkpoint = daemon.checkpoint()
                    self.write_json("checkpoint-before.json", checkpoint)
                    self.assertTrue(checkpoint["durable"])
                    self.assertTrue(checkpoint["conforms"])
                    self.assertEqual(checkpoint["pending"], [])
                    command, additions = build_command(
                        "harness", executable=binaries["session"], model=MODEL,
                        workspace=self.workspace, runtime=self.runtime, prompt="", endpoint=endpoint,
                        daemon_url=daemon.url, daemon_exe=binaries["daemon"], daemon_socket=daemon.socket,
                    )
                    # Native harness commands own their sandbox. macOS refuses
                    # nesting another sandbox around the controller process.
                    environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", **additions}
                    result = self.run_capture(command, environment, "session",
                                              input_bytes=canonical_json({"type": "quit"}))
                    self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
                    events = [json.loads(line) for line in result.stdout.splitlines()]
                    self.assertTrue(any(event["type"] == "state" for event in events))
                    self.assertEqual(events[-1]["type"], "closed")
                    for event in events:
                        if event["type"] == "state":
                            self.assertEqual(event["model"], MODEL)
                            self.assertIsNone(event["task"])
                    self.write_json("checkpoint-after.json", daemon.checkpoint())
                    self.assertEqual(posts, [], "probe must never ask a model to generate")
            finally:
                self.write_json("daemon-launch.json", {"command": daemon.command, "environment": daemon.env})
                if len(daemon.command) >= 3:
                    (self.evidence / "daemon.sb").write_text(daemon.command[2])
                self.write_json("daemon-final-identity.json", daemon.identity)
        for role, binary in binaries.items():
            self.assertEqual(sha256_file(binary), identity[role]["sha256"], "release binary changed during probe")

    def test_opencode_pure_version_in_confined_runtime(self):
        configured = template()["opencode"]
        if configured is None:
            self.skipTest("OpenCode is not installed")
        identity = freeze_client(Path(configured))
        executable = Path(identity["path"])
        self.write_json("opencode-binary.json", identity)
        _, additions = build_command(
            "opencode", executable=executable, model=MODEL, workspace=self.workspace,
            runtime=self.runtime, prompt="", endpoint="http://127.0.0.1:1/v1",
        )
        command = sandbox_command(
            [str(executable), "run", "--pure", "--version"],
            workspace=self.workspace, runtime=self.runtime,
            readable_paths=runtime_assets(executable), network_endpoints=[],
        )
        result = self.run_capture(command, {"PATH": identity["runtime_root"] + "/bin:/usr/bin:/bin:/usr/sbin:/sbin", **additions}, "opencode")
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        self.assertTrue(result.stdout.strip(), "version probe must identify the selected client")

    def test_opencode_native_requests_preserve_zero_temperature(self):
        configured = template()["opencode"]
        if configured is None:
            self.skipTest("OpenCode is not installed")
        identity = freeze_client(Path(configured))
        executable = Path(identity["path"])
        self.write_json("opencode-binary.json", identity)
        with recording_completion_stub(self.evidence) as (endpoint, posts):
            command, additions = build_command(
                "opencode", executable=executable, model=MODEL, workspace=self.workspace,
                runtime=self.runtime, prompt="Reply with a short greeting.", endpoint=endpoint,
            )
            command = sandbox_command(command, workspace=self.workspace, runtime=self.runtime,
                readable_paths=runtime_assets(executable), network_endpoints=[endpoint])
            result = self.run_capture(command,
                {"PATH": identity["runtime_root"] + "/bin:/usr/bin:/bin:/usr/sbin:/sbin", **additions},
                "opencode-completion", input_bytes=b"", timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        self.assertGreaterEqual(len(posts), 2, "probe must exercise both coding and title generation")
        self.assertTrue(any(post["body"].get("tools") for post in posts), "coding request missing")
        self.assertTrue(any("title" in json.dumps(post["body"].get("messages", [])).lower()
                            and not post["body"].get("tools") for post in posts), "title request missing")
        for post in posts:
            self.assertEqual(post["path"], "/v1/chat/completions")
            self.assertEqual(post["body"]["model"], MODEL)
            self.assertEqual(post["body"].get("temperature"), 0.0, post["body"])
        events = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertTrue(any(event.get("type") == "step_finish" for event in events), result.stdout)

    def test_codex_explicit_ca_enables_verified_tls_in_confined_runtime(self):
        configured = template()["codex"]
        if configured is None:
            self.skipTest("Codex is not installed")
        identity = freeze_client(Path(configured))
        executable = Path(identity["path"])
        self.write_json("codex-binary.json", identity)
        # The generated private key remains outside every client-readable path.
        cert, key = self.root / "server.crt", self.root / "server.key"
        ca, ca_key, csr = self.root / "ca.crt", self.root / "ca.key", self.root / "server.csr"
        openssl_config = self.root / "openssl.cnf"
        openssl_config.write_text("[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n"
                                  "[dn]\nCN=Study TLS root\n[ca]\nbasicConstraints=critical,CA:TRUE\n"
                                  "keyUsage=critical,keyCertSign,cRLSign\n"
                                  "[server]\nsubjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n"
                                  "keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
        commands = [
            ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-keyout", str(ca_key),
             "-out", str(ca), "-config", str(openssl_config)],
            ["req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key), "-out", str(csr),
             "-subj", "/CN=localhost"],
            ["x509", "-req", "-in", str(csr), "-CA", str(ca), "-CAkey", str(ca_key), "-CAcreateserial",
             "-out", str(cert), "-days", "1", "-extfile", str(openssl_config), "-extensions", "server"],
        ]
        for command in commands:
            generated = subprocess.run(["/usr/bin/openssl", *command], capture_output=True, timeout=15)
            self.assertEqual(generated.returncode, 0, generated.stderr)
        self.write_json("tls-certificate.json", {"sha256": sha256_file(cert), "purpose": "local synthetic TLS probe"})
        posts = []
        tls_failures = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                posts.append({"path": self.path, "body": body.decode(errors="replace")})
                response = canonical_json({"error": {"message": "study-tls-reached", "type": "invalid_request_error"}})
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)

        class Server(ThreadingHTTPServer):
            def get_request(self):
                connection, address = super().get_request()
                connection.settimeout(5)
                try:
                    return context.wrap_socket(connection, server_side=True), address
                except ssl.SSLError as error:
                    tls_failures.append(str(error))
                    connection.close()
                    raise

        server = Server(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        endpoint = f"https://127.0.0.1:{server.server_port}/v1"
        try:
            for trusted in (False, True):
                label = "codex-tls-trusted" if trusted else "codex-tls-untrusted"
                runtime = self.runtime / label
                command, additions = build_command("codex", executable=executable, model=MODEL,
                    workspace=self.workspace, runtime=runtime, prompt="Reply hello.",
                    ca_bundle=ca if trusted else None)
                provider = {"name": "Conformance stub", "base_url": endpoint, "env_key": "STUDY_PROBE_KEY",
                            "wire_api": "responses", "request_max_retries": 0, "stream_max_retries": 0}
                from bench.harness_study.adapters import _toml
                command[-2:-2] = ["-c", 'model_provider="study"', "-c", "model_providers.study=" + _toml(provider)]
                command = sandbox_command(command, workspace=self.workspace, runtime=runtime,
                    readable_paths=runtime_assets(executable), network_endpoints=[endpoint])
                environment = {"PATH": identity["runtime_root"] + "/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                               "STUDY_PROBE_KEY": "synthetic-conformance-credential", **additions}
                try:
                    result = self.run_capture(command, environment, label, input_bytes=b"", timeout=15)
                except subprocess.TimeoutExpired:
                    if trusted:
                        raise
                else:
                    self.assertNotEqual(result.returncode, 0, "stub intentionally returns HTTP400")
                    if trusted:
                        self.assertIn(b"study-tls-reached", result.stdout + result.stderr)
                if trusted:
                    self.assertTrue(posts, "explicit trusted CA must reach the TLS stub's HTTP handler")
                else:
                    self.assertEqual(posts, [], "untrusted TLS must never reach HTTP")
                    output = (self.evidence / (label + ".stdout")).read_bytes()
                    self.assertIn(b"Connection failed", output, "negative control must report failed transport")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
            self.write_json("tls-observations.json", {"requests": posts, "tls_failures": tls_failures})


if __name__ == "__main__":
    unittest.main()
