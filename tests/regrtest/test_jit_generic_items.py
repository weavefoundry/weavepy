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


# Rows of (index, weight) pairs: the pair lanes are trained from the live
# row, and every surprise (an int weight, a non-pair element, a pair that
# isn't a 2-tuple, a row that grows mid-loop) must behave as interpreted.
def sparse(rows, x, cycles):
    y = [0.0] * len(rows)
    for _ in range(cycles):
        for r, row in enumerate(rows):
            s = 0.0
            for col, v in row:
                s += x[col] * v
            y[r] = s
    return y


def sparse_ref(rows, x):
    return [sum(x[c] * v for c, v in row) for row in rows]


x = [float(i % 7) + 0.5 for i in range(40)]
rows = [[((r * 3 + k) % 40, (r + k) * 0.25) for k in range(5)] for r in range(60)]
assert sparse(rows, x, 20) == sparse_ref(rows, x)
rows[45][2] = (rows[45][2][0], 3)  # an int weight in a float lane
assert sparse(rows, x, 20) == sparse_ref(rows, x)
rows[50][1] = [7, 1.5]  # a list pair unpacks too
assert sparse(rows, x, 20) == sparse_ref(rows, x)
rows[55] = tuple(rows[55])  # a tuple row
assert sparse(rows, x, 20) == sparse_ref(rows, x)
rows[58][3] = (1, 2.0, 3.0)
try:
    sparse(rows, x, 20)
except ValueError as exc:
    assert str(exc) == "too many values to unpack (expected 2, got 3)", exc
else:
    raise AssertionError("expected ValueError")


def growing(pairs):
    total = 0.0
    for a, b in pairs:
        if a == 100 and len(pairs) < 3000:
            pairs.append((a, b + 1.0))  # the iterator sees appended pairs
        total += a * b
    return total, len(pairs)


def growing_ref(pairs):
    total, i = 0.0, 0
    while i < len(pairs):
        a, b = pairs[i]
        if a == 100 and len(pairs) < 3000:
            pairs.append((a, b + 1.0))
        total += a * b
        i += 1
    return total, len(pairs)


assert growing([(i % 101, 0.5) for i in range(2000)]) == \
    growing_ref([(i % 101, 0.5) for i in range(2000)])


# A metaclass `__call__` runs for every construction in a hot loop.
class Counting(type):
    calls = 0

    def __call__(cls, *args):
        Counting.calls += 1
        return super().__call__(*args)


class Made(metaclass=Counting):
    def __init__(self, v):
        self.v = v


def construct(n):
    total = 0.0
    for i in range(n):
        total += Made(i * 0.5).v
    return total


assert construct(3000) == sum(i * 0.5 for i in range(3000))
assert Counting.calls == 3000, Counting.calls

# Attribute stores and loads on a global instance: the site caches serve
# the plain field, and a class change mid-loop (a property, `__slots__`
# style storage, a `__setattr__`) must take effect on the next iteration.
class Box:
    def __init__(self):
        self.x = 0
        self.y = 0


box = Box()
log = []


def install():
    def setter(self, v):
        log.append(v)
        self.__dict__["_x"] = v
    Box.x = property(lambda self: self.__dict__.get("_x", -1), setter)


def store_loop(n, at):
    total = 0
    for i in range(n):
        if i == at:
            install()
        box.x = i
        total += box.x
    return total


assert store_loop(3000, -1) == sum(range(3000)) and box.x == 2999
assert store_loop(3000, 2000) == sum(range(3000)), store_loop
assert log == list(range(2000, 3000)), log[:3]
del Box.x


class Watched:
    def __init__(self):
        self.v = 0

    def __setattr__(self, name, value):
        if value == 2500:
            raise AttributeError("no 2500")
        object.__setattr__(self, name, value)


watched = Watched()


def guarded_stores(n):
    for i in range(n):
        watched.v = i


try:
    guarded_stores(3000)
except AttributeError as exc:
    assert str(exc) == "no 2500" and watched.v == 2499
else:
    raise AssertionError("expected AttributeError")

print("JIT generic items: ok")
