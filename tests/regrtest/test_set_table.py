"""Set and frozenset tables: equal numbers share a slot, NaN finds itself,
user hashes run once per probe, errors surface, and iteration notices a
change of size, across the small (scanned) and slotted table sizes."""


class Hashed:
    calls = 0

    def __init__(self, value, h=None):
        self.value = value
        self.h = value if h is None else h

    def __hash__(self):
        Hashed.calls += 1
        return self.h

    def __eq__(self, other):
        return isinstance(other, Hashed) and self.value == other.value


class BadHash:
    def __hash__(self):
        raise ZeroDivisionError("bad hash")


class BadEq:
    def __hash__(self):
        return 10**9 + 7

    def __eq__(self, other):
        raise ValueError("bad eq")


for size in (0, 1, 7, 8, 9, 40, 1000):
    base = list(range(size))
    for factory in (set, frozenset):
        s = factory(base)
        assert len(s) == size
        # 1 == 1.0 == True, and -0.0 == 0: one slot each.
        for i in base:
            assert i in s and float(i) in s
        if size > 1:
            assert True in s and 1.0 in s and -0.0 in s and 0.0 in s
        assert (size + 0.5) not in s
        assert "0" not in s
        # Unhashable probes raise; a set probe retries as a frozenset.
        try:
            [] in s
        except TypeError:
            pass
        else:
            raise AssertionError("unhashable list probe did not raise")
        assert set() not in s

    m = set(base)
    m.add(True)
    m.add(1.0)
    assert len(m) == max(size, 2) if size else len(m) == 1
    if size > 1:
        # The first-inserted key stays.
        assert type([x for x in m if x == 1][0]) is int

    # NaN: identity finds the stored object; another NaN is a new key.
    nan = float("nan")
    m = set(base)
    m.add(nan)
    assert nan in m
    assert float("nan") not in m
    m.add(float("nan"))
    assert len(m) == size + 2
    m.discard(nan)
    assert nan not in m and len(m) == size + 1

    # A user hash runs once per probe, and once per insert.
    m = set(base)
    Hashed.calls = 0
    k = Hashed(3)
    m.add(k)
    assert Hashed.calls == 1, Hashed.calls
    assert Hashed(3) in m
    assert Hashed.calls == 2, Hashed.calls
    assert Hashed(4, 3) not in m
    assert Hashed.calls == 3, Hashed.calls
    m.discard(Hashed(3))
    assert Hashed.calls == 4 and k not in m

    # A raising hash surfaces from every operation, even on an empty set.
    for op in (lambda t: BadHash() in t, lambda t: t.add(BadHash()),
               lambda t: t.discard(BadHash())):
        try:
            op(set(base))
        except ZeroDivisionError:
            pass
        else:
            raise AssertionError("hash error swallowed")

    # A raising __eq__ surfaces only when hashes collide.
    m = set(base)
    m.add(BadEq())
    try:
        BadEq() in m
    except ValueError:
        pass
    else:
        raise AssertionError("eq error swallowed")
    assert Hashed(77, 10**9 + 8) not in m

    # Changing size during iteration raises; same-size churn does not crash.
    m = set(base) | {"x"}
    try:
        for v in m:
            m.add(object())
    except RuntimeError as e:
        assert "changed size during iteration" in str(e), e
    else:
        raise AssertionError("set mutation during iteration not detected")
    m = set(base) | {"x"}
    seen = 0
    for v in m:
        m.discard(v)
        m.add(v)
        seen += 1
        if seen > 3 * len(m) + 3:
            break

    # Removals keep every survivor findable.
    m = set(range(size))
    for i in range(0, size, 3):
        m.remove(i)
    assert m == {i for i in range(size) if i % 3}
    assert all(i in m for i in range(size) if i % 3)
    while m:
        x = m.pop()
        assert x not in m

# Set algebra and comparisons across sizes and types.
a = set(range(0, 60, 2))
b = frozenset(range(0, 60, 3))
assert a & b == {x for x in range(60) if x % 6 == 0}
assert a | b == {x for x in range(60) if x % 2 == 0 or x % 3 == 0}
assert a - b == {x for x in range(0, 60, 2) if x % 3}
assert a ^ b == (a | b) - (a & b)
assert frozenset(a) == a and a == frozenset(a)
assert hash(frozenset(a)) == hash(frozenset(sorted(a, reverse=True)))
assert {1, 2} < {1, 2, 3} and not {1, 2} < {1, 2}
assert {1.0, 2} == {True, 2.0}
print("set table ok")
