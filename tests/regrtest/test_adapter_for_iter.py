"""`for` loops over builtin adapters (`map`, `filter`, `enumerate`, `zip`,
`itertools.groupby`, ...) stepped natively: results, exhaustion, errors
and tracebacks from the callback, `StopIteration` raised by a callback,
mutation of the source, and finalizer timing. Each loop runs enough times
to warm the interpreter's fast paths and the compiled loop."""
import gc
import itertools
import sys
import traceback

ROUNDS = 300


def leaf(x):
    return x + 1


def nonleaf(x):
    return abs(x) + len(str(x))


class C:
    def m(self, x):
        return x * 2


def check(name, make, want):
    for _ in range(ROUNDS):
        got = []
        for x in make():
            got.append(x)
        assert got == want, (name, got[:10], want[:10])


xs = list(range(40))
ws = ["a", "bb", "", "ccc"] * 5
check("map leaf", lambda: map(leaf, xs), [x + 1 for x in xs])
check("map nonleaf", lambda: map(nonleaf, xs), [abs(x) + len(str(x)) for x in xs])
check("map int", lambda: map(int, ws[:0] + ["1", "22", "-3"]), [1, 22, -3])
check("map str", lambda: map(str, xs), [str(x) for x in xs])
check("map len", lambda: map(len, ws), [len(w) for w in ws])
check("map type", lambda: map(type, [1, "a", 2.0, None]), [int, str, float, type(None)])
check("map bound", lambda: map(C().m, xs), [x * 2 for x in xs])
check("map bound builtin", lambda: map({1: "x"}.get, [1, 2]), ["x", None])
check("map two", lambda: map(lambda a, b: a * b, xs, xs[1:]), [a * b for a, b in zip(xs, xs[1:])])
check("map same source", lambda: (lambda it: map(lambda a, b: (a, b), it, it))(iter(xs)),
      [(xs[i], xs[i + 1]) for i in range(0, 40, 2)])
check("filter None", lambda: filter(None, [0, 1, "", "a", None, 2.0]), [1, "a", 2.0])
check("filter leaf", lambda: filter(lambda v: v % 3, xs), [x for x in xs if x % 3])
check("filter nonleaf", lambda: filter(nonleaf, [0, -1, 2]), [0, -1, 2])
check("filter len", lambda: filter(len, ws), [w for w in ws if w])
check("enumerate map", lambda: enumerate(map(leaf, xs), 5), list(enumerate([x + 1 for x in xs], 5)))
check("enumerate gen", lambda: enumerate(x for x in xs), list(enumerate(xs)))
check("zip", lambda: zip(xs, ws), list(zip(xs, ws)))
check("reversed", lambda: reversed(xs), xs[::-1])
check("chain", lambda: itertools.chain(xs, ws), xs + ws)
check("islice", lambda: itertools.islice(xs, 3, 30, 4), xs[3:30:4])
keys = [1, 1, 2, 2, 2, 1, 3, 3]
check("groupby", lambda: ((k, list(g)) for k, g in itertools.groupby(keys)),
      [(1, [1, 1]), (2, [2, 2, 2]), (1, [1]), (3, [3, 3])])
check("groupby type", lambda: ((k, len(list(g))) for k, g in itertools.groupby([1, 2, "a", 3.0, 4.0], type)),
      [(int, 2), (str, 1), (float, 2)])
check("groupby leaf key", lambda: ((k, list(g)) for k, g in itertools.groupby(xs[:9], lambda v: v // 3)),
      [(0, [0, 1, 2]), (1, [3, 4, 5]), (2, [6, 7, 8])])
# A grouper outlived by its groupby's advance is empty.
for _ in range(ROUNDS):
    groups = list(itertools.groupby(keys))
    assert [list(g) for _, g in groups] == [[], [], [], []], [list(g) for _, g in groups]

# A grouper's own `for`, stopping when the key changes.
for _ in range(ROUNDS):
    out = []
    for k, g in itertools.groupby("aaabbc"):
        n = 0
        for _c in g:
            n += 1
        out.append((k, n))
    assert out == [("a", 3), ("b", 2), ("c", 1)], out


# A raise in the callback propagates with its traceback.
def boom(x):
    if x == 7:
        raise ValueError("boom %d" % x)
    return x


for _ in range(ROUNDS):
    seen = []
    try:
        for v in map(boom, xs):
            seen.append(v)
    except ValueError as e:
        assert str(e) == "boom 7"
        names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert names[-1] == "boom", names
    else:
        raise AssertionError("no raise")
    assert seen == list(range(7))

# A pure leaf that raises (division by zero) raises from the loop.
for _ in range(ROUNDS):
    try:
        for v in map(lambda d: 10 // d, [5, 2, 0, 1]):
            pass
    except ZeroDivisionError:
        pass
    else:
        raise AssertionError("no ZeroDivisionError")


# StopIteration from a callback ends the loop (CPython's FOR_ITER clears it).
def stopper(x):
    if x == 5:
        raise StopIteration
    return x


for _ in range(ROUNDS):
    got = []
    for v in map(stopper, xs):
        got.append(v)
    assert got == [0, 1, 2, 3, 4], got
    got = []
    for v in filter(stopper, xs):
        got.append(v)
    assert got == [1, 2, 3, 4], got
    # ... and the map stays exhausted only as far as its source is.
    m = map(stopper, iter([5, 6]))
    assert list(m) == []
    assert list(m) == [6]

# An exhausted adapter stays exhausted; length hints.
for _ in range(ROUNDS):
    m = map(leaf, xs)
    for _v in m:
        pass
    assert list(m) == []
    it = iter(xs)
    m = map(leaf, it)
    next(m)
    assert it.__length_hint__() == 39
    assert next(m) == 2

# Mutating the source list during the loop.
for _ in range(ROUNDS):
    src = [1, 2, 3]
    got = []
    for v in map(leaf, src):
        got.append(v)
        if len(src) < 6:
            src.append(v * 10)
    assert got == [2, 3, 4, 21, 31, 41], got
    src = [1, 2, 3, 4]
    got = []
    for v in filter(None, src):
        got.append(v)
        if src:
            del src[-1]
    assert got == [1, 2], got

# Finalizers of items the callback drops run promptly.
log = []


class Fin:
    def __init__(self, i):
        self.i = i

    def __del__(self):
        log.append(self.i)


def take(f):
    return f.i


for _ in range(50):
    log.clear()
    for i in map(take, [Fin(j) for j in range(5)]):
        pass
    gc.collect()
    assert sorted(log) == list(range(5)), log

# A set changing size during iteration still raises.
for _ in range(ROUNDS):
    s = set(range(10))
    try:
        for v in map(leaf, s):
            s.add(v + 100)
    except RuntimeError as e:
        assert "changed size" in str(e)
    else:
        raise AssertionError("set size change not detected")

print("adapter for-iter ok")
