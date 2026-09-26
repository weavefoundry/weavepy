"""Compare plain, nested, and sent-value generator resumes.

Each call constructs its generators inside the timed workload. Set
WEAVEPY_GENERATOR_RESUME_KIND to for, next, sum, pipeline, or send.
"""
import os
import time

KIND = os.environ.get("WEAVEPY_GENERATOR_RESUME_KIND", "pipeline")


def numbers(n):
    for value in range(n):
        yield value


def increment(values):
    for value in values:
        yield value + 1


def accumulator():
    total = 0
    while True:
        value = yield total
        if value is None:
            return
        total += value


def bench(n):
    total = 0
    if KIND == "send":
        values = accumulator()
        assert next(values) == 0
        for _ in range(n):
            total += values.send(1)
        values.close()
        expected = n * (n + 1) // 2
    else:
        values = numbers(n)
        if KIND == "pipeline":
            for _ in range(4):
                values = increment(values)
        if KIND == "next":
            for _ in range(n):
                total += next(values)
        elif KIND == "sum":
            total = sum(values)
        elif KIND in ("for", "pipeline"):
            for value in values:
                total += value
        else:
            raise ValueError("unknown generator resume kind: " + KIND)
        values.close()
        expected = n * (n - 1) // 2 + (4 * n if KIND == "pipeline" else 0)
    assert total == expected, (total, expected)
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "300000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
