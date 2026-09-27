"""Evicted scalar-cache values finalize after consistent state is published."""
import functools
import sys
import weakref

seen = []


class Value:
    def __init__(self, number):
        self.number = number

    def __del__(self):
        caller = sys._getframe(1)
        seen.append((self.number, caller.f_code.co_name, cached.cache_info().currsize))


@functools.lru_cache(1)
def cached(key):
    return Value(key)


witness = weakref.ref(cached(1))


def evict():
    cached(2)
    assert witness() is None
    assert seen == [(1, 'evict', 1)], seen


def clear():
    cached.cache_clear()
    assert seen == [(1, 'evict', 1), (2, 'clear', 0)], seen
    assert cached.cache_info() == (0, 0, 1, 0)


evict()
clear()
print('scalar LRU eviction and clear owners: ok')
