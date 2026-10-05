"""Closures, decorators with functools.wraps, partial, lambdas, keyword
arguments, *args/**kwargs forwarding, and lru_cache."""

import functools

WORK = 25000


def counted(fn):
    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        wrapper.calls += 1
        return fn(*args, **kwargs)

    wrapper.calls = 0
    return wrapper


@counted
def area(w, h=1, *, scale=1.0):
    return w * h * scale


def make_adder(k):
    def add(x):
        return x + k
    return add


@functools.lru_cache(maxsize=256)
def collatz_len(n):
    if n == 1:
        return 1
    return 1 + collatz_len(n // 2 if n % 2 == 0 else 3 * n + 1)


def apply_all(fns, x):
    for f in fns:
        x = f(x)
    return x


def bench(n):
    total = 0.0
    calls0 = area.calls
    double = functools.partial(lambda a, b: a * b, 2)
    for i in range(n):
        total += area(i, h=2, scale=0.5)
        total += area(3)
        add = make_adder(i)
        total += apply_all((add, double, lambda v: v - 1), i)
        total += collatz_len(1 + i % 500)
        total += sorted((i % 7, i % 3, i % 5), key=lambda v: -v)[0]
    return (total, area.calls - calls0)
