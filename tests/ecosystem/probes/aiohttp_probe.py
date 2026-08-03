"""Ecosystem probe: aiohttp — server + client request/response cycle over
asyncio (exercises the C-accelerated multidict/yarl/frozenlist chain)."""

import asyncio

import aiohttp
from aiohttp import web


async def handle_json(request):
    return web.json_response({"path": request.path, "q": request.query.get("q")})


async def handle_echo(request):
    body = await request.text()
    return web.Response(text=f"echo:{body}")


async def main():
    app = web.Application()
    app.router.add_get("/api", handle_json)
    app.router.add_post("/echo", handle_echo)

    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    port = runner.addresses[0][1]

    async with aiohttp.ClientSession() as session:
        async with session.get(f"http://127.0.0.1:{port}/api?q=42") as resp:
            assert resp.status == 200, resp.status
            data = await resp.json()
            assert data == {"path": "/api", "q": "42"}, data

        async with session.post(f"http://127.0.0.1:{port}/echo", data="ping") as resp:
            assert await resp.text() == "echo:ping"

        async with session.get(f"http://127.0.0.1:{port}/missing") as resp:
            assert resp.status == 404

    await runner.cleanup()


asyncio.run(main())
print("aiohttp ok", aiohttp.__version__)
