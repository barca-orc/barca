"""An HTTP reverse proxy in front of the S3 emulator that counts the bytes it serves per object.

Tests point barca's artifact store at the proxy (`endpoint_url`) to measure what crossed the
wire: a whole-object download and a ranged read both show up as response bytes of a GET on the
object's path. Requests are forwarded unchanged, `Host` included, so S3 signatures still verify.
"""

from __future__ import annotations

import http.client
import threading
from collections import defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlsplit

_HOP = {"connection", "keep-alive", "transfer-encoding", "content-length", "proxy-connection"}


class CountingProxy:
    def __init__(self, upstream: str):
        parts = urlsplit(upstream)
        self._upstream = (parts.hostname or "localhost", parts.port or 80)
        self._lock = threading.Lock()
        self._served: dict[str, int] = defaultdict(int)
        self._responses: list[tuple[str, int]] = []
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):  # quiet
                pass

            def _body(self) -> bytes | None:
                if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
                    out = bytearray()
                    while True:
                        size = int(self.rfile.readline().split(b";")[0].strip(), 16)
                        if size == 0:
                            while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                                pass
                            return bytes(out)
                        out += self.rfile.read(size)
                        self.rfile.readline()
                length = int(self.headers.get("Content-Length") or 0)
                return self.rfile.read(length) if length else None

            def _forward(self):
                body = self._body()
                headers = {k: v for k, v in self.headers.items() if k.lower() not in _HOP}
                if body is not None:
                    headers["Content-Length"] = str(len(body))
                conn = http.client.HTTPConnection(*proxy._upstream, timeout=60)
                try:
                    conn.request(self.command, self.path, body=body, headers=headers)
                    resp = conn.getresponse()
                    data = resp.read()
                    upstream_length = resp.getheader("Content-Length")
                    self.send_response_only(resp.status, resp.reason)
                    for k, v in resp.getheaders():
                        if k.lower() not in _HOP:
                            self.send_header(k, v)
                    if self.command == "HEAD":
                        self.send_header("Content-Length", upstream_length or "0")
                    else:
                        self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    if self.command != "HEAD":
                        self.wfile.write(data)
                finally:
                    conn.close()
                if self.command == "GET" and resp.status in (200, 206):
                    path = unquote(urlsplit(self.path).path)
                    with proxy._lock:
                        proxy._served[path] += len(data)
                        proxy._responses.append((path, len(data)))

            do_GET = do_PUT = do_POST = do_HEAD = do_DELETE = _forward

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        self.endpoint = f"http://127.0.0.1:{self._server.server_address[1]}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def reset(self) -> None:
        with self._lock:
            self._served.clear()
            self._responses.clear()

    def served(self, contains: str = "") -> dict[str, int]:
        """Bytes served per object path whose path contains `contains`."""
        with self._lock:
            return {p: n for p, n in self._served.items() if contains in p}

    def responses(self, contains: str = "") -> list[int]:
        """The size of each GET response, in order, for paths that contain `contains`."""
        with self._lock:
            return [n for p, n in self._responses if contains in p]

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
