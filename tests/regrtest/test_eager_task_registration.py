"""Eager tasks started from compiled code: a lazily running activation's
native body (`loop.create_task`, through the eager task factory) runs the
coroutine's first step, whose own compiled code makes direct calls and
inline calls before the outer activation registers. The outer one's
pending entries belong below the newer ones' (an entry left above a
returned activation's was read after its stack frame was gone), and every
frame a stack walk finds is a live one."""
import asyncio
import sys


def isfuture(obj):
    # (Runs to the end in compiled code, as `type(obj)` lets it: the shape
    # that kept the outer activation unregistered.)
    return hasattr(type(obj), "_asyncio_future_blocking") and obj._asyncio_future_blocking is not None


asyncio.base_futures.isfuture.__code__ = isfuture.__code__

walk = False
stacks = []


async def recursive_gather(width, depth):
    if depth == 0:
        # (One stack walk, in the last run: a walk materializes every
        # activation, which registers the outer one early.)
        if walk and not stacks:
            names = []
            f = sys._getframe()
            while f is not None:
                names.append(f.f_code.co_name)
                f = f.f_back
            stacks.append(names)
        return
    await asyncio.gather(*[recursive_gather(width, depth - 1) for _ in range(width)])


async def awaitable_chain(depth):
    if depth == 0:
        return 0
    return 1 + await awaitable_chain(depth - 1)


def run(task_class):
    loop = asyncio.new_event_loop()
    created = [0]

    def counting_task(*args, eager_start=False, **kwargs):
        if not eager_start:
            created[0] += 1
        kwargs["eager_start"] = eager_start
        return task_class(*args, **kwargs)

    loop.set_task_factory(asyncio.create_eager_task_factory(counting_task))
    try:
        assert loop.run_until_complete(awaitable_chain(100)) == 100
        loop.run_until_complete(recursive_gather(5, 4))
    finally:
        loop.close()
    return created[0]


# Past the frame JIT's threshold, so later rounds run compiled code.
for _ in range(2):
    for cls in (asyncio.tasks._CTask, asyncio.tasks._PyTask, asyncio.tasks._CTask):
        # Eager tasks all finish in their first step: none is scheduled.
        assert run(cls) == 0
walk = True
assert run(asyncio.tasks._CTask) == 0
# The eager steps nest: each level's gather starts the next level's task.
[names] = stacks
level = ["recursive_gather", "counting_task", "factory", "create_task", "ensure_future", "gather"]
assert names[: 4 * len(level) + 1] == level * 4 + ["recursive_gather"], names
assert names[-1] == "<module>", names
print("ok")
