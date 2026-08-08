"""Dict insert / lookup / delete / iterate with str and int keys."""

import os


def bench(n):
    d = {}
    total = 0
    for i in range(n):
        d[i & 1023] = i
        d["k%d" % (i & 255)] = i
    for i in range(n):
        total += d[i & 1023]
        v = d.get("k%d" % (i & 255))
        if v is not None:
            total += v
        if ((i * 7) & 1023) in d:
            total += 1
    for k in list(d):
        if isinstance(k, int) and k & 1:
            del d[k]
    for k, v in d.items():
        total += v if isinstance(k, int) else 1
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "20000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
