"""Mock CONNECT gateway checks; no external network or model calls."""
from contextlib import contextmanager
from http.client import HTTPConnection
import socket
import threading
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from bench.harness_study.hosted_proxy import DomainProxy, _public_addresses


@contextmanager
def echo_server():
    server = socket.socket()
    server.bind(('127.0.0.1', 0))
    server.listen()
    server.settimeout(2)
    def echo():
        try:
            peer, _ = server.accept()
            with peer:
                while data := peer.recv(65536):
                    peer.sendall(data)
        except OSError:
            pass
    thread = threading.Thread(target=echo, daemon=True)
    thread.start()
    try:
        yield server.getsockname()
    finally:
        server.close()
        thread.join(timeout=2)


class HostedProxyTests(unittest.TestCase):
    def test_only_bare_https_origins_allowed(self):
        for endpoint in ('http://example.com', 'https://example.com:80', 'https://a:b@example.com',
                         'https://example.com/', 'https://example.com/path', 'https://example.com?q=1',
                         'https://example.com#fragment', 'https://exam_ple.com'):
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                DomainProxy([endpoint], lambda event: None)
        with self.assertRaises(ValueError):
            DomainProxy([], lambda event: None)

    def test_dns_rejects_every_nonpublic_result(self):
        for address in ('127.0.0.1', '10.1.2.3', '169.254.169.254', '::1', 'fc00::1'):
            with self.subTest(address=address), patch('socket.getaddrinfo', return_value=[
                    (socket.AF_INET, socket.SOCK_STREAM, 6, '', ('8.8.8.8', 443)),
                    (socket.AF_INET, socket.SOCK_STREAM, 6, '', (address, 443))]):
                with self.assertRaises(ValueError):
                    _public_addresses('example.com')
        with patch('socket.getaddrinfo', return_value=[]), self.assertRaises(ValueError):
            _public_addresses('example.com')

    def test_denied_host_private_dns_and_plain_http_never_connect(self):
        events = []
        with DomainProxy(['https://example.com'], events.append) as proxy:
            for target in ('other.example:443', 'example.com:80', 'user:pass@example.com:443',
                           'example.com:443/path'):
                with self.subTest(target=target), patch('bench.harness_study.hosted_proxy._public_addresses') as dns:
                    client = HTTPConnection('127.0.0.1', urlsplit(proxy.url).port, timeout=2)
                    client.request('CONNECT', target)
                    self.assertEqual(client.getresponse().status, 403)
                    client.close()
                    dns.assert_not_called()
            with patch('bench.harness_study.hosted_proxy._public_addresses', side_effect=ValueError('private')):
                client = HTTPConnection('127.0.0.1', urlsplit(proxy.url).port, timeout=2)
                client.request('CONNECT', 'example.com:443')
                self.assertEqual(client.getresponse().status, 403)
                client.close()
            client = HTTPConnection('127.0.0.1', urlsplit(proxy.url).port, timeout=2)
            client.request('GET', 'http://example.com/private?token=secret')
            self.assertEqual(client.getresponse().status, 501)
            client.close()
        self.assertNotIn('secret', str(events))
        self.assertNotIn('user:pass', str(events))

    def test_opaque_tunnel_counts_bytes_and_shutdown_stops_recording(self):
        events = []
        original_connect = socket.socket.connect
        with echo_server() as upstream:
            def connect(sock, address):
                # Only the proxy's selected public destination is rerouted in this test.
                if address == ('8.8.8.8', 443):
                    return original_connect(sock, upstream)
                return original_connect(sock, address)
            with patch('bench.harness_study.hosted_proxy._public_addresses', return_value=['8.8.8.8']), \
                    patch.object(socket.socket, 'connect', connect):
                proxy = DomainProxy(['https://EXAMPLE.com'], events.append)
                with proxy:
                    client = socket.create_connection(('127.0.0.1', urlsplit(proxy.url).port), timeout=2)
                    client.sendall(b'CONNECT example.com:443 HTTP/1.0\r\nProxy-Authorization: secret\r\n\r\n')
                    header = b''
                    while not header.endswith(b'\r\n\r\n'):
                        header += client.recv(1)
                    self.assertIn(b' 200 ', header)
                    opaque = b'\x16\x03\x01opaque TLS payload with private data'
                    client.sendall(opaque)
                    received = b''
                    while len(received) < len(opaque):
                        received += client.recv(65536)
                    self.assertEqual(received, opaque)
                    # Leave tunnel open; context exit must interrupt and join it.
                client.close()
        self.assertFalse(proxy._threads)
        ends = [event for event in events if event['event'] == 'end']
        self.assertEqual(len(ends), 1)
        self.assertEqual(ends[0]['upstream_bytes'], len(opaque))
        self.assertEqual(ends[0]['downstream_bytes'], len(opaque))
        self.assertEqual(ends[0]['connected_ip'], '8.8.8.8')
        self.assertNotIn('secret', str(events))
        self.assertNotIn('private data', str(events))
        frozen = len(events)
        proxy.emit({'event': 'late'})
        self.assertEqual(len(events), frozen)


if __name__ == '__main__':
    unittest.main()
