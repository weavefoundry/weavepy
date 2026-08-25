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

# gevent-native legs (RFC 0072 WS2): a Greenlet subclass — the shape a
# compiled Cython subclass takes over the C-API type — plus queue and
# Timeout.
import gevent.queue


class Doubler(gevent.Greenlet):
    def __init__(self, n):
        super().__init__()
        self.n = n

    def _run(self):
        gevent.sleep(0)
        return self.n * 2


doublers = [Doubler(i) for i in range(4)]
for d in doublers:
    d.start()
gevent.joinall(doublers, timeout=30)
assert [d.value for d in doublers] == [0, 2, 4, 6], [d.value for d in doublers]

# queue producer/consumer: bounded queue forces the producer to block
# cooperatively until the consumer drains.
q = gevent.queue.Queue(maxsize=2)
consumed = []


def producer():
    for i in range(6):
        q.put(i)
    q.put(StopIteration)


def consumer():
    for item in q:
        consumed.append(item)


gevent.joinall([gevent.spawn(producer), gevent.spawn(consumer)], timeout=30)
assert consumed == list(range(6)), consumed

# Timeout: fires inside a blocking sleep as gevent.Timeout.
try:
    with gevent.Timeout(0.05):
        gevent.sleep(5)
    raise AssertionError("Timeout did not fire")
except gevent.Timeout:
    pass

print("gevent ok", gevent.__version__)
