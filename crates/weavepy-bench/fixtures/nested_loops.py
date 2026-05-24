"""Three-level nested loop — measures nested FOR_ITER + BINARY_OP."""

import os


def bench(n):
    total = 0
    for i in range(n):
        for j in range(n):
            for k in range(n):
                total = total + i + j + k
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "20"))
    bench(n)
