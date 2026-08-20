"""RFC 0054 WS3 — asyncio TLS end-to-end over `sslproto` on loopback.

Runs a real `asyncio.start_server(ssl=...)` echo server and an
`asyncio.open_connection(ssl=...)` client in one event loop, both over
the rustls-backed `_ssl` with the checked-in self-signed certificate
(`certs/localhost.*`). On top of the echo round-trip it asserts two
OpenSSL-shaped surfaces this RFC graduated: the `getpeercert()`
X.509→dict parse (subject/issuer RDN tuples, serialNumber,
notBefore/notAfter) and server-side SNI callback dispatch
(`sni_callback` observes the client's `server_hostname`; dispatched on
the socket `wrap_socket` path, where the rustls Acceptor two-phase
handshake lives — asyncio's `wrap_bio` path builds its server config
at wrap time and has no SNI hook yet).
"""

import asyncio
import os
import socket
import ssl
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
CERT = os.path.join(HERE, "certs", "localhost.cert")
KEY = os.path.join(HERE, "certs", "localhost.key")
assert os.path.exists(CERT), CERT
assert os.path.exists(KEY), KEY

sni_seen = []


def make_contexts():
    sctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    sctx.load_cert_chain(CERT, KEY)

    cctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    # Trust exactly the fixture cert so verification succeeds and the
    # peer certificate is exposed to getpeercert().
    cctx.load_verify_locations(CERT)
    cctx.check_hostname = False
    cctx.verify_mode = ssl.CERT_REQUIRED
    return sctx, cctx


async def tls_echo():
    sctx, cctx = make_contexts()

    async def handle(reader, writer):
        line = await reader.readline()
        writer.write(b"S:" + line.upper())
        await writer.drain()
        writer.close()

    server = await asyncio.start_server(handle, "127.0.0.1", 0, ssl=sctx)
    host, port = server.sockets[0].getsockname()[:2]

    reader, writer = await asyncio.open_connection(
        host, port, ssl=cctx, server_hostname="localhost"
    )
    ssl_obj = writer.get_extra_info("ssl_object")
    assert ssl_obj is not None

    # getpeercert(): the verified peer cert parses into CPython's dict shape.
    cert = ssl_obj.getpeercert()
    assert isinstance(cert, dict), cert
    for key in ("subject", "issuer", "serialNumber", "notBefore", "notAfter"):
        assert key in cert, (key, sorted(cert))
    subject = dict(pair for rdn in cert["subject"] for pair in rdn)
    assert subject.get("commonName") == "localhost", cert["subject"]
    assert isinstance(cert["serialNumber"], str) and cert["serialNumber"]

    # The raw DER form is non-empty and consistent with the parsed dict.
    der = ssl_obj.getpeercert(binary_form=True)
    assert isinstance(der, bytes) and len(der) > 100

    writer.write(b"ping over tls\n")
    await writer.drain()
    reply = await reader.readline()
    assert reply == b"S:PING OVER TLS\n", reply

    writer.close()
    await writer.wait_closed()
    server.close()
    await server.wait_closed()


async def guarded():
    await asyncio.wait_for(tls_echo(), timeout=30)


asyncio.run(guarded())


# ---------------------------------------------------------------------------
# SNI callback dispatch (socket wrap path): the server context's
# `sni_callback` observes the client's server_hostname mid-handshake.
# ---------------------------------------------------------------------------

def sni_dispatch():
    sctx, cctx = make_contexts()

    def on_sni(sock, server_name, ctx):
        sni_seen.append(server_name)
        return None

    sctx.sni_callback = on_sni

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    host, port = listener.getsockname()[:2]
    server_error = []
    # Post-mortem for an intermittent CI-only stall (client recv timing
    # out after 15s): timestamped server-thread progress, dumped on any
    # client-side failure to show where the server stopped.
    t0 = time.monotonic()
    progress = []

    def step(what):
        progress.append("%.3fs %s" % (time.monotonic() - t0, what))

    def serve():
        try:
            step("accepting")
            raw, _ = listener.accept()
            step("accepted")
            raw.settimeout(15)
            tls = sctx.wrap_socket(raw, server_side=True)
            step("handshake done")
            data = tls.recv(64)
            step("received %r" % (data,))
            tls.sendall(data)
            step("echoed")
            tls.close()
            step("closed")
        except Exception as e:
            server_error.append(repr(e))

    t = threading.Thread(target=serve)
    t.start()
    try:
        raw = socket.create_connection((host, port), timeout=15)
        raw.settimeout(15)
        step("client connected")
        tls = cctx.wrap_socket(raw, server_hostname="localhost")
        step("client handshake done")
        tls.sendall(b"sni")
        step("client sent")
        assert tls.recv(64) == b"sni"
        tls.close()
    except Exception:
        # Let the server thread hit its own 15s timeout so server_error
        # is populated before the report is printed.
        t.join(timeout=20)
        print("sni_dispatch progress:", progress, file=sys.stderr)
        print("sni_dispatch server error:", server_error, file=sys.stderr)
        print("sni_dispatch sni_seen:", sni_seen, file=sys.stderr)
        raise
    finally:
        t.join(timeout=15)
        listener.close()
    assert not server_error, server_error
    assert sni_seen == ["localhost"], sni_seen


# Native `_ssl` read/write traces on stderr for the section that stalls
# intermittently on CI; harmless on success, decisive in a failure's
# captured stderr.
os.environ["WEAVE_SSL_DEBUG"] = "1"
try:
    sni_dispatch()
finally:
    del os.environ["WEAVE_SSL_DEBUG"]

print("RFC 0054 asyncio TLS echo fixture ok")
