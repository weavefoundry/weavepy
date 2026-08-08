"""Interpreter startup — measured as full subprocess wall time by the
harness (see WALL_CLOCK_FIXTURES); the body is intentionally trivial."""

import os

def bench(n):
    return n


if __name__ == "__main__":
    bench(int(os.environ.get("WEAVEPY_BENCH_WORK", "1")))
