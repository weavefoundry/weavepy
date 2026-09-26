"""Measure native deque operations and Python subscript fallback costs.

WEAVEPY_NATIVE_CONTAINER_KIND selects reads, queue, stack, or python_reads.
All cases check their results and include container construction in the timer.
"""
import os
import time
from collections import deque

KIND = os.environ.get("WEAVEPY_NATIVE_CONTAINER_KIND", "reads")


class PythonContainer:
    def __getitem__(self, index):
        return 3 if index == 0 else 7


def read(values, n):
    total = 0
    for _ in range(n):
        total += values[0] + values[-1]
    return total


def queue(n):
    values = deque()
    total = 0
    for i in range(n):
        values.append(i)
        total += values.popleft()
    return total


def stack(n):
    values = deque()
    total = 0
    for i in range(n):
        values.appendleft(i)
        total += values.pop()
    return total


def bench(n):
    if KIND == "reads":
        result = read(deque([3, 7]), n)
        expected = n * 10
    elif KIND == "python_reads":
        result = read(PythonContainer(), n)
        expected = n * 10
    elif KIND == "queue":
        result = queue(n)
        expected = n * (n - 1) // 2
    elif KIND == "stack":
        result = stack(n)
        expected = n * (n - 1) // 2
    else:
        raise ValueError("unknown native container kind: " + KIND)
    assert result == expected, (result, expected)
    return result


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "200000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
