"""Pure policy checks; real Seatbelt probes run explicitly on the study host."""

import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import platform
import socket
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

from bench.harness_study import isolation


class ProfileTests(unittest.TestCase):
    def test_profile_grants_only_exact_ports_and_storage(self):
        profile = isolation.sandbox_profile(
            readable_directories=["/private/tmp/work", "/private/tmp/runtime"],
            readable_files=["/tools/agent"],
            writable_directories=["/private/tmp/work", "/private/tmp/runtime"],
            network_targets=[("127.0.0.1", 1234), ("::1", 2345)],
        )
        self.assertIn('(remote tcp4 "localhost:1234")', profile)
        self.assertIn('(remote tcp6 "localhost:2345")', profile)
        self.assertNotIn('localhost:*', profile)
        self.assertNotIn('(allow network-inbound', profile)
        self.assertNotIn('(allow network-bind', profile)
        self.assertNotIn('(allow mach-lookup', profile)
        self.assertNotIn('(subpath "/")', profile)
        self.assertNotIn('(subpath "/Users")', profile)
        self.assertIn('(deny file-write-flags)', profile)
        self.assertIn('(allow file-read* process-exec (literal "/tools/agent"))', profile)

    def test_literal_escaping_prevents_policy_injection(self):
        self.assertEqual(isolation.sandbox_literal('a"\\b'), '"a\\"\\\\b"')
        for value in ["a\nb", "a\0b", "a\rb", "a\u202eb"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                isolation.sandbox_literal(value)

    def test_endpoint_validation_never_wildcards_ports_or_addresses(self):
        for endpoint in ["http://127.0.0.1", "http://localhost:*", "http://127.0.0.1:0",
                         "http://10.0.0.1:1234", "https://169.254.169.254:443",
                         "https://user:secret@example.com:443", "http://example.com:80",
                         "https://example.com:8443", "https://example.com:443?x=1"]:
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                isolation._network_targets([endpoint])
        targets, dns, unix = isolation._network_targets(["http://127.0.0.1:1234/v1"])
        self.assertEqual(targets, [("127.0.0.1", 1234)])
        self.assertFalse(dns)
        self.assertEqual(unix, [])

    def test_remote_endpoints_require_a_proxy_without_dns_or_wildcard_fallback(self):
        for endpoint in ["https://example.com:443", "https://1.1.1.1:443", "http://127.0.0.2:1234"]:
            with self.subTest(endpoint=endpoint), self.assertRaisesRegex(ValueError, "proxy"):
                isolation._network_targets([endpoint])
        with self.assertRaises(ValueError):
            isolation.sandbox_profile(readable_directories=[], readable_files=[],
                                      writable_directories=[], network_targets=[("1.1.1.1", 443)])

    def test_unsupported_platform_never_returns_original_command(self):
        with mock.patch.object(isolation.platform, "system", return_value="Linux"):
            with self.assertRaisesRegex(RuntimeError, "unsupported"):
                isolation.sandbox_command(["/bin/true"], workspace=Path("/w"), runtime=Path("/r"))

    def test_unix_endpoints_are_real_runtime_sockets_not_foreign_daemons(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            runtime = root / "runtime"
            runtime.mkdir()
            own = runtime / "daemon.sock"
            foreign = root / "other.sock"
            with socket.socket(socket.AF_UNIX) as own_socket, socket.socket(socket.AF_UNIX) as foreign_socket:
                own_socket.bind(str(own))
                foreign_socket.bind(str(foreign))
                self.assertEqual(isolation._network_targets([own.as_uri().replace("file:", "unix:")], runtime),
                                 ([], False, [str(own)]))
                for path in [foreign, runtime]:
                    with self.subTest(path=path), self.assertRaises(ValueError):
                        isolation._network_targets([path.as_uri().replace("file:", "unix:")], runtime)
                (runtime / "alias").symlink_to(foreign)
                with self.assertRaises(ValueError):
                    isolation._network_targets([f"unix://{runtime / 'alias'}"], runtime)

    def test_listening_is_explicit_loopback_or_runtime_socket_only(self):
        for endpoint in ["http://0.0.0.0:8080", "http://127.0.0.1:0",
                         "https://example.com:443", "https://1.1.1.1:443"]:
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                isolation._network_targets([endpoint], listening=True)
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary).resolve()
            socket_path = runtime / "not-yet-bound.sock"
            targets, dns, sockets = isolation._network_targets(
                ["http://127.0.0.1:8080", f"unix://{socket_path}"], runtime, listening=True)
            self.assertEqual(targets, [("127.0.0.1", 8080)])
            self.assertFalse(dns)
            self.assertEqual(sockets, [str(socket_path)])
            profile = isolation.sandbox_profile(
                readable_directories=[], readable_files=[], writable_directories=[],
                listening_targets=targets, listening_sockets=sockets)
            self.assertIn('(allow network-bind network-inbound (local tcp4 "localhost:8080"))', profile)
            self.assertNotIn('(allow network-outbound', profile)

    def test_symlinks_broad_paths_and_special_files_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            workspace, runtime = root / "work", root / "runtime"
            workspace.mkdir()
            runtime.mkdir()
            (root / "alias").symlink_to(workspace, target_is_directory=True)
            with self.assertRaises(ValueError):
                isolation._checked_path(root / "alias", directory=True)
            os.mkfifo(root / "fifo")
            with self.assertRaises(ValueError):
                isolation._checked_path(root / "fifo")
            with self.assertRaises(ValueError):
                isolation._readable_asset(root, workspace, runtime)
            for broad in [Path("/"), Path("/Users"), isolation.REPOSITORY]:
                with self.subTest(path=broad), self.assertRaises(ValueError):
                    isolation._reject_broad(broad)
            checkout = root / "checkout"
            (checkout / ".git").mkdir(parents=True)
            (checkout / "src").mkdir()
            (checkout / "target").mkdir()
            with self.assertRaises(ValueError):
                isolation._readable_asset(checkout / "src", workspace, runtime)
            self.assertEqual(isolation._readable_asset(checkout / "target", workspace, runtime),
                             checkout / "target")


@unittest.skipUnless(platform.system() == "Darwin" and os.environ.get("MOOSEDEV_RUN_STUDY_SANDBOX_TESTS") == "1",
                     "explicit macOS Seatbelt probe")
class RuntimeTests(unittest.TestCase):
    def test_macos_rejects_nested_seatbelt_so_study_must_use_one_layer(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            workspace, runtime = root / "work", root / "runtime"
            workspace.mkdir()
            runtime.mkdir()
            command = isolation.sandbox_command(
                ["/usr/bin/sandbox-exec", "-p", "(version 1)(allow default)", "/usr/bin/true"],
                workspace=workspace, runtime=runtime)
            result = subprocess.run(command, capture_output=True, timeout=5,
                                    env={"HOME": str(runtime), "TMPDIR": str(runtime), "PATH": "/usr/bin:/bin"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"Operation not permitted", result.stderr)

    def test_outbound_loopback_grant_allows_only_selected_port(self):
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", "7")
                self.end_headers()
                self.wfile.write(b"allowed")

        servers = [ThreadingHTTPServer(("127.0.0.1", 0), Handler) for _ in range(2)]
        for server in servers:
            threading.Thread(target=server.serve_forever, daemon=True).start()
            self.addCleanup(server.server_close)
            self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            workspace, runtime = root / "work", root / "runtime"
            workspace.mkdir()
            runtime.mkdir()
            endpoints = [f"http://127.0.0.1:{server.server_port}" for server in servers]
            for index, endpoint in enumerate(endpoints):
                command = ["/usr/bin/curl", "--noproxy", "*", "--silent", "--show-error",
                           "--connect-timeout", "1", "--max-time", "3", endpoint]
                wrapped = isolation.sandbox_command(command, workspace=workspace, runtime=runtime,
                                                     network_endpoints=[endpoints[0]])
                result = subprocess.run(wrapped, capture_output=True, timeout=5,
                                        env={"HOME": str(runtime), "TMPDIR": str(runtime), "PATH": "/usr/bin:/bin"})
                if index == 0:
                    self.assertEqual(result.returncode, 0, result.stderr.decode())
                    self.assertEqual(result.stdout, b"allowed")
                else:
                    self.assertNotEqual(result.returncode, 0, "ungranted localhost port was reachable")
                    self.assertNotIn(b"allowed", result.stdout)

    def test_agent_and_children_cannot_read_gold_or_write_outside_workspace(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            workspace, runtime = root / "work", root / "runtime"
            workspace.mkdir()
            runtime.mkdir()
            gold = root / "gold"
            gold.write_text("secret reference")
            # The symlink is inside a grant, but its destination is not.
            (workspace / "gold-link").symlink_to(gold)
            command = ["/bin/sh", "-c",
                       'cat "$1" && exit 11; cat "$2" && exit 12; '
                       'echo escaped > "$3" && exit 13; echo safe > "$4"',
                       "probe", str(gold), str(workspace / "gold-link"),
                       str(root / "outside"), str(workspace / "result")]
            wrapped = isolation.sandbox_command(command, workspace=workspace, runtime=runtime)
            result = subprocess.run(wrapped, capture_output=True, timeout=10,
                                    env={"HOME": str(runtime), "TMPDIR": str(runtime), "PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual((workspace / "result").read_text(), "safe\n")
            self.assertFalse((root / "outside").exists())
            self.assertNotIn(b"secret reference", result.stdout)


if __name__ == "__main__":
    unittest.main()
