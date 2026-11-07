"""`async with` over coroutine `__aenter__`/`__aexit__` methods (which
compiled code now runs in line) keeps CPython's results: the entered value,
exceptions raised inside the body reaching `__aexit__` with their details,
a true `__aexit__` result suppressing them, an exception from `__aenter__`
or `__aexit__` propagating, the order of calls, an `__aenter__` that isn't
a coroutine function, and a non-awaitable result's error message."""
import asyncio


class Recorder:
    def __init__(self, log, suppress=False):
        self.log = log
        self.suppress = suppress

    async def __aenter__(self):
        self.log.append("enter")
        await asyncio.sleep(0)
        return self

    async def __aexit__(self, et, ev, tb):
        self.log.append(("exit", et.__name__ if et else None, str(ev) if ev else None))
        return self.suppress


class FailEnter:
    async def __aenter__(self):
        raise KeyError("enter")

    async def __aexit__(self, *a):
        raise AssertionError("not reached")


class FailExit:
    async def __aenter__(self):
        return 1

    async def __aexit__(self, *a):
        raise ValueError("exit")


class PlainEnter:
    def __aenter__(self):
        return asyncio.sleep(0, "plain")

    async def __aexit__(self, *a):
        return False


class BadEnter:
    def __aenter__(self):
        return 42

    async def __aexit__(self, *a):
        return False


async def run(n):
    log = []
    total = 0
    lock = asyncio.Lock()
    for i in range(n):
        async with lock:
            total += 1
        async with Recorder(log) as r:
            total += r is not None
    assert lock.locked() is False
    assert log.count("enter") == n
    assert log[-1] == ("exit", None, None)
    log.clear()
    async with Recorder(log, suppress=True):
        raise ZeroDivisionError("boom")
    assert log == ["enter", ("exit", "ZeroDivisionError", "boom")], log
    log.clear()
    try:
        async with Recorder(log):
            raise IndexError("kept")
    except IndexError as e:
        assert str(e) == "kept"
    else:
        raise AssertionError("not suppressed")
    try:
        async with FailEnter():
            raise AssertionError("body ran")
    except KeyError:
        pass
    try:
        async with FailExit():
            pass
    except ValueError as e:
        assert str(e) == "exit"
    async with PlainEnter() as v:
        assert v == "plain", v
    try:
        async with BadEnter():
            pass
    except TypeError as e:
        assert "async with" in str(e), str(e)
    else:
        raise AssertionError("no TypeError")
    return total


assert asyncio.run(run(3000)) == 6000
print("ok")
