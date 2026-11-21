"""Generator steps over builtin-subclass values, past the compile threshold.

`email`'s header parser runs generator expressions over a `list` subclass
whose items are `str` subclasses with properties: `iter()` of a list or
tuple subclass that keeps `__iter__` iterates its value; a property read in
a generator's step runs its getter (a pure one in place, any other through
the interpreter); and the truth of a builtin subclass is its value's,
unless the class overrides `__bool__` or `__len__`.
"""


class Term(str):
    @property
    def value(self):
        return self

    @property
    def defects(self):
        return []


class Raises(str):
    @property
    def value(self):
        raise KeyError(self)


class Tokens(list):
    @property
    def value(self):
        return "".join(x.value for x in self if x.value)

    @property
    def defects(self):
        return sum((x.defects for x in self), [])

    def __str__(self):
        return "".join(str(x) for x in self)


class Reversed(list):
    def __iter__(self):
        return iter(list.__iter__(self).__reduce__()[1][0][::-1])


class Empty(str):
    def __len__(self):
        return 0


class Falsy(str):
    def __bool__(self):
        return False


class Pair(tuple):
    pass


toks = Tokens([Term("a"), Term(""), Term("b")])
for _ in range(3000):
    assert toks.value == "ab", toks.value
    assert toks.defects == []
    assert str(toks) == "ab"
    assert [x for x in Reversed([1, 2, 3])] == [3, 2, 1]
    assert sum(x for x in Pair((1, 2))) == 3
    assert [bool(x) for x in (Term(""), Term("x"), Empty("x"), Falsy("x"))] == [
        False,
        True,
        False,
        False,
    ]
    try:
        "".join(x.value for x in Tokens([Raises("k")]))
    except KeyError as e:
        assert e.args == ("k",), e
    else:
        raise AssertionError("no KeyError")

grow = Tokens([Term("a")])
it = (x.value for x in grow)
assert next(it) == "a"
grow.append(Term("z"))
assert list(it) == ["z"]
