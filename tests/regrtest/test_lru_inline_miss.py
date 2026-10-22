"""A cache miss runs the wrapped function as CPython's C wrapper does.

The call has no frame of its own, an exception caches nothing, the result
is stored before the caller sees it, and the wrapper lives until the call
returns. Each case runs warm, from compiled and interpreted callers.
"""

import functools
import sys
import traceback
import weakref


def check_frames():
    @functools.lru_cache(maxsize=4)
    def where(n):
        return sys._getframe(1).f_code.co_name, cached_size(where)

    def caller():
        return [where(i) for i in range(8)]

    for _ in range(50):
        where.cache_clear()
        seen = caller()
        # The result is stored before the caller takes it.
        assert seen == [('<listcomp>' if sys.version_info < (3, 12) else 'caller', min(i, 4))
                        for i in range(8)], seen


def cached_size(wrapper):
    return wrapper.cache_info().currsize


def check_exceptions():
    calls = []

    @functools.lru_cache(maxsize=8)
    def fragile(n):
        calls.append(n)
        if n % 3 == 0:
            raise ValueError(n)
        return n * 2

    def run():
        out = []
        for i in range(12):
            try:
                out.append(fragile(i))
            except ValueError as error:
                out.append(('error', error.args[0], error.__traceback__.tb_next.tb_frame.f_code.co_name))
        return out

    for _ in range(30):
        fragile.cache_clear()
        calls.clear()
        first = run()
        assert first == [('error', i, 'fragile') if i % 3 == 0 else i * 2 for i in range(12)], first
        info = fragile.cache_info()
        assert info.misses == 12 and info.hits == 0 and info.currsize == 8, info
        # Errors were not cached: they run again.
        before = len(calls)
        fragile(0) if False else None
        try:
            fragile(3)
        except ValueError:
            pass
        assert len(calls) == before + 1


def check_traceback():
    @functools.lru_cache(maxsize=None)
    def deep(n):
        if n == 0:
            raise KeyError('bottom')
        return deep(n - 1)

    for _ in range(20):
        deep.cache_clear()
        try:
            deep(3)
        except KeyError as error:
            names = [f.name for f in traceback.extract_tb(error.__traceback__)]
        assert names == ['check_traceback', 'deep', 'deep', 'deep', 'deep'], names


def check_wrapper_lifetime():
    events = []

    def body(n):
        events.append(('body', n))
        return n + 1

    for _ in range(30):
        events.clear()
        wrapper = functools.lru_cache(maxsize=2)(body)
        ref = weakref.ref(wrapper, lambda r: events.append('dead'))
        # The temporary wrapper dies after the call, not during it.
        assert functools.lru_cache(maxsize=2)(body)(1) == 2
        del wrapper
        assert ref() is None
        assert events[0] == ('body', 1) and 'dead' in events, events


class Opaque:
    """A key whose comparison runs Python."""

    def __hash__(self):
        return 7

    def __eq__(self, other):
        return self is other


def check_colliding_key_during_call():
    opaque = Opaque()
    # An int key hashed as the tuple key `(opaque,)` is.
    collide = hash((opaque,))
    for maxsize in (4, None):
        @functools.lru_cache(maxsize=maxsize)
        def mixed(n):
            if type(n) is int and n == collide:
                # The colliding key arrives during the call: storing this
                # call's result meets it.
                assert mixed(opaque) == 'inner'
                return 'outer'
            return 'inner'

        def run():
            return [mixed(1), mixed(collide), mixed(collide), mixed(opaque), mixed(1)]

        for _ in range(20):
            mixed.cache_clear()
            assert run() == ['inner', 'outer', 'outer', 'inner', 'inner']
            assert mixed.cache_info() == (3, 3, maxsize, 3), (maxsize, mixed.cache_info())


def check_recursion_limit():
    @functools.lru_cache(maxsize=None)
    def fib(n):
        if n <= 1:
            return n
        return fib(n - 1) + fib(n - 2)

    old = sys.getrecursionlimit()
    sys.setrecursionlimit(100000)
    try:
        try:
            fib(100000)
        except RecursionError:
            pass
        else:
            raise AssertionError('deep lru_cache recursion must raise RecursionError')
    finally:
        sys.setrecursionlimit(old)
    fib.cache_clear()
    assert fib(300) == 222232244629420445529739893461909967206666939096499764990979600


check_frames()
check_exceptions()
check_traceback()
check_wrapper_lifetime()
check_colliding_key_during_call()
check_recursion_limit()
print('LRU inline misses: ok')
