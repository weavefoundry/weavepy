"""Method/attribute kernels in the tier-2 JITable subset — the RFC
0065 WS5 lanes.

`append_kernel` grows a fresh int list through `list.append` and reads
it back with `len`; `attr_kernel` runs a scalar load/store loop on a
pinned instance (`p.x = p.x + p.y`). Both are while-loop bodies with
stable shapes, so with `WEAVEPY_JIT=1` they tier up and run through
`wpjit_list_append`/`wpjit_list_len` and the burned-in attribute
sites; without it they measure the tier-1 inline caches on the same
shapes, which is the comparison the `--jit` column reports.
"""

import os


class Point:
    def __init__(self):
        self.x = 0
        self.y = 1


def append_kernel(xs, n):
    i = 0
    while i < n:
        xs.append(i * 2)
        i = i + 1
    return len(xs)


def len_kernel(xs, n):
    s = 0
    i = 0
    while i < n:
        s = s + len(xs)
        i = i + 1
    return s


def attr_kernel(p, n):
    i = 0
    while i < n:
        p.x = p.x + p.y + i
        i = i + 1
    return p.x


def bench(n):
    total = 0
    k = 0
    while k < n:
        xs = []
        total = total + append_kernel(xs, 200)
        total = total + len_kernel(xs, 200)
        p = Point()
        total = total + attr_kernel(p, 200)
        k = k + 1
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "2000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
