"""Exact-host HTTPS CONNECT gateway; TLS content remains opaque and unrecorded."""
from http.server import BaseHTTPRequestHandler, HTTPServer
import ipaddress
import select
import socket
import threading
import time
from urllib.parse import urlsplit


def _hostname(value):
    if not value or any(c.isspace() for c in value) or value.endswith('.'):
        raise ValueError("invalid configured HTTPS hostname")
    result = value.encode("idna").decode("ascii").lower()
    if any(not part or part.startswith('-') or part.endswith('-') or
           any(not (c.isalnum() or c == '-') for c in part) for part in result.split('.')):
        raise ValueError("invalid configured HTTPS hostname")
    return result


def _public_addresses(host):
    addresses = sorted({item[4][0] for item in socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)})
    if not addresses or any(not ipaddress.ip_address(address).is_global for address in addresses):
        raise ValueError("hosted endpoint must resolve only to public addresses")
    return addresses


class DomainProxy:
    def __init__(self, endpoints, record):
        self.hosts = set()
        for endpoint in endpoints:
            parsed = urlsplit(endpoint)
            if (parsed.scheme != 'https' or parsed.port not in (None, 443)
                    or parsed.username is not None or parsed.password is not None
                    or parsed.path or parsed.query or parsed.fragment):
                raise ValueError("hosted endpoints must be bare HTTPS origins on port 443")
            self.hosts.add(_hostname(parsed.hostname))
        if not self.hosts:
            raise ValueError("at least one hosted HTTPS origin is required")
        self.record = record
        self._lock = threading.Lock()
        self._record_lock = threading.Lock()
        self._closed = False
        self._entered = False
        self._stop = threading.Event()
        self._sockets = set()
        self._threads = set()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.0'
            def log_message(self, *args):
                pass
            def setup(self):
                self.request.settimeout(5)
                super().setup()
            def do_CONNECT(self):
                upstream = None
                counts = [0, 0]
                started = time.monotonic()
                connected = False
                host = None
                selected = None
                try:
                    if self.path.count(':') != 1:
                        raise ValueError("CONNECT requires hostname:443")
                    name, port = self.path.rsplit(':', 1)
                    host = _hostname(name)
                    if port != '443' or host not in owner.hosts:
                        raise ValueError("CONNECT destination is not allowlisted")
                    addresses = _public_addresses(host)
                    selected = addresses[0]
                    upstream = socket.socket(socket.AF_INET6 if ':' in selected else socket.AF_INET, socket.SOCK_STREAM)
                    upstream.settimeout(3)
                    with owner._lock:
                        if owner._stop.is_set():
                            raise OSError("proxy stopping")
                        owner._sockets.add(upstream)
                    upstream.connect((selected, 443))
                    owner.emit({'event': 'connect', 'host': host, 'port': 443, 'resolved_ips': addresses,
                                'connected_ip': selected})
                    self.send_response(200, 'Connection Established')
                    self.end_headers()
                    connected = True
                    self.connection.settimeout(2)
                    upstream.settimeout(2)
                    # A conforming HTTPS proxy client waits for 200 before TLS;
                    # HTTP buffered headers are never interpreted as tunnel data.
                    while not owner._stop.is_set() and time.monotonic() - started < 1200:
                        readable, _, _ = select.select([self.connection, upstream], [], [], 0.1)
                        for source in readable:
                            data = source.recv(65536)
                            if not data:
                                return
                            direction = 0 if source is self.connection else 1
                            target = upstream if direction == 0 else self.connection
                            pending = memoryview(data)
                            while pending:
                                sent = target.send(pending)
                                if not sent:
                                    raise OSError("tunnel stopped accepting bytes")
                                counts[direction] += sent
                                pending = pending[sent:]
                except (OSError, ValueError, UnicodeError):
                    # Do not retain paths, headers, exception text, or credentials.
                    if not connected:
                        try:
                            self.send_error(403, 'HTTPS destination unavailable or denied')
                        except OSError:
                            pass
                finally:
                    owner.emit({'event': 'end', 'host': host if host in owner.hosts else None,
                                'port': 443, 'connected_ip': selected, 'connected': connected,
                                'upstream_bytes': counts[0], 'downstream_bytes': counts[1]})
                    if upstream is not None:
                        owner.close_socket(upstream)
                    self.close_connection = True

        class Server(HTTPServer):
            def process_request(self, request, address):
                def handle():
                    try:
                        self.finish_request(request, address)
                    except OSError:
                        pass
                    finally:
                        owner.close_socket(request)
                        with owner._lock:
                            owner._threads.discard(threading.current_thread())
                thread = threading.Thread(target=handle, daemon=True)
                with owner._lock:
                    if owner._stop.is_set():
                        request.close()
                        return
                    owner._sockets.add(request)
                    owner._threads.add(thread)
                    thread.start()
        self.server = Server(('127.0.0.1', 0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_port}'

    def emit(self, value):
        with self._record_lock:
            if not self._closed:
                self.record(value)

    def close_socket(self, sock):
        with self._lock:
            self._sockets.discard(sock)
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        sock.close()

    def __enter__(self):
        if self._entered:
            raise RuntimeError("hosted proxy instances cannot be restarted")
        self._entered = True
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.1), daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *args):
        deadline = time.monotonic() + 5
        with self._lock:
            self._stop.set()
            sockets, threads = list(self._sockets), list(self._threads)
        self.emit({'event': 'shutdown', 'active_tunnels': len(threads)})
        for sock in sockets:
            self.close_socket(sock)
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=max(0, deadline-time.monotonic()))
        for thread in threads:
            thread.join(timeout=max(0, deadline-time.monotonic()))
        with self._record_lock:
            self._closed = True
        if self.thread.is_alive() or any(thread.is_alive() for thread in threads):
            raise RuntimeError('hosted proxy shutdown incomplete; retain run evidence')
