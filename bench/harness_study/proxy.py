"""Lossless local inference evidence with checked identities and bounded shutdown.

Only an explicit unauthenticated http://127.0.0.1:PORT/v1 upstream is accepted.
Direct HTTPConnection avoids environment proxies and never follows redirects.
The driver owns this proxy outside the agent sandbox; credentials and management
routes are never forwarded. Raw received chunks are archived before validation.
"""
import base64
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import math
import re
import socket
import threading
import time
from urllib.parse import urlsplit
import uuid

MAX_REQUEST_BYTES = 8 * 1024 * 1024
MAX_RESPONSE_BYTES = 32 * 1024 * 1024
SHUTDOWN_SECONDS = 5


class ModelProxy:
    def __init__(self, upstream, model, record, role, *, expected_temperature=None):
        address = urlsplit(upstream)
        if (address.scheme != "http" or address.hostname != "127.0.0.1"
                or not address.port or address.path.rstrip("/") != "/v1"
                or address.username is not None or address.password is not None
                or address.query or address.fragment):
            raise ValueError("proxy upstream must be explicit unauthenticated http://127.0.0.1:PORT/v1")
        if not isinstance(model, str) or not model.strip():
            raise ValueError("exact proxy model is required")
        if expected_temperature is not None and (
                type(expected_temperature) not in (int, float)
                or not math.isfinite(expected_temperature) or not 0 <= expected_temperature <= 2):
            raise ValueError("expected temperature must be a finite number between zero and two")
        self.expected_temperature = expected_temperature
        self.upstream = upstream.rstrip("/")
        self._port = address.port
        self.model, self.record, self.role = model, record, role
        self.failures = []
        self.failure_details = []
        self._state = threading.Lock()
        self._record_lock = threading.Lock()
        self._stopping = threading.Event()
        self._recording_closed = False
        self._handlers = {}
        self._connections = {}
        self._entered = False
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def setup(self):
                self.request.settimeout(30)
                super().setup()

            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.path != "/v1/models":
                    self.send_error(404)
                    return
                data = json.dumps({"object": "list", "data": [{"id": owner.model, "object": "model"}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                request_id = str(uuid.uuid4())
                sent_headers = False
                upstream_connection = None
                response = None
                stage = "request"
                failure_origin = "request"
                started = None
                response_body_complete = False
                request_metadata = {}

                def forward(data, content_type, status=200):
                    nonlocal sent_headers, failure_origin
                    previous_origin = failure_origin
                    failure_origin = "downstream"
                    if not sent_headers:
                        self.send_response(status)
                        self.send_header("Content-Type", content_type)
                        self.end_headers()
                        sent_headers = True
                    self.wfile.write(data)
                    self.wfile.flush()
                    failure_origin = previous_origin

                try:
                    if self.path != "/v1/chat/completions":
                        self.send_error(404)
                        return
                    if self.headers.get("Transfer-Encoding"):
                        raise ValueError("chunked request bodies are unsupported")
                    size = int(self.headers.get("Content-Length", "0"))
                    if not 0 < size <= MAX_REQUEST_BYTES:
                        raise ValueError("request body missing or exceeds 8 MiB")
                    raw = self.rfile.read(size)
                    if len(raw) != size:
                        raise ValueError("incomplete request body")
                    # Retain malformed bytes too; requests never include forwarded auth.
                    owner.emit({"id": request_id, "event": "request_bytes",
                                "raw_base64": base64.b64encode(raw).decode()})
                    body = json.loads(raw)
                    if not isinstance(body, dict) or body.get("model") != owner.model:
                        raise ValueError("native client requested an unexpected model")
                    if "stream" in body and not isinstance(body["stream"], bool):
                        raise ValueError("stream must be boolean")
                    owner.emit({"id": request_id, "event": "request", "body": body,
                                "raw_base64": base64.b64encode(raw).decode(),
                                "usage_metadata": usage_metadata(self.headers)})
                    request_metadata = usage_metadata(self.headers)
                    temperature = body.get("temperature")
                    if owner.expected_temperature is not None and (
                            type(temperature) not in (int, float)
                            or temperature != owner.expected_temperature):
                        raise ValueError("native client temperature differs from the frozen generation policy")
                    started = time.monotonic()
                    stage = "upstream"
                    failure_origin = "upstream"
                    upstream_connection = HTTPConnection("127.0.0.1", owner._port, timeout=2)
                    upstream_connection.connect()
                    upstream_connection.sock.settimeout(1200)
                    with owner._state:
                        if owner._stopping.is_set():
                            raise OSError("proxy is stopping")
                        # HTTP/1.0 getresponse() may detach connection.sock while
                        # its response file still blocks. Retain the actual socket.
                        owner._connections[upstream_connection] = upstream_connection.sock
                    upstream_connection.request("POST", "/v1/chat/completions", body=raw,
                                                headers={"Content-Type": "application/json"})
                    response = upstream_connection.getresponse()
                    owner.emit({"id": request_id, "event": "response_headers",
                                "status": response.status,
                                "content_type": response.getheader("Content-Type")})
                    streaming_response = bool(body.get("stream")) and "application/json" not in (response.getheader("Content-Type") or "").lower()
                    pending = b""
                    prelude = b""
                    event_data = []
                    event_wire = b""
                    total = 0
                    saw_response = False
                    done = False
                    while chunk := response.read1(65536):
                        owner.emit({"id": request_id, "event": "response_chunk",
                                    "raw_base64": base64.b64encode(chunk).decode()})
                        total += len(chunk)
                        if total > MAX_RESPONSE_BYTES:
                            raise ValueError("provider response exceeds 32 MiB")
                        pending += chunk
                        if response.status != 200 or not streaming_response:
                            continue
                        while b"\n" in pending:
                            line, pending = pending.split(b"\n", 1)
                            wire_line = line + b"\n"
                            value = line.rstrip(b"\r")
                            event_wire += wire_line
                            if value.startswith(b"data:"):
                                payload = value[5:]
                                event_data.append(payload[1:] if payload.startswith(b" ") else payload)
                            elif value and not value.startswith((b":", b"event:", b"id:", b"retry:")):
                                raise ValueError("unsupported provider SSE framing")
                            elif not value:
                                if event_data:
                                    payload = b"\n".join(event_data)
                                    event_data = []
                                    if done:
                                        raise ValueError("provider sent data after stream completion")
                                    if payload == b"[DONE]":
                                        if not saw_response:
                                            raise ValueError("provider completed without an identified response")
                                        done = True
                                    else:
                                        owner.check_response(json.loads(payload))
                                        saw_response = True
                                if saw_response:
                                    forward(prelude + event_wire, "text/event-stream")
                                    prelude = b""
                                else:
                                    prelude += event_wire
                                event_wire = b""
                    owner.emit({"id": request_id, "event": "response_body_complete"})
                    response_body_complete = True
                    if usage_option_rejection(response.status, body, pending):
                        owner.emit({"id": request_id, "event": "error", "compatibility_rejection": True,
                                    "error": "provider explicitly rejected stream usage reporting",
                                    "elapsed_seconds": time.monotonic() - started})
                        forward(pending, response.getheader("Content-Type") or "text/plain", response.status)
                        return
                    if response.status != 200:
                        # Includes 3xx: never follow Location, even to localhost.
                        raise ValueError(f"provider returned HTTP {response.status}")
                    if streaming_response:
                        if pending or event_data or event_wire or not done:
                            raise ValueError("provider stream ended with incomplete framing")
                    else:
                        owner.check_response(json.loads(pending))
                        forward(pending, "application/json")
                    owner.emit({"id": request_id, "event": "complete", "elapsed_seconds": time.monotonic() - started})
                except Exception as error:
                    if not owner._stopping.is_set():
                        occurred_at = time.monotonic()
                        with owner._state:
                            owner.failures.append(str(error))
                            owner.failure_details.append({
                                "request_id": request_id,
                                "client_request_id": request_metadata.get("client_request_id"),
                                "purpose": request_metadata.get("purpose"),
                                "error_type": type(error).__name__, "error": str(error),
                                "stage": stage, "failure_origin": failure_origin,
                                "occurred_at_monotonic": occurred_at,
                                "response_started": sent_headers,
                                "response_body_complete": response_body_complete,
                            })
                        owner.emit({"id": request_id, "event": "error", "error": str(error),
                                    "error_type": type(error).__name__,
                                    "failure_origin": failure_origin,
                                    "elapsed_seconds": time.monotonic() - started if started is not None else None})
                        if not sent_headers:
                            try:
                                self.send_error(400 if stage == "request" else 502,
                                                "Invalid study request" if stage == "request" else "Invalid provider response")
                            except OSError:
                                pass
                    self.close_connection = True
                finally:
                    if upstream_connection is not None:
                        with owner._state:
                            owner._connections.pop(upstream_connection, None)
                        if response is not None:
                            response.close()
                        upstream_connection.close()

        class Server(HTTPServer):
            def process_request(self, request, client_address):
                def handle():
                    try:
                        self.finish_request(request, client_address)
                    except OSError:
                        pass
                    finally:
                        self.shutdown_request(request)
                        with owner._state:
                            owner._handlers.pop(threading.current_thread(), None)
                thread = threading.Thread(target=handle, daemon=True)
                with owner._state:
                    if owner._stopping.is_set():
                        request.close()
                        return
                    owner._handlers[thread] = request
                    # Register and start under one lock: shutdown never joins an
                    # unstarted thread or misses a just-accepted connection.
                    thread.start()

        self.server = Server(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}/v1"

    def emit(self, event):
        with self._record_lock:
            if not self._recording_closed:
                self.record({"role": self.role, "model": self.model, **event})

    def check_response(self, body):
        if not isinstance(body, dict) or body.get("model") != self.model:
            raise ValueError("provider response has missing or mismatched model identity")

    def __enter__(self):
        if self._entered:
            raise RuntimeError("proxy instances cannot be restarted")
        self._entered = True
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.1), daemon=True)
        self.thread.start()
        return self

    @staticmethod
    def _abort(connection):
        sock = connection.sock if isinstance(connection, HTTPConnection) else connection
        if sock is not None:
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        connection.close()

    def __exit__(self, *args):
        deadline = time.monotonic() + SHUTDOWN_SECONDS
        with self._state:
            self._stopping.set()
            handlers = dict(self._handlers)
            connections = list(self._connections.items())
        self.emit({"event": "shutdown", "interrupted_handlers": len(handlers)})
        # Disable callbacks before waiting: even failed shutdown cannot append to
        # a bundle the caller later seals. Partial streams remain archived above.
        with self._record_lock:
            self._recording_closed = True
        for connection, upstream_socket in connections:
            self._abort(upstream_socket)
            connection.close()
        for client in handlers.values():
            self._abort(client)
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=max(0, deadline - time.monotonic()))
        for thread in handlers:
            thread.join(timeout=max(0, deadline - time.monotonic()))
        if self.thread.is_alive() or any(thread.is_alive() for thread in handlers):
            raise RuntimeError("model proxy could not stop all request handlers; retain incomplete run evidence")


def usage_metadata(headers):
    """Keep only bounded, non-secret observation IDs; do not forward headers."""
    result = {}
    for header, field in (("request-id", "client_request_id"), ("purpose", "purpose"),
                          ("decision-id", "decision_id"), ("candidate", "candidate_attempt")):
        value = headers.get("x-moosedev-" + header)
        if isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", value):
            if field == "candidate_attempt":
                if value.isascii() and value.isdigit() and 0 < int(value) < 256:
                    result[field] = int(value)
            else:
                result[field] = value
    return result


def usage_option_rejection(status, body, response):
    """Only explicit bounded pre-generation option negotiation passes through."""
    if status not in {400, 422} or "stream_options" not in body or len(response) > 65536:
        return False
    lower = response.decode("utf-8", errors="replace").lower()
    return (any(field in lower for field in ("stream_options", "include_usage"))
            and any(reason in lower for reason in ("unsupported", "not supported", "unrecognized",
                "unknown", "not permitted", "not allowed", "unexpected", "extra inputs")))
