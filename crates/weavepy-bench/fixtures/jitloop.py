"""While-loop numeric kernel called many times — the fixture the
RFC 0032 tier-2 JIT targets most directly.

`kernel` is a pure integer hot loop (no FOR_ITER, no calls in the
loop body) so it lands in the JITable subset; `bench` calls it `n`
times so the per-`CodeObject` hot counter crosses the tier-up
threshold and the kernel runs as native code for the bulk of the
work. With `WEAVEPY_JIT=0` it measures the interpreter on the same
shape, which is the comparison we care about.
"""

import os


def kernel(n):
    s = 0
    i = 0
    while i < n:
        s = s + i * 2 - (i // 3) + (i % 7)
        i = i + 1
    return s


def bench(n):
    total = 0
    k = 0
    while k < n:
        total = total + kernel(n)
        k = k + 1
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "300"))
    bench(n)
