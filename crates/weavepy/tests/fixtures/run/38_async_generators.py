# Async generators (PEP 525): `async def` + `yield` cooperatively
# producing values via `async for`.


async def squares(n):
    for i in range(n):
        yield i * i


async def collect():
    out = []
    async for v in squares(5):
        out.append(v)
    return out


async def filtered():
    return [x async for x in squares(6) if x % 2 == 0]


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


print(drive(collect()))
print(drive(filtered()))


# `async for ... else`: the else runs when the generator exhausts
# without a `break`.


async def trace():
    out = []
    async for v in squares(3):
        out.append(v)
    else:
        out.append("done")
    return out


print(drive(trace()))
