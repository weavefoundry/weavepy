"""`for` over instances whose class's `__iter__` is a Python function.

`configparser`'s `SectionProxy.__init__` loops over a `ConverterMapping`
whose `__iter__` returns `iter(self._data)`. Compiled code calls such a
method directly; its result must still be an iterator (an instance
without `__next__` is the `TypeError` `iter()` raises), an exception it
raises propagates, and replacing `__iter__` on the class takes effect.
Items are heap objects. Run it past the compile threshold.
"""


class Item:
    pass


class Wrap:
    def __init__(self, data):
        self._data = data

    def __iter__(self):
        return iter(self._data)


class Gen:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        for i in range(self.n):
            yield Item()


class Self:
    def __init__(self, n):
        self.i, self.n = 0, n

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            raise StopIteration
        self.i += 1
        return self.i


class NoNext:
    def __iter__(self):
        return Item()


class Boom:
    def __iter__(self):
        raise ValueError("boom")


def total(obj):
    n = 0
    for x in obj:
        n += 1
    return n


items = [Item(), Item(), Item()]
for _ in range(3000):
    assert total(Wrap(items)) == 3
    assert total(Gen(2)) == 2
    assert total(Self(4)) == 4
    assert [type(x) for x in Wrap(items)] == [Item] * 3
    try:
        total(NoNext())
    except TypeError as e:
        assert "non-iterator" in str(e), e
    else:
        raise AssertionError("no TypeError")
    try:
        total(Boom())
    except ValueError as e:
        assert str(e) == "boom"
    else:
        raise AssertionError("no ValueError")

Wrap.__iter__ = lambda self: iter([1, 2])
for _ in range(100):
    assert total(Wrap(items)) == 2
