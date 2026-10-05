"""Subscripts, unary operators, closure lists, and computed range bounds
in hot loops: the generic lanes and their guards, against the
interpreter's own results and errors.
"""

import sys
import traceback


def tuple_reads(pairs):
    total = 0.0
    best = None
    for i in range(len(pairs)):
        p = pairs[i]
        total += p[0] * p[1]
        if best is None or p[0] > best[0]:
            best = p
    return total, best


pairs = [(float(i % 11), float(i % 7)) for i in range(3000)]
assert tuple_reads(pairs) == (sum(a * b for a, b in pairs), max(pairs, key=lambda p: (p[0], -pairs.index(p))))


class Grid:
    def __init__(self):
        self.cells = {}

    def __getitem__(self, key):
        return self.cells.get(key, 0)

    def __setitem__(self, key, value):
        if value < 0:
            raise ValueError("negative cell")
        self.cells[key] = value


class Counter(dict):
    def __missing__(self, key):
        return 0


def tuple_keys(n, grid, counts):
    d = {}
    for i in range(n):
        key = (i % 13, i % 17)
        d[key] = d.get(key, 0) + 1
        grid[key] = grid[key] + 1
        counts[i % 5] = counts[i % 5] + i
    return d, grid.cells, dict(counts)


d, cells, counts = tuple_keys(4000, Grid(), Counter())
expect = {}
for i in range(4000):
    k = (i % 13, i % 17)
    expect[k] = expect.get(k, 0) + 1
assert d == expect and cells == expect
assert counts == {r: sum(i for i in range(4000) if i % 5 == r) for r in range(5)}


def negative_store(n, grid):
    for i in range(n):
        grid[(i, 0)] = n - 10 - i  # raises once n - 10 - i < 0


try:
    negative_store(3000, Grid())
except ValueError as exc:
    assert str(exc) == "negative cell"
    assert traceback.extract_tb(sys.exc_info()[2])[-2].line == \
        "grid[(i, 0)] = n - 10 - i  # raises once n - 10 - i < 0"
else:
    raise AssertionError("expected ValueError")


# A list index that comes in as an object: guarded as an exact int.
def indexed(xs, idxs):
    total = 0.0
    for k in idxs:
        total += xs[k]
    return total


xs = [float(i) for i in range(100)]
idxs = [(i * 7) % 100 for i in range(3000)]
assert indexed(xs, idxs) == sum(xs[k] for k in idxs)
idxs[2500] = True  # a bool indexes as 1
assert indexed(xs, idxs) == sum(xs[k] for k in idxs)
idxs[2600] = 1.5
try:
    indexed(xs, idxs)
except TypeError as exc:
    assert "list indices must be integers or slices, not float" in str(exc), exc
else:
    raise AssertionError("expected TypeError")


class Money:
    def __init__(self, cents):
        self.cents = cents

    def __neg__(self):
        return Money(-self.cents)

    def __pos__(self):
        return Money(abs(self.cents))


def negate_all(ms):
    total = 0
    for m in ms:
        total += (-m).cents + (+m).cents
    return total


ms = [Money(i - 1500) for i in range(3000)]
assert negate_all(ms) == sum(-(i - 1500) + abs(i - 1500) for i in range(3000))


# A list in a closure cell, read in a hot loop; the cell is rebound
# mid-loop by the closure.
def make():
    data = [float(i) for i in range(50)]

    def swap(new):
        nonlocal data
        data = new

    def total(n, at):
        s = 0.0
        for i in range(n):
            if i == at:
                swap([1.0] * 50)
            s += data[i % 50]
        return s

    return total


total = make()
assert total(3000, -1) == sum(float(i % 50) for i in range(3000))
total = make()
assert total(3000, 1000) == sum(float(i % 50) for i in range(1000)) + 2000.0


# Counted ranges whose bounds are expressions.
def bounded(rows, n):
    s = 0
    for i in range(1, n - 1):
        for j in range(i + 1, len(rows[i]) - 1):
            s += rows[i][j] * j
    return s


rows = [[(i * j) % 9 for j in range(60)] for i in range(60)]
assert bounded(rows, 60) == sum(rows[i][j] * j for i in range(1, 59) for j in range(i + 1, 59))


def overflowing_bound(n, big):
    s = 0
    for i in range(n):
        for j in range(big - n, big - n + 3):  # big - n overflows i64 eventually
            s += 1
    return s


assert overflowing_bound(2000, 100) == 6000
assert overflowing_bound(3, 2 ** 63 + 1) == 9

print("JIT generic items: ok")
