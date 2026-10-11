"""Operations `inspect` leans on, at warm sites: membership in and
subscripts of a class's `__dict__` view, `sys.getrecursionlimit()`, and constructing `weakref.ref`
and `types.MappingProxyType` objects."""

import sys
import types
import weakref


class A:
    x = 1


for i in range(300):
    assert "x" in A.__dict__
    assert "missing" not in A.__dict__
    assert ("x" not in A.__dict__) is False
    assert "__dict__" in A.__dict__
    assert "__mro__" in type.__dict__
for i in range(300):
    assert A.__dict__["x"] == 1
    assert type.__dict__["__mro__"].__get__(A) == (A, object)
    try:
        A.__dict__["missing"]
    except KeyError as e:
        assert e.args == ("missing",)
    else:
        raise AssertionError("expected KeyError")
A.y = 2
assert "y" in A.__dict__ and A.__dict__["y"] == 2
del A.y
assert "y" not in A.__dict__

old = sys.getrecursionlimit()
try:
    for limit in (old, old + 50, old + 100):
        sys.setrecursionlimit(limit)
        for _ in range(100):
            assert sys.getrecursionlimit() == limit
finally:
    sys.setrecursionlimit(old)


class Target:
    pass


for _ in range(300):
    t = Target()
    r = weakref.ref(t)
    assert r() is t
    del t
    assert r() is None

for i in range(300):
    d = {"k": i}
    proxy = types.MappingProxyType(d)
    assert proxy["k"] == i and len(proxy) == 1
    d["k2"] = 0
    assert "k2" in proxy
try:
    types.MappingProxyType(1)
except TypeError as e:
    assert "mappingproxy() argument must be a mapping" in str(e), str(e)
else:
    raise AssertionError("expected TypeError")
try:
    weakref.ref(1)
except TypeError as e:
    assert "cannot create weak reference to 'int' object" in str(e), str(e)
else:
    raise AssertionError("expected TypeError")

print("ok")
