import functools


def add(x, y):
    return x + y


print(functools.reduce(add, [1, 2, 3, 4], 0))


inc = functools.partial(add, 1)
print(inc(10))


@functools.lru_cache(maxsize=None)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


print(fib(10))
