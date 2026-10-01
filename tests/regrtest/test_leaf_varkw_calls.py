"""Small functions taking **kwargs see a fresh dictionary of exactly the
keywords they collect, however they are called."""


def with_kwargs(a, **kw):
    return a + kw.get("delta", 0)


def just_kw(a, **kw):
    return a


def mk(**kw):
    return kw


def count(**kw):
    return len(kw)


def first(a, b=10, **kw):
    return a + b + kw.get("c", 0)


class Box:
    def get(self, **kw):
        return kw.get("x", -1)


seen = []
box = Box()
for i in range(3000):
    assert with_kwargs(i, delta=2) == i + 2
    assert with_kwargs(i) == i
    assert with_kwargs(a=i, delta=3) == i + 3
    assert just_kw(i, other=1) == i
    d = mk(delta=i, other=2)
    assert d == {"delta": i, "other": 2} and list(d) == ["delta", "other"]
    seen.append(d)
    assert mk() == {}
    assert count(x=1, y=2, z=3) == 3
    assert first(i, c=5) == i + 15
    assert first(i, b=1, c=5) == i + 6
    assert box.get(x=i) == i
    assert box.get() == -1
    if i % 1000 == 0:
        try:
            with_kwargs(i, a=1)
        except TypeError as e:
            assert "multiple values" in str(e), e
        else:
            raise AssertionError("duplicate argument accepted")
        try:
            with_kwargs(i, 1)
        except TypeError as e:
            assert "positional" in str(e), e
        else:
            raise AssertionError("extra positional argument accepted")

# Every call built its own dictionary.
assert len({id(d) for d in seen}) == len(seen)
seen[0]["delta"] = "changed"
assert seen[1]["delta"] == 1
print("ok")
