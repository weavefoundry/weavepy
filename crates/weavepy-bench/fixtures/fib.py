"""Naive recursive fib — pure call-overhead benchmark."""

import os


def _fib(n):
    if n < 2:
        return n
    return _fib(n - 1) + _fib(n - 2)


def bench(n):
    return _fib(n)


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "20"))
    bench(n)
