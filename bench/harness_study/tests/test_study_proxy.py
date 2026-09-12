"""Loopback-only mock provider probes; these never load or invoke a model."""
import base64
from contextlib import contextmanager
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import socket
import threading
import time
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from bench.harness_study.proxy import ModelProxy


@contextmanager
def mock_provider(respond):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_POST(self):
            # Consume the complete request before closing an HTTP/1.0 reply.
            # Closing with unread inbound bytes can produce a TCP reset on macOS,
            # masking the response status or truncating an otherwise valid stream.
            self.request_body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            respond(self)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.05), daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def request(proxy, body, *, headers=None, path="/v1/chat/completions"):
    connection = HTTPConnection("127.0.0.1", urlsplit(proxy.url).port, timeout=3)
    raw = json.dumps(body).encode() if isinstance(body, (dict, list)) else body
    try:
        connection.request("POST", path, body=raw, headers={"Content-Type": "application/json", **(headers or {})})
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


class ProxyTests(unittest.TestCase):
    def test_frozen_temperature_is_checked_before_provider_without_rewriting(self):
        received, events = [], []
        def respond(handler):
            received.append(handler.request_body)
            handler.send_response(200)
            handler.end_headers()
            handler.wfile.write(b'{"model":"exact","choices":[]}')
        with mock_provider(respond) as upstream:
            with ModelProxy(upstream, "exact", events.append, "agent", expected_temperature=0.0) as proxy:
                for value in (None, True, False, "0", 0.5, float("nan"), float("inf")):
                    body = {"model": "exact"}
                    if value is not None:
                        body["temperature"] = value
                    with self.subTest(value=value):
                        self.assertEqual(request(proxy, body)[0], 400)
                raw = b'{ "model": "exact", "temperature": 0, "messages": [] }'
                self.assertEqual(request(proxy, raw)[0], 200)
        self.assertEqual(received, [raw])
        requests = [event for event in events if event['event'] == 'request']
        self.assertEqual(len(requests), 8)
        self.assertEqual(base64.b64decode(requests[-1]['raw_base64']), raw)

    def test_usage_headers_are_observed_without_forwarding_or_changing_body(self):
        received, events = [], []
        def respond(handler):
            received.append((handler.request_body, dict(handler.headers)))
            handler.send_response(200)
            handler.end_headers()
            handler.wfile.write(b'{"model":"exact","usage":{"prompt_tokens":9}}')
        raw = b'{"model":"exact","messages":[]}'
        with mock_provider(respond) as upstream, ModelProxy(upstream, "exact", events.append, "agent") as proxy:
            self.assertEqual(request(proxy, raw, headers={"x-moosedev-request-id": "wire-id",
                "x-moosedev-purpose": "harness_capture", "x-moosedev-candidate": "2"})[0], 200)
        self.assertEqual(received[0][0], raw)
        self.assertFalse(any(key.lower().startswith("x-moosedev-") for key in received[0][1]))
        metadata = next(event["usage_metadata"] for event in events if event["event"] == "request")
        self.assertEqual(metadata["client_request_id"], "wire-id")
        self.assertEqual(metadata["candidate_attempt"], 2)

    def test_stream_usage_handles_multiline_events_and_json_content_type(self):
        payloads = [
            ("text/event-stream", b'id: 7\nevent: message\nretry: 1000\ndata: {"model":"exact",\n'
             b'data: "usage":{"prompt_tokens":10}}\n\ndata: [DONE]\n\n'),
            ("application/json", b'{"model":"exact","usage":{"prompt_tokens":10}}')]
        from bench.harness_study.usage import UsageLedger, resource_metrics
        for content_type, payload in payloads:
            with self.subTest(content_type=content_type):
                ledger = UsageLedger("harness")
                def respond(handler):
                    handler.send_response(200)
                    handler.send_header("Content-Type", content_type)
                    handler.end_headers()
                    handler.wfile.write(payload)
                with mock_provider(respond) as upstream:
                    with ModelProxy(upstream, "exact", lambda event: ledger.consume("model", event), "agent") as proxy:
                        status, body = request(proxy, {"model": "exact", "stream": True})
                self.assertEqual((status, body), (200, payload))
                self.assertEqual(resource_metrics(ledger.report())["input_tokens"], 10)

    def test_explicit_usage_option_rejection_reaches_native_fallback_and_is_counted(self):
        from bench.harness_study.usage import UsageLedger, resource_metrics
        ledger, received = UsageLedger("harness"), []
        def respond(handler):
            body = json.loads(handler.request_body)
            received.append(body)
            if "stream_options" in body:
                handler.send_response(422)
                handler.end_headers()
                handler.wfile.write(b'{"error":"unknown field stream_options", "usage":{"prompt_tokens":0}}')
            else:
                handler.send_response(200)
                handler.send_header("Content-Type", "text/event-stream")
                handler.end_headers()
                handler.wfile.write(b'data: {"model":"exact","usage":{"prompt_tokens":10}}\n\ndata: [DONE]\n\n')
        with mock_provider(respond) as upstream:
            with ModelProxy(upstream, "exact", lambda event: ledger.consume("model", event), "agent") as proxy:
                first = {"model":"exact", "stream":True, "stream_options":{"include_usage":True}}
                self.assertEqual(request(proxy, first)[0], 422)
                self.assertEqual(request(proxy, {"model":"exact", "stream":True})[0], 200)
                self.assertEqual(proxy.failures, [])
        summary = ledger.report()["sources"]["proxy"]["summary"]
        self.assertEqual(summary["requests"], 2)
        self.assertEqual(summary["statuses"], {"error": 1, "completed": 1})
        self.assertEqual(resource_metrics(ledger.report())["input_tokens"], 10)
        self.assertEqual(received[0], first)

    def test_usage_option_pass_through_does_not_accept_arbitrary_errors(self):
        from bench.harness_study.proxy import usage_option_rejection
        for status, body, error in ((500, {"stream_options": {}}, b"stream_options unsupported"),
                (400, {}, b"stream_options unsupported"), (400, {"stream_options": {}}, b"backend failed"),
                (302, {"stream_options": {}}, b"unknown stream_options"),
                (400, {"stream_options": {}}, b"unknown stream_options" + b" " * 65536)):
            self.assertFalse(usage_option_rejection(status, body, error))

    def test_invalid_frozen_temperature_is_rejected_at_setup(self):
        for value in (True, False, "0", -1, 3, float("nan"), float("inf")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                ModelProxy("http://127.0.0.1:1234/v1", "exact", lambda event: None,
                           "agent", expected_temperature=value)

    def test_endpoint_identity_is_explicit_and_local(self):
        for endpoint in ("https://127.0.0.1:1234/v1", "http://localhost:1234/v1", "http://127.0.0.1/v1",
                         "http://127.0.0.1:1234/other", "http://user:password@127.0.0.1:1234/v1",
                         "http://127.0.0.1:1234/v1?key=secret", "http://127.0.0.1:1234/v1#fragment"):
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                ModelProxy(endpoint, "exact", lambda event: None, "agent")

    def test_direct_request_ignores_proxy_environment_and_never_forwards_auth(self):
        received = []
        payload = json.dumps({"model": "exact", "choices": []}).encode()
        def respond(handler):
            body = handler.request_body
            received.append((handler.path, dict(handler.headers), body))
            handler.send_response(200)
            handler.end_headers()
            handler.wfile.write(payload)
        events = []
        with mock_provider(respond) as upstream, patch.dict(os.environ, {
                "http_proxy": "http://127.0.0.1:1", "HTTP_PROXY": "http://127.0.0.1:1", "NO_PROXY": ""}):
            with ModelProxy(upstream, "exact", events.append, "agent") as proxy:
                status, body = request(proxy, {"model": "exact"}, headers={"Authorization": "Bearer must-not-forward"})
        self.assertEqual((status, body), (200, payload))
        self.assertEqual(received[0][0], "/v1/chat/completions")
        self.assertNotIn("Authorization", received[0][1])
        chunks = b"".join(base64.b64decode(event["raw_base64"]) for event in events if event["event"] == "response_chunk")
        self.assertEqual(chunks, payload)
        self.assertNotIn("must-not-forward", json.dumps(events))

    def test_redirects_are_rejected_without_following_location(self):
        calls = []
        def respond(handler):
            calls.append(handler.path)
            handler.send_response(307)
            handler.send_header("Location", "http://127.0.0.1:1/private")
            handler.end_headers()
        with mock_provider(respond) as upstream, ModelProxy(upstream, "exact", lambda event: None, "agent") as proxy:
            status, _ = request(proxy, {"model": "exact"})
            self.assertEqual(status, 502)
            self.assertTrue(any("307" in failure for failure in proxy.failures))
            self.assertEqual(proxy.failure_details[0]["error_type"], "ValueError")
            self.assertEqual(proxy.failure_details[0]["stage"], "upstream")
            self.assertEqual(proxy.failure_details[0]["failure_origin"], "upstream")
            self.assertTrue(proxy.failure_details[0]["response_started"] is False)
        self.assertEqual(calls, ["/v1/chat/completions"])

    def test_malformed_or_wrong_model_requests_get_400_without_upstream_calls(self):
        calls = []
        with mock_provider(lambda handler: calls.append(True)) as upstream:
            with ModelProxy(upstream, "exact", lambda event: None, "agent") as proxy:
                for body in (b"not-json", [], {"model": "different"}, {"model": "exact", "stream": "yes"}):
                    with self.subTest(body=body):
                        self.assertEqual(request(proxy, body)[0], 400)
        self.assertEqual(calls, [])

    def test_nonstream_identity_is_checked_before_exposing_content(self):
        for response in ({"model": "other", "secret": "MISMATCHED_PAYLOAD"},
                         {"secret": "MISSING_IDENTITY"}):
            payload = json.dumps(response).encode()
            def respond(handler):
                handler.send_response(200)
                handler.end_headers()
                handler.wfile.write(payload)
            events = []
            with mock_provider(respond) as upstream, ModelProxy(upstream, "exact", events.append, "agent") as proxy:
                status, body = request(proxy, {"model": "exact"})
                self.assertEqual(status, 502)
                self.assertNotIn(response["secret"].encode(), body)
            chunks = b"".join(base64.b64decode(event["raw_base64"]) for event in events if event["event"] == "response_chunk")
            self.assertEqual(chunks, payload)

    def test_stream_fragments_are_buffered_and_preserved_losslessly(self):
        payload = b'data: {"model":"exact","choices":[]}\r\n\r\ndata: [DONE]\n\n'
        def respond(handler):
            handler.send_response(200)
            handler.end_headers()
            for piece in (payload[:9], payload[9:23], payload[23:]):
                handler.wfile.write(piece)
                handler.wfile.flush()
        events = []
        with mock_provider(respond) as upstream, ModelProxy(upstream, "exact", events.append, "agent") as proxy:
            self.assertEqual(request(proxy, {"model": "exact", "stream": True}), (200, payload))
            self.assertEqual(proxy.failures, [])
        chunks = b"".join(base64.b64decode(event["raw_base64"]) for event in events if event["event"] == "response_chunk")
        self.assertEqual(chunks, payload)

    def test_split_mismatched_stream_record_never_reaches_client(self):
        payload = b'data: {"model":"other","secret":"FORBIDDEN"}\n\ndata: [DONE]\n\n'
        def respond(handler):
            handler.send_response(200)
            handler.end_headers()
            handler.wfile.write(payload[:17])
            handler.wfile.flush()
            handler.wfile.write(payload[17:])
        with mock_provider(respond) as upstream, ModelProxy(upstream, "exact", lambda event: None, "agent") as proxy:
            status, body = request(proxy, {"model": "exact", "stream": True})
            self.assertEqual(status, 502)
            self.assertNotIn(b"FORBIDDEN", body)
            self.assertTrue(proxy.failures)

    def test_stream_without_completion_is_recorded_as_failure(self):
        def respond(handler):
            handler.send_response(200)
            handler.end_headers()
            handler.wfile.write(b'data: {"model":"exact","choices":[]}\n\n')
        with mock_provider(respond) as upstream, ModelProxy(upstream, "exact", lambda event: None, "agent") as proxy:
            request(proxy, {"model": "exact", "stream": True})
            self.assertTrue(any("incomplete framing" in failure for failure in proxy.failures))

    def test_shutdown_interrupts_upstream_read_and_prevents_late_evidence(self):
        blocked = threading.Event()
        release = threading.Event()
        def respond(handler):
            handler.send_response(200)
            handler.end_headers()
            handler.wfile.flush()
            blocked.set()
            release.wait(8)
        events = []
        with mock_provider(respond) as upstream:
            proxy = ModelProxy(upstream, "exact", events.append, "agent")
            proxy.__enter__()
            client_errors = []
            def client():
                try:
                    request(proxy, {"model": "exact"})
                except Exception as error:
                    client_errors.append(type(error).__name__)
            thread = threading.Thread(target=client, daemon=True)
            thread.start()
            try:
                self.assertTrue(blocked.wait(2))
                started = time.monotonic()
                proxy.__exit__(None, None, None)
                self.assertLess(time.monotonic() - started, 5)
                thread.join(timeout=2)
                self.assertFalse(thread.is_alive())
                self.assertFalse(proxy._handlers)
                frozen = len(events)
                proxy.emit({"event": "late-attempt"})
                self.assertEqual(len(events), frozen)
                self.assertTrue(any(event["event"] == "shutdown" for event in events))
            finally:
                release.set()

    def test_shutdown_interrupts_client_with_partial_request_body(self):
        with mock_provider(lambda handler: None) as upstream:
            proxy = ModelProxy(upstream, "exact", lambda event: None, "agent")
            proxy.__enter__()
            client = socket.create_connection(("127.0.0.1", urlsplit(proxy.url).port), timeout=2)
            try:
                client.sendall(b"POST /v1/chat/completions HTTP/1.0\r\nContent-Length: 100\r\n\r\n{")
                deadline = time.monotonic() + 2
                while not proxy._handlers and time.monotonic() < deadline:
                    time.sleep(0.005)
                proxy.__exit__(None, None, None)
                self.assertFalse(proxy._handlers)
            finally:
                client.close()


if __name__ == "__main__":
    unittest.main()
