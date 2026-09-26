"""Numeric work after constructing configuration with keyword arguments."""

import os
import time


class Options:
    def __init__(self, seed=0):
        self.seed = seed


def bench(n):
    options = Options(seed=3)
    seed = options.seed
    total = 0
    for i in range(n):
        total += (i & 255) + seed
    cycles, tail = divmod(n, 256)
    assert total == 3 * n + cycles * 32640 + tail * (tail - 1) // 2
    return total


if __name__ == '__main__':
    n = int(os.environ.get('WEAVEPY_BENCH_WORK', '1000000'))
    start = time.perf_counter_ns()
    bench(n)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
