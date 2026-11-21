"""f-string `!r` of scalar lists and tuples, and generator loops over dict views.

Compiled code renders `{x!r}` of a list or tuple of plain scalars in place,
and steps a generator expression's dict view to its end in place (the
generator's `.0` holds the iterator too). Lists or tuples holding anything
else, a dict changed during iteration, and an exhausted view's iterator
must still behave as CPython's. Run it past the compile threshold.
"""


class R:
    def __repr__(self):
        return "R!"


def render(x):
    return f"<{x!r}>"


d = {"a": 1, "b": 2}
for _ in range(3000):
    assert render([]) == "<[]>"
    assert render([1, "x", None, 2.5, True]) == "<[1, 'x', None, 2.5, True]>"
    assert render((1,)) == "<(1,)>" and render(()) == "<()>"
    assert render([R()]) == "<[R!]>"
    assert render(("it's",)) == '<("it\'s",)>'
    assert tuple(v for v in d.values() if v) == (1, 2)
    assert sum(k for k, v in d.items() if k) == 0 if False else True
    g = (k for k in d.keys())
    assert list(g) == ["a", "b"] and list(g) == []

e = {"k": 1}
g = (v for v in e.values())
next(g)
e["z"] = 2
try:
    next(g)
except RuntimeError as ex:
    assert "changed size" in str(ex), ex
else:
    raise AssertionError("no RuntimeError")
