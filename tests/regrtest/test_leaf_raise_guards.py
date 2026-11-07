"""Small functions guarded by `if bad: raise ...`, warm enough to run
frameless from native callers (`sorted`'s key, `hash`, `==`, a property),
keep their semantics on both sides of the guard: the value when it
passes, and the exception, its traceback and the raise path's own effects
(exactly once) when it doesn't."""
import traceback


class P:
    def __init__(self, a):
        self.a = a

    def __hash__(self):
        if self.a is None:
            raise TypeError("unhashable P(%r)" % (self.a,))
        return hash(self.a)

    def __eq__(self, other):
        if not isinstance(other, P):
            raise TypeError("compared with %r" % (other,))
        return self.a == other.a

    @property
    def doubled(self):
        if self.a < 0:
            raise ValueError("negative %r" % (self.a,))
        return self.a * 2


log = []


def key(p):
    if p.a is None:
        log.append(p)
        raise ValueError("no key for %r" % (p,))
    return -p.a


ps = [P(i) for i in range(10)]
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    assert [p.a for p in sorted(ps, key=key)] == list(range(9, -1, -1))
    assert hash(ps[3]) == 3
    assert ps[4] == P(4) and not (ps[4] == P(5))
    assert ps[2].doubled == 4
    assert {P(1): "x"}[P(1)] == "x"

    try:
        sorted([P(1), P(None)], key=key)
    except ValueError as e:
        assert str(e).startswith("no key for"), e
        frames = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert frames[-1] == "key", frames
    else:
        raise AssertionError("key didn't raise")
    assert len(log) == i + 1, len(log)

    try:
        hash(P(None))
    except TypeError as e:
        assert str(e) == "unhashable P(None)"
    else:
        raise AssertionError("hash didn't raise")

    try:
        P(1) == 3
    except TypeError as e:
        assert str(e) == "compared with 3"
    else:
        raise AssertionError("== didn't raise")

    try:
        P(-2).doubled
    except ValueError as e:
        assert str(e) == "negative -2"
    else:
        raise AssertionError("property didn't raise")
print("ok")
