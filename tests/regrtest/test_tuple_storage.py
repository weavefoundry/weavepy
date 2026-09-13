"""Small tuple allocation preserves evaluation order, ownership, and reuse."""

import gc
import weakref


def one(a):
    return (a,)


def two(a, b):
    return (a, b)


def three(a, b, c):
    return (a, b, c)


def four(a, b, c, d):
    return (a, b, c, d)


values = [object() for _ in range(4)]
retained = []
for _ in range(80):
    for count, make in enumerate((one, two, three, four), 1):
        result = make(*values[:count])
        assert type(result) is tuple
        assert len(result) == count
        assert all(item is values[i] for i, item in enumerate(result))
        retained.append(result)
for i, result in enumerate(retained):
    assert len(result) == i % 4 + 1
    assert all(item is values[j] for j, item in enumerate(result))

calls = []


def value(i):
    calls.append(i)
    return values[i]


result = (value(0), value(1), value(2))
assert calls == [0, 1, 2]
assert all(result[i] is values[i] for i in range(3))
empty = ()
assert tuple() is empty

# A displaced tuple releases its sole references immediately, even when
# automatic cycle collection is disabled. Escaped tuples keep theirs.
finalized = []


class Leaf:
    def __init__(self, name):
        self.name = name

    def __del__(self):
        finalized.append(self.name)


gc.collect()
gc.disable()
for count, make in enumerate((one, two, three, four), 1):
    leaves = [Leaf((count, i)) for i in range(count)]
    refs = [weakref.ref(item) for item in leaves]
    result = make(*leaves)
    del leaves
    assert all(ref() is not None for ref in refs)
    del result
    assert all(ref() is None for ref in refs)
assert len(finalized) == 10

# Tuple/list cycles remain visible through their mutable anchor.
leaf = Leaf("cycle")
reference = weakref.ref(leaf)
anchor = [leaf]
cycle = (anchor,)
anchor.append(cycle)
del leaf, anchor, cycle
gc.collect()
assert reference() is None
assert finalized[-1] == "cycle"
gc.enable()
print("ok")
