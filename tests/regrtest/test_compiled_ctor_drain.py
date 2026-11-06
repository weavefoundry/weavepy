"""`list`, `tuple`, `set` and `frozenset` of a fresh generator, called from
compiled code (whose fast steps drain the generator straight into the
container), keep CPython's results: the items in order, duplicates,
equal keys of different kinds, items that hash in Python, an error the
generator raises, a generator already started, closures, and nesting."""
import traceback


class Key:
    hashed = 0

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        Key.hashed += 1
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and other.v == self.v


def boom(xs):
    return list(1 // x for x in xs)


def run(i):
    xs = range(i % 5, i % 5 + 6)
    assert list(x * 2 for x in xs) == [x * 2 for x in xs]
    assert tuple(x for x in xs if x % 2) == tuple(x for x in list(xs) if x % 2)
    assert set(x % 3 for x in xs) == {x % 3 for x in xs}
    assert frozenset(str(x) for x in xs) == frozenset(map(str, xs))
    assert list(x for x in ()) == [] and set(x for x in ()) == set()
    # Equal keys of different kinds: the first stays, as CPython keeps it.
    s = set(v for v in (1, 1.0, True, 2))
    assert s == {1, 2} and [type(v) for v in sorted(s)] == [int, int]
    # Items that hash in Python.
    before = Key.hashed
    ks = set(Key(x % 2) for x in xs)
    assert len(ks) == 2 and Key.hashed - before == 6
    # A raise partway: the error, with the generator's frame.
    try:
        boom([3, 2, 0, 1])
    except ZeroDivisionError as e:
        names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert names[-1] == "<genexpr>", names
    else:
        raise AssertionError("the generator's raise was lost")
    # A generator already started: the rest.
    g = (x for x in xs)
    first = next(g)
    assert list(g) == list(xs)[1:] and first == xs[0]
    # A closure, and a generator of generators.
    k = i % 7
    assert tuple(x + k for x in xs) == tuple(x + k for x in list(xs))
    assert list(list(y for y in range(x)) for x in range(3)) == [[], [0], [0, 1]]
    assert set(tuple(c for c in w) for w in ("ab", "ab", "c")) == {("a", "b"), ("c",)}


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
print("ok")
