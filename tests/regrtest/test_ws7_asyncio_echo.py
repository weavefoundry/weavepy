"""RFC 0039 WS7 — asyncio over the real selector event loop.

The loop's I/O core runs on the WS6 selectors: `start_server`/
`open_connection` build socket transports, `StreamReader`/`StreamWriter`
drive `drain`/`readline`, and timer/callback scheduling (`sleep`, `gather`,
`call_soon`) advances off `selector.select(timeout)`. This exercises a real
TCP round-trip on the loopback interface (sandbox-safe — no external network).
"""

import asyncio


# ---------------------------------------------------------------------------
# Coroutine scheduling: sleep + gather fan-out/fan-in.
# ---------------------------------------------------------------------------

async def add(a, b):
    await asyncio.sleep(0.01)
    return a + b


async def fanout():
    return await asyncio.gather(add(1, 2), add(3, 4), add(5, 6))


assert asyncio.run(fanout()) == [3, 7, 11]


# ---------------------------------------------------------------------------
# call_soon / call_later ordering on the loop.
# ---------------------------------------------------------------------------

async def scheduling():
    loop = asyncio.get_running_loop()
    order = []
    loop.call_soon(order.append, "soon")
    loop.call_later(0.02, order.append, "later")
    await asyncio.sleep(0.05)
    return order


assert asyncio.run(scheduling()) == ["soon", "later"]


# ---------------------------------------------------------------------------
# Full TCP echo round-trip over a SelectorEventLoop on loopback.
# ---------------------------------------------------------------------------

async def echo_roundtrip():
    async def handle(reader, writer):
        while True:
            line = await reader.readline()
            if not line:
                break
            writer.write(line.upper())
            await writer.drain()
        writer.close()

    server = await asyncio.start_server(handle, "127.0.0.1", 0)
    host, port = server.sockets[0].getsockname()[:2]

    reader, writer = await asyncio.open_connection(host, port)
    got = []
    for msg in (b"hello\n", b"world\n"):
        writer.write(msg)
        await writer.drain()
        got.append(await reader.readline())
    writer.close()
    await writer.wait_closed()

    server.close()
    await server.wait_closed()
    return got


# Guard with a timeout so a regression surfaces as a failure, not a hang.
async def guarded():
    return await asyncio.wait_for(echo_roundtrip(), timeout=10)


assert asyncio.run(guarded()) == [b"HELLO\n", b"WORLD\n"]


# ---------------------------------------------------------------------------
# run_in_executor offloads a blocking call to a worker thread (WS3 pool).
# ---------------------------------------------------------------------------

def blocking_double(x):
    return x * 2


async def via_executor():
    loop = asyncio.get_running_loop()
    return await loop.run_in_executor(None, blocking_double, 21)


assert asyncio.run(via_executor()) == 42


print("WS7 asyncio echo server ok")
