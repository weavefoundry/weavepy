# `async with` — async context managers with `__aenter__` /
# `__aexit__`.


class AsyncCtx:
    def __init__(self, name):
        self.name = name

    async def __aenter__(self):
        print(f"enter {self.name}")
        return self

    async def __aexit__(self, et, ev, tb):
        print(f"exit {self.name}")
        return False


async def basic():
    async with AsyncCtx("a"):
        print("inside a")


async def nested():
    async with AsyncCtx("outer"):
        async with AsyncCtx("inner"):
            print("inside both")


async def multi():
    # `async with cm1, cm2:` is just sugar for nested `async with`s.
    async with AsyncCtx("x"), AsyncCtx("y"):
        print("inside x and y")


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


drive(basic())
drive(nested())
drive(multi())
