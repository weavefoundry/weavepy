"""Bignum-arithmetic stress test — keeps multiplying ints past
the i64 boundary so the BinOp specializations need to deopt to
the BigInt slow path. Loosely modeled after the spigot for
digits of pi but trimmed to the simplest shape that exercises
overflow promotion without a full pi spigot."""

import os


def _bignum_loop(n):
    a = 1
    b = 1
    for _ in range(n):
        a, b = b, a + b
    return b


def bench(n):
    return _bignum_loop(n)


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "100"))
    bench(n)
