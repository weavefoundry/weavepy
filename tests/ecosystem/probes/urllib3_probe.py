"""Ecosystem probe: urllib3 (standalone) — PoolManager GET against a
local http.server, retry configuration, and response body handling."""

import http.server
import json
import threading

import urllib3
from urllib3.util.retry import Retry


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        payload = json.dumps({"path": self.path}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass


httpd = http.server.HTTPServer(("127.0.0.1", 0), Handler)
port = httpd.server_address[1]
threading.Thread(
    target=lambda: httpd.serve_forever(poll_interval=0.05), daemon=True
).start()

http_pool = urllib3.PoolManager(retries=Retry(total=3, backoff_factor=0.1))
r = http_pool.request("GET", f"http://127.0.0.1:{port}/pool?x=2", timeout=10.0)
assert r.status == 200, r.status
assert json.loads(r.data)["path"] == "/pool?x=2"

# Retry object arithmetic (no network): decrementing consumes the budget.
retry = Retry(total=2)
retry2 = retry.increment(method="GET", url="/", response=None, error=None)
assert retry2.total == 1, retry2.total

httpd.shutdown()
print("urllib3 ok", urllib3.__version__)
