"""Ecosystem probe: httpx — sync and async GET against a local
http.server (exercises httpcore, anyio, sniffio, h11)."""

import asyncio
import http.server
import json
import threading

import httpx


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

# --- sync client ---------------------------------------------------------
r = httpx.get(f"http://127.0.0.1:{port}/sync?q=1", timeout=10)
assert r.status_code == 200, r.status_code
assert r.json()["path"] == "/sync?q=1", r.json()

with httpx.Client(base_url=f"http://127.0.0.1:{port}") as client:
    r = client.get("/with-client")
    assert r.status_code == 200 and r.json()["path"] == "/with-client"


# --- async client --------------------------------------------------------
async def main():
    async with httpx.AsyncClient() as client:
        r = await client.get(f"http://127.0.0.1:{port}/async", timeout=10)
        assert r.status_code == 200, r.status_code
        assert r.json()["path"] == "/async"


asyncio.run(main())
httpd.shutdown()

print("httpx ok", httpx.__version__)
