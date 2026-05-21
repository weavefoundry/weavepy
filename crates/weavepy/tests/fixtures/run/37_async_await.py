# `async def` + `await` — the heart of PEP 492 in WeavePy.


async def succ(x):
    return x + 1


async def pipeline(n):
    total = 0
    for i in range(n):
        total = await succ(total)
    return total


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


print(type(succ(0)).__name__)
print(drive(pipeline(5)))


# Cross-call: an `await` whose operand calls another `async def`,
# whose body in turn `await`s. Three frames stacked.


async def inner(x):
    return await succ(x)


async def outer(x):
    a = await inner(x)
    b = await inner(a)
    return b


print(drive(outer(10)))


# An async function can also contain plain branching / loops.


async def maybe(positive):
    if positive:
        return await succ(0)
    return -1


print(drive(maybe(True)))
print(drive(maybe(False)))
