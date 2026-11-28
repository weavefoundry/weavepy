"""`reversed()` of something that isn't a sequence raises `TypeError`.

CPython's `reversed` wants `__reversed__`, or `__len__` and `__getitem__`.
An iterator, a generator, a set or a native adapter has neither, so it
isn't reversible, even when iterating it would end (and an endless
`itertools.count()` must not be read to the end first).
"""

import collections
import itertools

for x in (
    itertools.count(),
    itertools.repeat(1),
    itertools.cycle([1]),
    map(str, [1]),
    zip([1]),
    enumerate([1]),
    filter(None, [1]),
    iter([1]),
    iter({}.items()),
    (i for i in [1]),
    {1},
    frozenset(),
):
    try:
        reversed(x)
    except TypeError as e:
        assert "not reversible" in str(e), e
    else:
        raise AssertionError(f"reversed() accepted {type(x).__name__}")


class Seq:
    def __len__(self):
        return 2

    def __getitem__(self, i):
        if i > 1:
            raise IndexError
        return i * 10


class Rev:
    def __reversed__(self):
        return iter("zy")


class MyList(list):
    pass


assert list(reversed(Seq())) == [10, 0]
assert list(reversed(Rev())) == ["z", "y"]
assert list(reversed(MyList([1, 2]))) == [2, 1]
assert list(reversed(collections.deque([1, 2]))) == [2, 1]
assert list(reversed({"a": 1, "b": 2})) == ["b", "a"]
assert list(reversed("ab")) == ["b", "a"]
assert list(reversed(range(3))) == [2, 1, 0]
