"""Measure scalar and fallback LRU key paths with setup and checks timed."""
import functools
import os
import time

KIND = os.environ.get('WEAVEPY_LRU_KEY_KIND', 'integer')
CAPACITY = int(os.environ.get('WEAVEPY_LRU_CAPACITY', '128'))
if KIND not in ('integer', 'string', 'long', 'tuple', 'custom', 'typed', 'keyword', 'transition'):
    raise ValueError('unknown LRU key kind: ' + KIND)
if CAPACITY < 1:
    raise ValueError('capacity must be positive')
OFFSET = 10**30


class Key:
    def __init__(self, number):
        self.number = number

    def __hash__(self):
        return self.number

    def __eq__(self, other):
        return isinstance(other, Key) and self.number == other.number


def make_key(number):
    if KIND == 'string':
        return str(number)
    if KIND == 'long':
        return OFFSET + number
    if KIND == 'tuple':
        return (number,)
    if KIND == 'custom':
        return Key(number)
    return number


def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    calls = 0

    @functools.lru_cache(maxsize=CAPACITY, typed=KIND == 'typed')
    def cached(key):
        nonlocal calls
        calls += 1
        if KIND == 'string':
            return int(key)
        if KIND == 'long':
            return key - OFFSET
        if KIND == 'custom':
            return key.number
        if isinstance(key, tuple):
            return key[0]
        return key

    if KIND == 'transition':
        cached(0)
        cached((0,))
        cached.cache_clear()
    keys = [make_key(i) for i in range(CAPACITY)]
    lookups = [make_key(i) for i in range(CAPACITY)]
    for i in range(CAPACITY):
        value = cached(key=keys[i]) if KIND == 'keyword' else cached(keys[i])
        assert value == i
    before = cached.cache_info()
    before_calls = calls
    total = 0
    if KIND == 'keyword':
        for i in range(n):
            total += cached(key=lookups[i % CAPACITY])
    else:
        for i in range(n):
            total += cached(lookups[i % CAPACITY])
    whole, tail = divmod(n, CAPACITY)
    assert total == whole * CAPACITY * (CAPACITY - 1) // 2 + tail * (tail - 1) // 2
    after = cached.cache_info()
    assert after.hits - before.hits == n
    assert after.misses == before.misses and calls == before_calls
    assert after.currsize == after.maxsize == CAPACITY
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, CAPACITY, 0)
    return total


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '10000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
