"""Ecosystem probe: uvloop (PyPI wheel) — installs the uvloop policy,
runs a task fan-out under gather, echoes over loopback TCP through the
uvloop transports, round-trips a UDP datagram endpoint, resolves
localhost via loop.getaddrinfo, hands off to run_in_executor, and runs
a subprocess_exec echo. POSIX-only upstream (RFC 0072 WS3)."""

import asyncio
import socket
import sys

import uvloop

assert isinstance(uvloop.new_event_loop(), asyncio.AbstractEventLoop)
asyncio.set_event_loop_policy(uvloop.EventLoopPolicy())


async def main():
    loop = asyncio.get_running_loop()
    assert "uvloop" in type(loop).__module__, type(loop)

    # task fan-out under gather
    async def sq(i):
        await asyncio.sleep(0)
        return i * i

    results = await asyncio.gather(*(sq(i) for i in range(8)))
    assert results == [i * i for i in range(8)], results

    # loopback TCP echo over the uvloop transports
    async def handle(reader, writer):
        data = await reader.read(1024)
        writer.write(data.upper())
        await writer.drain()
        writer.close()

    server = await asyncio.start_server(handle, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    writer.write(b"weavepy echo")
    await writer.drain()
    reply = await reader.read(1024)
    writer.close()
    assert reply == b"WEAVEPY ECHO", reply
    server.close()
    await server.wait_closed()

    # UDP datagram endpoint round-trip
    class EchoServer(asyncio.DatagramProtocol):
        def connection_made(self, transport):
            self.transport = transport

        def datagram_received(self, data, addr):
            self.transport.sendto(data[::-1], addr)

    class Client(asyncio.DatagramProtocol):
        def __init__(self):
            self.reply = loop.create_future()

        def connection_made(self, transport):
            transport.sendto(b"datagram")

        def datagram_received(self, data, addr):
            self.reply.set_result(data)

    stransport, _ = await loop.create_datagram_endpoint(
        EchoServer, local_addr=("127.0.0.1", 0)
    )
    saddr = stransport.get_extra_info("sockname")
    ctransport, cproto = await loop.create_datagram_endpoint(
        Client, remote_addr=("127.0.0.1", saddr[1])
    )
    reply = await asyncio.wait_for(cproto.reply, timeout=10)
    assert reply == b"margatad", reply
    ctransport.close()
    stransport.close()

    # getaddrinfo through the loop
    infos = await loop.getaddrinfo("localhost", 80, type=socket.SOCK_STREAM)
    assert any(info[4][0] in ("127.0.0.1", "::1") for info in infos), infos

    # run_in_executor handoff
    out = await loop.run_in_executor(None, lambda: "executor ran")
    assert out == "executor ran"

    # subprocess_exec echo (RFC 0072 WS3: binds the process surface)
    proc = await asyncio.create_subprocess_exec(
        sys.executable,
        "-c",
        "print('sub ok')",
        stdout=asyncio.subprocess.PIPE,
    )
    stdout, _ = await proc.communicate()
    assert proc.returncode == 0, proc.returncode
    assert stdout.strip() == b"sub ok", stdout


uvloop.run(main())
print("uvloop ok", uvloop.__version__)
