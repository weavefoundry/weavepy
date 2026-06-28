"""RFC 0042 WS2 — socketless TLS via MemoryBIO/SSLObject (rustls memory BIO).

Drives a *complete* TLS 1.3 handshake and bidirectional application-data
exchange between a client `SSLObject` and a server `SSLObject` with **no socket
at all** — ciphertext is shuttled by hand through four `MemoryBIO` queues. This
is the non-blocking BIO surface asyncio's TLS transport sits on. Pure
in-process; uses the checked-in self-signed cert.
"""

import os
import ssl

HERE = os.path.dirname(os.path.abspath(__file__))
CERT = os.path.join(HERE, "certs", "localhost.cert")
KEY = os.path.join(HERE, "certs", "localhost.key")

# --- MemoryBIO unit behaviour (CPython MemoryBIOTests parity) ---------------
bio = ssl.MemoryBIO()
assert bio.pending == 0
assert bio.eof is False
assert bio.read() == b""
bio.write(b"foo")
assert bio.pending == 3
assert bio.eof is False
bio.write_eof()
assert bio.eof is False          # not eof until drained
assert bio.read(1) == b"f"
assert bio.eof is False
assert bio.read() == b"oo"
assert bio.eof is True           # drained + write_eof
assert bio.read() == b""
# Type checks.
for bad in ("str-not-allowed", 1, None, True):
    try:
        bio.write(bad)
    except TypeError:
        pass
    else:
        raise AssertionError("MemoryBIO.write(%r) should TypeError" % (bad,))
# Contiguous buffer types are accepted; a non-contiguous memoryview is rejected.
bio2 = ssl.MemoryBIO()
bio2.write(bytearray(b"bar"))
assert bio2.read() == b"bar"
bio2.write(memoryview(b"baz"))
assert bio2.read() == b"baz"
_nc = memoryview(bytearray(b"noncontig"))[::-2]
try:
    bio2.write(_nc)
except BufferError:
    pass
else:
    raise AssertionError("non-contiguous memoryview write should BufferError")

# SSLObject has no public constructor.
try:
    ssl.SSLObject()
except TypeError:
    pass
else:
    raise AssertionError("SSLObject() should raise TypeError")

# --- a full socketless handshake between two SSLObjects ----------------------
sctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
sctx.load_cert_chain(CERT, KEY)
cctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
cctx.check_hostname = False
cctx.verify_mode = ssl.CERT_NONE

c_in, c_out = ssl.MemoryBIO(), ssl.MemoryBIO()
s_in, s_out = ssl.MemoryBIO(), ssl.MemoryBIO()

client = cctx.wrap_bio(c_in, c_out, server_side=False, server_hostname="localhost")
server = sctx.wrap_bio(s_in, s_out, server_side=True)


def pump(src_out, dst_in):
    data = src_out.read()
    if data:
        dst_in.write(data)


def step(obj):
    try:
        obj.do_handshake()
        return True
    except ssl.SSLWantReadError:
        return False


c_done = s_done = False
for _ in range(20):
    if not c_done:
        c_done = step(client)
    pump(c_out, s_in)
    if not s_done:
        s_done = step(server)
    pump(s_out, c_in)
    if c_done and s_done:
        break
assert c_done and s_done, (c_done, s_done)

assert client.version().startswith("TLS"), client.version()
assert client.cipher() is not None
assert server.cipher() is not None

# --- application data both directions ---------------------------------------
client.write(b"ping over memory BIO")
pump(c_out, s_in)
assert server.read(4096) == b"ping over memory BIO"

server.write(b"pong over memory BIO")
pump(s_out, c_in)
assert client.read(4096) == b"pong over memory BIO"

# A read with no pending ciphertext is a non-blocking WANT_READ.
try:
    client.read(4096)
except ssl.SSLWantReadError:
    pass
else:
    raise AssertionError("empty client.read should raise SSLWantReadError")

# --- bidirectional unwrap (close_notify) handshake --------------------------
# Unilateral client.unwrap() sends close_notify but raises until it reads the
# peer's; the server reads the client's and closes without raising; then the
# client reads the server's and closes too (CPython SSLObjectTests.test_unwrap).
try:
    client.unwrap()
except ssl.SSLWantReadError:
    pass
else:
    raise AssertionError("unilateral client.unwrap should raise SSLWantReadError")

s_in.write(c_out.read())
server.unwrap()          # reads client's close_notify; must not raise

c_in.write(s_out.read())
client.unwrap()          # reads server's close_notify; must not raise

print("WS2 MemoryBIO/SSLObject fixture ok")
