# weavepy-skip: windows
#
# The `select`/`selectors`/`asyncio` stack is built on a Unix-only
# mio `SourceFd` backend right now. Until we wire up an IOCP / winsock
# `select(2)` adapter on Windows, this fixture is unix-only.
import asyncio
import selectors
import socket


print("--- selectors ---")
sel = selectors.DefaultSelector()
r, w = socket.socketpair()
sel.register(r.fileno(), selectors.EVENT_READ, data="reader")
w.sendall(b"ping")
events = sel.select(timeout=1.0)
print("events:", len(events))
key, mask = events[0]
print("data:", key.data)
print("read mask:", mask & selectors.EVENT_READ != 0)
sel.unregister(r.fileno())
sel.close()
r.close()
w.close()

print("--- asyncio sleep ---")

async def step(n):
    await asyncio.sleep(0.01)
    return n * 2

async def main_sleep():
    result = await step(21)
    print("sleep result:", result)

asyncio.run(main_sleep())

print("--- asyncio sock primitives ---")

async def main_sock():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    host, port = srv.getsockname()
    print("listener up on", host, "port>0:", port > 0)

    loop = asyncio.get_event_loop()

    client = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    await loop.sock_connect(client, (host, port))
    conn, addr = await loop.sock_accept(srv)
    print("accepted from:", addr[0])

    await loop.sock_sendall(client, b"hello async\n")
    data = await loop.sock_recv(conn, 64)
    print("server got:", data)

    await loop.sock_sendall(conn, b"hi back\n")
    echo = await loop.sock_recv(client, 64)
    print("client got:", echo)

    client.close()
    conn.close()
    srv.close()

asyncio.run(main_sock())

print("--- asyncio gather ---")

async def square(n):
    await asyncio.sleep(0.005)
    return n * n

async def main_gather():
    results = await asyncio.gather(square(2), square(3), square(4))
    print("gathered:", results)

asyncio.run(main_gather())
