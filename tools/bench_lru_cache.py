"""Measure complete LRU workloads, including setup and public result checks."""
import functools
import os
import time

KIND = os.environ.get('WEAVEPY_LRU_KIND', 'cycle')
CAPACITY = int(os.environ.get('WEAVEPY_LRU_CAPACITY', '128'))
if KIND not in ('hot', 'cycle', 'churn', 'unbounded', 'uncached'):
    raise ValueError('unknown WEAVEPY_LRU_KIND: ' + KIND)
if CAPACITY < 1:
    raise ValueError('capacity must be positive')


def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    calls = 0

    def value(key):
        nonlocal calls
        calls += 1
        return key + 1

    maxsize = None if KIND == 'unbounded' else (0 if KIND == 'uncached' else CAPACITY)
    cached = functools.lru_cache(maxsize=maxsize)(value)
    for key in range(CAPACITY):
        assert cached(key) == key + 1
    total = 0
    if KIND == 'hot':
        for _ in range(n):
            total += cached(CAPACITY - 1)
        expected = n * CAPACITY
        expected_hits = n
    elif KIND == 'churn':
        for i in range(n):
            total += cached(CAPACITY + i)
        expected = n * (CAPACITY + 1) + n * (n - 1) // 2
        expected_hits = 0
    else:
        for i in range(n):
            total += cached(i % CAPACITY)
        whole, tail = divmod(n, CAPACITY)
        expected = whole * CAPACITY * (CAPACITY + 1) // 2 + tail * (tail + 1) // 2
        expected_hits = 0 if KIND == 'uncached' else n
    assert total == expected
    info = cached.cache_info()
    expected_misses = CAPACITY + n - expected_hits
    assert info.hits == expected_hits
    assert info.misses == calls == expected_misses
    assert info.maxsize == maxsize
    assert info.currsize == (0 if KIND == 'uncached' else CAPACITY)
    cached.cache_clear()
    info = cached.cache_info()
    assert info.hits == info.misses == info.currsize == 0
    assert info.maxsize == maxsize
    return total


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
