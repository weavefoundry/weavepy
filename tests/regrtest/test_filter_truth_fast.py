"""`filter(None, it)` and `filter(bool, it)` over plain sequence iterators.

Items whose truth is plain data are stepped in place; an instance with a
`__bool__` between them is tested through it, in order; a list that
grows while it's filtered yields the new items; an exhausted filter
stays exhausted. Items are heap objects too. Run it past the compile
threshold.
"""


class B:
    calls = 0

    def __init__(self, v):
        self.v = v

    def __bool__(self):
        B.calls += 1
        return self.v


t, f = B(True), B(False)
for i in range(3000):
    B.calls = 0
    assert list(filter(None, (0, 1, "", "a", t, 0.0, f, None, 2.5, False))) == [1, "a", t, 2.5]
    assert B.calls == 2
    assert list(filter(bool, [0, "x", 0, ()])) == ["x"]
    xs = [0, 1]
    it = filter(None, xs)
    assert next(it) == 1
    xs.append(5)
    assert next(it) == 5
    assert next(it, "end") == "end"
    xs.append(7)
    assert next(it, "end") == "end"
    assert list(filter(None, "a\x00b")) == ["a", "\x00", "b"]
