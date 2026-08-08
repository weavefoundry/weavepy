"""Call-shape matrix: positional, defaults, kwargs, bound methods,
builtins — the pure function-call overhead benchmark."""

import os


def pos2(a, b):
    return a + b


def with_defaults(a, b=10, c=20):
    return a + b + c


def with_kwargs(a, **kw):
    return a + kw.get("delta", 0)


class Counter:
    def __init__(self):
        self.n = 0

    def bump(self, by):
        self.n += by
        return self.n


def bench(n):
    c = Counter()
    bump = c.bump
    total = 0
    seq = (1, 2, 3)
    for i in range(n):
        total += pos2(i, 1)
        total += with_defaults(i)
        total += with_defaults(i, c=5)
        total += with_kwargs(i, delta=2)
        total += c.bump(1)
        total += bump(1)
        total += len(seq)
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "20000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
