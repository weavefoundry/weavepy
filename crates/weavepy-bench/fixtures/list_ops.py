"""List subscripts, append/pop, slices, sort, and comprehensions."""

import os


def bench(n):
    data = list(range(256))
    total = 0
    for i in range(n):
        acc = []
        for j in range(64):
            acc.append(data[(i + j) & 255])
        total += acc[0] + acc[-1] + acc[31]
        acc[5] = acc[5] + 1
        sl = acc[8:24]
        total += len(sl)
        squares = [x * x for x in sl]
        evens = [x for x in squares if x & 1 == 0]
        total += sum(evens)
        acc.sort()
        total += acc.pop()
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "5000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
