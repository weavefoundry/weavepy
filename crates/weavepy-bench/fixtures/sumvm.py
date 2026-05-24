"""Pure dispatch-loop benchmark — a tight `total += i` loop that
exercises the hot path the BINARY_OP / FOR_ITER specializations
target most directly."""

import os


def bench(n):
    total = 0
    for i in range(n):
        total = total + i
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10000"))
    bench(n)
