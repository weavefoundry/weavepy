async def agen():
    yield 1
    yield 2
    yield 3


async def collect():
    out = []
    async for v in agen():
        out.append(v)
    return out


c = collect()
try:
    while True:
        c.send(None)
except StopIteration as e:
    print(e.value)
