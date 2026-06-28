"""RFC 0042 WS2 — real TLS over rustls: server + client `wrap_socket`.

Stands up a loopback TCP listener, wraps the *accepted* fd server-side with a
checked-in self-signed cert (`certs/localhost.*`), connects a client that wraps
its fd client-side, completes a real handshake, and exchanges application data
both directions. This is the core WS2 deliverable: attach a rustls session to
an *existing* socket fd for both roles (sandbox-safe loopback, no external
network, no live certificate authority).
"""

import os
import socket
import ssl
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
CERT = os.path.join(HERE, "certs", "localhost.cert")
KEY = os.path.join(HERE, "certs", "localhost.key")
assert os.path.exists(CERT), CERT
assert os.path.exists(KEY), KEY

listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 0))
listener.listen(1)
host, port = listener.getsockname()[:2]

server_error = []


def serve():
    try:
        raw, _ = listener.accept()
        raw.settimeout(15)
        sctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        sctx.load_cert_chain(CERT, KEY)
        tls = sctx.wrap_socket(raw, server_side=True)
        # Read a request line, echo it upper-cased.
        data = tls.recv(1024)
        tls.sendall(b"S:" + data.upper())
        # Confirm protocol/cipher are populated server-side.
        assert tls.version() is not None
        assert tls.cipher() is not None
        tls.close()
    except Exception as e:  # surface to the main thread
        server_error.append(repr(e))


t = threading.Thread(target=serve)
t.start()

try:
    cctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    cctx.check_hostname = False
    cctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection((host, port), timeout=15)
    raw.settimeout(15)
    tls = cctx.wrap_socket(raw, server_hostname="localhost")
    assert tls.version().startswith("TLS"), tls.version()
    assert tls.cipher() is not None
    tls.sendall(b"ping")
    reply = tls.recv(1024)
    assert reply == b"S:PING", reply
    tls.close()
finally:
    t.join(timeout=15)
    listener.close()

assert not server_error, "server thread error: " + "; ".join(server_error)

print("WS2 ssl wrap_socket fixture ok")
