import asyncio


async def add(a, b):
    await asyncio.sleep(0)
    return a + b


async def main():
    results = await asyncio.gather(add(1, 2), add(3, 4), add(5, 6))
    print(results)
    print(await asyncio.wait_for(add(10, 20), timeout=1.0))


asyncio.run(main())
