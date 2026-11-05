"""Dictionary merges run by compiled code (`{**a, **b}`, `f(**kw)` and a
closure's `cb(*args, **kwds)`) keep CPython's results, order and errors:
later keys win in a display, a repeated keyword in a call raises, and
non-string keys or other mappings take the general path."""


class Sub:
    def keys(self):
        return ["k"]

    def __getitem__(self, key):
        return "sub"


def f(**kw):
    return kw


def make(cb, *args, **kwds):
    def wrapper():
        return cb(*args, **kwds)

    return wrapper


wrap = make(f, x=1, y=2)
wrap_empty = make(f)
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    a = {"x": 1, "y": 2}
    b = {"y": 3, "z": i}
    m = {**a, **b}
    assert m == {"x": 1, "y": 3, "z": i} and list(m) == ["x", "y", "z"], m
    assert {**a, 1: "int"} == {"x": 1, "y": 2, 1: "int"}
    assert {**{1: 2}, **a} == {1: 2, "x": 1, "y": 2}
    assert {**Sub()} == {"k": "sub"}
    assert f(**a, w=0) == {"x": 1, "y": 2, "w": 0}
    assert f(**Sub()) == {"k": "sub"}
    assert wrap() == {"x": 1, "y": 2} and wrap() is not wrap()
    assert wrap_empty() == {}

    try:
        f(**a, **{"x": 5})
    except TypeError as e:
        assert "multiple values" in str(e), e
    else:
        raise AssertionError("repeated keyword didn't raise")

    try:
        f(**{1: 2})
    except TypeError as e:
        assert "keywords must be strings" in str(e), e
    else:
        raise AssertionError("non-string keyword didn't raise")
print("ok")
