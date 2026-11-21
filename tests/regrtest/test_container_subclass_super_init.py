"""`super().__init__(...)` in a `list`, `set` or `dict` subclass's `__init__`.

Compiled code calls the container's own `__init__` body in place, so the
body must still iterate its argument through the interpreter (a generator,
a class with `__iter__` that raises), check its argument count and the
items' hashability, and replace what the container held before. Run it
past the compile threshold.
"""


class L(list):
    def __init__(self, *args, **kw):
        super().__init__(*args, **kw)
        self.extra = 1


class S(set):
    def __init__(self, *args):
        super().__init__(*args)


class D(dict):
    def __init__(self, *args, **kw):
        super().__init__(*args, **kw)


def gen():
    yield 1
    yield 2


class Raises:
    def __iter__(self):
        raise ValueError("boom")


for _ in range(3000):
    lst = L(gen())
    assert lst == [1, 2] and lst.extra == 1, lst
    assert L((x for x in range(3))) == [0, 1, 2]
    assert L() == []
    assert S([1, 2, 2]) == {1, 2}
    assert D([("a", 1)], b=2) == {"a": 1, "b": 2}
    for bad, exc, text in (
        (lambda: L(1), TypeError, "not iterable"),
        (lambda: L([1], [2]), TypeError, "at most 1 argument"),
        (lambda: S([[1]]), TypeError, "unhashable"),
        (lambda: L(Raises()), ValueError, "boom"),
    ):
        try:
            bad()
        except exc as e:
            assert text in str(e), e
        else:
            raise AssertionError("no error")

again = L([5])
again.__init__([7, 8])
assert again == [7, 8], again
