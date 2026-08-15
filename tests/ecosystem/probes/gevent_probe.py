"""Ecosystem probe: gevent (PyPI wheel) — monkey-patches, runs a
gevent.spawn fan-out with joinall, asserts gevent.sleep cooperative
ordering, and echoes through a loopback socket via the patched socket
module. Rides the RFC 0066 WS4 native greenlet; measured honestly —
gevent's compiled Cython modules bind the greenlet C-API capsule."""

from gevent import monkey

monkey.patch_all()

import socket

import gevent

# spawn fan-out + joinall
results = []
greenlets = [gevent.spawn(lambda i=i: results.append(i * i)) for i in range(8)]
gevent.joinall(greenlets, timeout=30)
assert sorted(results) == [i * i for i in range(8)], results
assert all(g.successful() for g in greenlets)

# gevent.sleep ordering: shorter sleeps wake first across greenlets.
order = []


def sleeper(tag, delay):
    gevent.sleep(delay)
    order.append(tag)


gevent.joinall(
    [
        gevent.spawn(sleeper, "slow", 0.20),
        gevent.spawn(sleeper, "mid", 0.10),
        gevent.spawn(sleeper, "fast", 0.01),
    ],
    timeout=30,
)
assert order == ["fast", "mid", "slow"], order

# Loopback echo through the patched socket module: server and client
# run as greenlets in the same thread — only cooperative yielding can
# make this terminate.
server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", 0))
server.listen(1)
port = server.getsockname()[1]


def echo_once():
    conn, _ = server.accept()
    data = conn.recv(1024)
    conn.sendall(data.upper())
    conn.close()


def client():
    sock = socket.create_connection(("127.0.0.1", port), timeout=10)
    sock.sendall(b"weavepy echo")
    reply = sock.recv(1024)
    sock.close()
    return reply


srv = gevent.spawn(echo_once)
cli = gevent.spawn(client)
gevent.joinall([srv, cli], timeout=30)
assert cli.value == b"WEAVEPY ECHO", cli.value
server.close()

print("gevent ok", gevent.__version__)
