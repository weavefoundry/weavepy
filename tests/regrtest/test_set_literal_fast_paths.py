"""Set literals keep their semantics when compiled code builds them."""


def build(a, b, c):
    return {a, b, c}


def update(a):
    return {1, 2, 3, a}


def starred(xs):
    return {*xs, 0}


def constant():
    return {"x", "y", "z"}


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and other.v == self.v


# Past the frame JIT's threshold, so later rounds run compiled code.
for _ in range(3000):
    s = build(1, True, 1.0)
    assert s == {1} and len(s) == 1, s
    assert type(next(iter(s))) is int, s
    assert build("a", "b", "a") == {"a", "b"}
    assert build(None, 2.5, -3) == {None, 2.5, -3}
    assert update(4) == {1, 2, 3, 4}
    assert update(1) == {1, 2, 3}
    assert starred([5, 6]) == {0, 5, 6}
    assert starred({7}) == {0, 7}
    first, second = constant(), constant()
    assert first == {"x", "y", "z"} and first is not second
    first.add("w")
    assert constant() == {"x", "y", "z"}
    assert len(build(Key(1), Key(1), 3)) == 2
    try:
        build([], 1, 2)
    except TypeError as error:
        assert "unhashable" in str(error), error
    else:
        raise AssertionError("an unhashable list joined a set")
print("set literal fast paths: ok")
