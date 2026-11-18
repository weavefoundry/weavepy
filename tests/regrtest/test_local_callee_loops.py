"""Loops that call a Python function held in a local.

Tier 2 calls such a callee through its generic call helper; a loop doing
little else runs in the frame JIT, which checks and enters the callee
directly. Results, a callee that changes partway, a bound method, keyword
arguments, a callee that raises, and a loop heavy with arithmetic (which
tier 2 keeps) all behave as before. Run it past the compile threshold.
"""


def f3(a, b, c):
    return a + b * c


def g3(a, b, c):
    return a - b - c


class Acc:
    def __init__(self):
        self.n = 0

    def add(self, k, *, by=1):
        self.n += k * by
        return self.n


def calls(n, f):
    total = 0
    for i in range(n):
        total += f(i, 2, 3)
    return total


def swaps(n):
    total = 0
    f = f3
    for i in range(n):
        if i == n // 2:
            f = g3
        total += f(i, 1, 1)
    return total


def bound(n):
    acc = Acc()
    add = acc.add
    for i in range(n):
        add(i, by=2)
    return acc.n


def raises(n):
    def boom(i):
        if i == n - 1:
            raise ValueError(i)
        return i

    caught = 0
    for i in range(n):
        try:
            boom(i)
        except ValueError as e:
            caught = e.args[0]
    return caught


def heavy(n, f):
    total = 0.0
    for i in range(n):
        x = i * 0.5
        y = x * x + x * 3.0 - i / 7 + (i % 5) * 2.0 - (i % 3) * 1.5
        z = y * y - x * y + 0.25 * x - y / 9 + x * 0.125 - (y - x) * 0.5
        total += z + y + x + f(i, 1, 1) * 1.0
    return total


n = 4000
assert calls(n, f3) == sum(i + 6 for i in range(n))
assert swaps(n) == sum(i + 1 for i in range(n // 2)) + sum(i - 2 for i in range(n // 2, n))
assert bound(n) == 2 * sum(range(n))
assert raises(n) == n - 1


def heavy_expected(n):
    total = 0.0
    for i in range(n):
        x = i * 0.5
        y = x * x + x * 3.0 - i / 7 + (i % 5) * 2.0 - (i % 3) * 1.5
        z = y * y - x * y + 0.25 * x - y / 9 + x * 0.125 - (y - x) * 0.5
        total += z + y + x + (i + 1) * 1.0
    return total


assert heavy(n, f3) == heavy_expected(n)
print("ok")
