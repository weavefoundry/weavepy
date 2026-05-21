async def square(x):
    return x * x


async def pipeline():
    return [await square(i) for i in range(5)]


c = pipeline()
try:
    while True:
        c.send(None)
except StopIteration as e:
    print(e.value)
