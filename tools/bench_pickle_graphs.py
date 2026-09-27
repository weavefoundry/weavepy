#!/usr/bin/env python3
"""Measure pickle encoding of text, shared children, and deep containers.

Set WEAVEPY_PICKLE_SHAPE to text (the default), aliases, or deep.
WEAVEPY_BENCH_WORK controls the number of protocol-5 encodings.
"""

import os
import pickle
import time


def make_payload(shape):
    if shape == "text":
        return ["record-%d" % i for i in range(1000)]
    if shape == "aliases":
        child = {"items": list(range(32)), "name": "shared child"}
        return [child] * 1000
    if shape == "deep":
        value = ["bottom"]
        for _ in range(100):
            value = [value]
        return value
    raise ValueError("unknown pickle shape: %s" % shape)


payload = make_payload(os.environ.get("WEAVEPY_PICKLE_SHAPE", "text"))
expected = pickle.dumps(payload, protocol=5)


def bench(n):
    total = 0
    blob = expected
    for _ in range(n):
        blob = pickle.dumps(payload, protocol=5)
        total += len(blob)
    assert total == len(expected) * n
    assert blob == expected
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "2000"))
    start = time.perf_counter_ns()
    bench(n)
    elapsed = time.perf_counter_ns() - start
    print("WEAVEPY_BENCH_NS=%d" % elapsed)
