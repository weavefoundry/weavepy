"""RFC 0042 WS1 — `socket.makefile()` over a real loopback socket.

The verbatim `http.client`/`ftplib`/`smtplib`/`imaplib`/`poplib` drivers all
speak their line protocols over `sock.makefile("rb")` / `makefile("wb")`, so
`makefile()` must return a genuine buffered `io` stream over the socket fd.
This drives a buffered line/byte round-trip on the loopback interface
(sandbox-safe — no external network).
"""

import socket
import threading

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", 0))
server.listen(1)
host, port = server.getsockname()[:2]


def serve():
    conn, _ = server.accept()
    with conn:
        rf = conn.makefile("rb")
        wf = conn.makefile("wb")
        # Echo each line upper-cased until EOF.
        for line in rf:
            wf.write(line.upper())
            wf.flush()
        rf.close()
        wf.close()


t = threading.Thread(target=serve)
t.start()

client = socket.create_connection((host, port), timeout=10)
client.settimeout(10)
try:
    cr = client.makefile("rb")
    cw = client.makefile("wb")
    for msg in (b"hello\n", b"world\n"):
        cw.write(msg)
        cw.flush()
        echoed = cr.readline()
        assert echoed == msg.upper(), (echoed, msg.upper())
    cw.close()  # half-close so the server loop sees EOF
    cr.close()
finally:
    client.close()

t.join(timeout=10)
assert not t.is_alive(), "server thread did not terminate"
server.close()

# --- text-mode makefile with newline translation ----------------------------
a, b = socket.socketpair() if hasattr(socket, "socketpair") else (None, None)
if a is not None:
    try:
        wf = a.makefile("w", encoding="utf-8")
        rf = b.makefile("r", encoding="utf-8")
        wf.write("a line\n")
        wf.flush()
        assert rf.readline() == "a line\n"
        wf.close()
        rf.close()
    finally:
        a.close()
        b.close()

print("WS1 socket.makefile fixture ok")
