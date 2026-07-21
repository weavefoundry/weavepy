"""Ecosystem probe: requests — GET against a local http.server, HTTPS
against a local TLS server when the harness provides a cert pair via
$WEAVEPY_ECOSYSTEM_CERTS."""

import http.server
import json
import os
import ssl
import threading

import requests


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"path": self.path, "ua": self.headers.get("User-Agent", "")})
        payload = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass


def serve(server):
    with server:
        server.serve_forever(poll_interval=0.05)


# --- plain HTTP ---------------------------------------------------------
httpd = http.server.HTTPServer(("127.0.0.1", 0), Handler)
port = httpd.server_address[1]
t = threading.Thread(target=serve, args=(httpd,), daemon=True)
t.start()

r = requests.get(f"http://127.0.0.1:{port}/hello?x=1", timeout=10)
assert r.status_code == 200, r.status_code
data = r.json()
assert data["path"] == "/hello?x=1"
assert data["ua"].startswith("python-requests/")
httpd.shutdown()

# --- HTTPS with a local self-signed cert --------------------------------
certs = os.environ.get("WEAVEPY_ECOSYSTEM_CERTS")
if certs:
    cert = os.path.join(certs, "localhost.cert")
    key = os.path.join(certs, "localhost.key")
    if os.path.exists(cert) and os.path.exists(key):
        httpsd = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert, key)
        httpsd.socket = ctx.wrap_socket(httpsd.socket, server_side=True)
        sport = httpsd.server_address[1]
        threading.Thread(target=serve, args=(httpsd,), daemon=True).start()

        r = requests.get(f"https://127.0.0.1:{sport}/tls", timeout=10, verify=cert)
        assert r.status_code == 200 and r.json()["path"] == "/tls"
        httpsd.shutdown()
        print("https ok")

print("requests ok", requests.__version__)
