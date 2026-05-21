async def add(a, b):
    return a + b


async def chain():
    x = await add(1, 2)
    y = await add(x, 3)
    return y


c = chain()
print(type(c).__name__)
try:
    c.send(None)
except StopIteration as e:
    print(e.value)
