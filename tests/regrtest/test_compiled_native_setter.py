"""A store through a native property's setter whose fast half runs no
Python code (asyncio's `Task._log_destroy_pending`), made from compiled
code, keeps CPython's results: the value reads back, a value whose truth
runs Python code still takes the full setter, and the task finishes as
before."""
import asyncio


class Truthy:
    calls = 0

    def __bool__(self):
        Truthy.calls += 1
        return False


async def leaf(i):
    return i


def mark(task, value):
    task._log_destroy_pending = value
    return task


async def main(n):
    loop = asyncio.get_running_loop()
    total = 0
    for i in range(n):
        t = mark(loop.create_task(leaf(i)), False)
        assert t._log_destroy_pending is False
        total += await t
        mark(t, True)
        assert t._log_destroy_pending is True
    t = mark(loop.create_task(leaf(0)), Truthy())
    assert Truthy.calls == 1 and t._log_destroy_pending is False
    await t
    return total


assert asyncio.run(main(3000)) == sum(range(3000))
print("ok")
