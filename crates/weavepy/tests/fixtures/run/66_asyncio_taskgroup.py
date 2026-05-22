import asyncio


async def worker(name, delay, value):
    await asyncio.sleep(delay)
    return (name, value)


async def main():
    results = []
    async with asyncio.TaskGroup() as tg:
        t1 = tg.create_task(worker("a", 0, 1))
        t2 = tg.create_task(worker("b", 0, 2))
        t3 = tg.create_task(worker("c", 0, 3))
    results = [t1.result(), t2.result(), t3.result()]
    return results


out = asyncio.run(main())
out.sort()
for item in out:
    print(item)


async def main_eg():
    try:
        async with asyncio.TaskGroup() as tg:
            tg.create_task(asyncio.sleep(0))

            async def boom():
                raise ValueError("nope")

            tg.create_task(boom())
        return "no exc"
    except* ValueError as eg:
        return f"caught {len(eg.exceptions)} value error(s): {eg.exceptions[0].args[0]}"


print(asyncio.run(main_eg()))
