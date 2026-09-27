"""Measure unordered set removal with construction and result checks timed."""
import gc
import os
import time
import weakref

KIND = os.environ.get('WEAVEPY_SET_MUTATION_KIND', 'pop')
if KIND not in ('pop', 'remove', 'discard', 'difference', 'mixed', 'weak-set'):
    raise ValueError('unknown WEAVEPY_SET_MUTATION_KIND: ' + KIND)


class Node:
    __slots__ = ('value', '__weakref__')

    def __init__(self, value):
        self.value = value


def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    if KIND == 'weak-set':
        owners = [Node(i) for i in range(n)]
        values = weakref.WeakSet(owners)
        assert len(values) == n
        if n:
            assert owners[0] in values and owners[-1] in values
        del owners
        gc.collect()
        assert not values
        return n
    values = set(range(n))
    if KIND == 'pop':
        total = 0
        for _ in range(n):
            value = values.pop()
            assert value not in values
            total += value
        assert total == n * (n - 1) // 2
        assert not values
    elif KIND == 'remove':
        for i in range(n):
            assert values.remove(i) is None
        assert not values
    elif KIND == 'discard':
        for i in range(n):
            assert values.discard(i) is None
        assert values.discard(-1) is None
        assert not values
    elif KIND == 'difference':
        assert values.difference_update(range(0, n, 2)) is None
        assert values == set(range(1, n, 2))
    else:
        for i in range(n):
            assert values.remove(i) is None
            assert values.add(n + i) is None
        assert values == set(range(n, 2 * n))
    return n


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '10000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
