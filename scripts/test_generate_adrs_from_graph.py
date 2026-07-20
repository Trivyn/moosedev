#!/usr/bin/env python3
"""Behavioral tests for crash-stale ADR-generator address discovery."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


SCRIPT = Path(__file__).with_name("generate-adrs-from-graph.py")
SPEC = importlib.util.spec_from_file_location("generate_adrs_from_graph", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def start_server(health: dict[str, Any] | None, *, redirect: bool = False):
    requests: list[str] = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            requests.append(self.path)
            if redirect:
                self.send_response(302)
                self.send_header("Location", "/redirected-health")
                self.end_headers()
                return
            body = json.dumps(health).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, _format: str, *_args: Any) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    host, port = server.server_address
    return server, thread, requests, f"{host}:{port}"


class BackendAddressTests(unittest.TestCase):
    def test_file_discovery_requires_matching_direct_health_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="moosedev-adr-identity-") as raw_root:
            root = Path(raw_root)
            data_dir = root / ".moosedev"
            data_dir.mkdir()

            matching = {
                "status": "ok",
                "project_graph": MODULE.PROJECT_GRAPH_IRI,
                "data_dir": str(data_dir.resolve()),
            }
            server, thread, requests, addr = start_server(matching)
            try:
                (data_dir / "http.addr").write_text(addr, encoding="utf-8")
                self.assertEqual(MODULE.backend_addr(root, None), addr)
                self.assertEqual(requests, ["/api/v1/health"])
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

            mismatched = {**matching, "data_dir": str(root / "somewhere-else")}
            server, thread, requests, addr = start_server(mismatched)
            try:
                (data_dir / "http.addr").write_text(addr, encoding="utf-8")
                with self.assertRaises(SystemExit):
                    MODULE.backend_addr(root, None)
                self.assertEqual(requests, ["/api/v1/health"])
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

            server, thread, requests, addr = start_server(None, redirect=True)
            try:
                (data_dir / "http.addr").write_text(addr, encoding="utf-8")
                with self.assertRaises(SystemExit):
                    MODULE.backend_addr(root, None)
                self.assertEqual(requests, ["/api/v1/health"])
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

    def test_explicit_address_remains_an_intentional_override(self) -> None:
        with tempfile.TemporaryDirectory(prefix="moosedev-adr-explicit-") as raw_root:
            self.assertEqual(
                MODULE.backend_addr(Path(raw_root), "127.0.0.1:9999"),
                "127.0.0.1:9999",
            )


if __name__ == "__main__":
    unittest.main()
