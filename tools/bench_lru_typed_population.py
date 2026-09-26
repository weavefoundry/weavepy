"""Measure construction, use, and collection of many small typed LRU caches."""
import functools
import gc
import os
import time

KIND = os.environ.get('WEAVEPY_LRU_KIND', 'scalar')
if KIND not in ('empty', 'scalar', 'tuple', 'fallback'):
    raise ValueError('unknown typed LRU population kind: ' + KIND)


def value(key):
    return key


def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    wrappers = [functools.lru_cache(maxsize=4, typed=True)(value) for _ in range(n)]
    key = (1,) if KIND == 'tuple' else 1.0 if KIND == 'fallback' else 1
    for cached in wrappers:
        if KIND == 'empty':
            assert cached.cache_info() == (0, 0, 4, 0)
        else:
            assert cached(key) == key
            assert cached(key) == key
            assert cached.cache_info() == (1, 1, 4, 1)
    if n:
        del cached
    wrappers.clear()
    gc.collect()
    return n


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '1000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
