"""RFC 0042 WS3 — `http.client` + `http.server` loopback round-trip.

Runs a real `http.server.HTTPServer` in a background thread and drives it with
`http.client.HTTPConnection`: a GET (chunked-free, Content-Length body), a POST
with a request body echoed back, and header round-tripping. Exercises the
socket-backed `HTTPResponse` over `sock.makefile('rb')` (sandbox-safe loopback).
"""

import threading
from http.server import HTTPServer, BaseHTTPRequestHandler
import http.client


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass  # silence

    def do_GET(self):
        body = b"hello from GET " + self.path.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Echo-Path", self.path)
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        data = self.rfile.read(n)
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)  # echo


server = HTTPServer(("127.0.0.1", 0), Handler)
host, port = server.server_address[:2]
t = threading.Thread(target=server.serve_forever)
t.daemon = True
t.start()

try:
    # --- GET -----------------------------------------------------------------
    conn = http.client.HTTPConnection(host, port, timeout=10)
    conn.request("GET", "/abc")
    resp = conn.getresponse()
    assert resp.status == 200, resp.status
    assert resp.getheader("X-Echo-Path") == "/abc"
    assert resp.read() == b"hello from GET /abc"
    conn.close()

    # --- POST echo -----------------------------------------------------------
    conn = http.client.HTTPConnection(host, port, timeout=10)
    payload = b"the quick brown fox" * 64
    conn.request("POST", "/echo", body=payload,
                 headers={"Content-Type": "application/octet-stream"})
    resp = conn.getresponse()
    assert resp.status == 200
    assert resp.read() == payload
    conn.close()

    # --- persistent connection: two GETs over one connection -----------------
    conn = http.client.HTTPConnection(host, port, timeout=10)
    for p in ("/one", "/two"):
        conn.request("GET", p)
        r = conn.getresponse()
        assert r.read() == b"hello from GET " + p.encode()
    conn.close()
finally:
    server.shutdown()
    server.server_close()
    t.join(timeout=10)

print("WS3 http.client/http.server fixture ok")
