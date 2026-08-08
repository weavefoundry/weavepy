"""Generator pipeline — creation, resumption, and close overhead."""

import os


def _naturals(limit):
    i = 0
    while i < limit:
        yield i
        i += 1


def _squared(it):
    for x in it:
        yield x * x


def _odds_only(it):
    for x in it:
        if x & 1:
            yield x


def bench(n):
    total = 0
    total += sum(_odds_only(_squared(_naturals(n))))
    total += sum(x + 1 for x in _naturals(n))
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "50000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
