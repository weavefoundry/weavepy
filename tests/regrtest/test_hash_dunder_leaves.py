"""A `__hash__` that returns `hash()` of an attribute, warm enough to run
frameless, hashes what the attribute holds now: a native value's hash,
another instance's `__hash__`, an unhashable value's `TypeError`, and a
NaN's identity hash."""


class Key:
    def __init__(self, x):
        self.x = x

    def __hash__(self):
        return hash(self.x)

    def __eq__(self, other):
        return isinstance(other, Key) and self.x == other.x


class Inner:
    def __hash__(self):
        return 12345


nan = float("nan")
values = [5, -1, 2**70, "s", "", (1, "a"), 2.5, True, None, Inner()]
keys = [Key(v) for v in values]
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    for k, v in zip(keys, values):
        assert hash(k) == hash(v), (k.x, hash(k), hash(v))
    d = {k: n for n, k in enumerate(keys)}
    assert all(d[Key(v)] == n for n, v in enumerate(values) if not isinstance(v, Inner))
    assert len({Key(1), Key(1), Key(2)}) == 2
    assert hash(Key(-1)) == -2

k = Key(1)
h1 = hash(k)
k.x = 2
assert hash(k) == hash(2) != h1
k.x = nan
assert hash(k) == hash(nan)
k.x = [1]
try:
    hash(k)
except TypeError as e:
    assert "unhashable" in str(e), e
else:
    raise AssertionError("hash of a list didn't raise")
print("ok")
