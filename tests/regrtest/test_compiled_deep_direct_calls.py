"""Deep recursion through compiled functions' direct calls (a recursive
`lru_cache` function's misses, a plain recursive function), on the main
thread and on a worker thread, keeps CPython's results: the values, the
`RecursionError` past the limit, and the stack unwinding back."""
import functools
import sys
import threading


@functools.lru_cache(maxsize=256)
def collatz_len(n):
    if n == 1:
        return 1
    return 1 + collatz_len(n // 2 if n % 2 == 0 else 3 * n + 1)


def depth(n):
    if n == 0:
        return 0
    return 1 + depth(n - 1)


def run():
    for i in range(1, 3000):
        collatz_len.cache_clear() if i % 500 == 0 else None
        assert collatz_len(i) >= 1
    assert collatz_len(27) == 112
    for k in range(200):
        assert depth(k * 3) == k * 3
    try:
        depth(100000)
    except RecursionError:
        pass
    else:
        raise AssertionError("no RecursionError")
    assert depth(500) == 500


run()
errors = []


def worker():
    try:
        run()
    except BaseException as e:  # (reported below)
        errors.append(e)


t = threading.Thread(target=worker)
t.start()
t.join()
assert not errors, errors
print("ok")
