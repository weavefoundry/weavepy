"""Methods of a class's `__dict__` view (a mappingproxy) at warm call
sites: they read the class's current namespace, refuse writes, and stay
distinct from a proxy over another mapping."""

import types


class A:
    x = 1

    def f(self):
        return 2


for i in range(300):
    d = A.__dict__
    assert d.get("x") == 1
    assert d.get("missing") is None
    assert d.get("missing", i) == i
    assert "f" in d.keys()
    assert d["x"] == 1
    assert ("x", 1) in d.items()
    assert 1 in d.values()
    assert d.copy()["x"] == 1 and type(d.copy()) is dict
    assert "x" in d and "nope" not in d
    assert len(d) == len(dict(d))
    assert set(iter(d)) == set(d.keys())

# The view follows the class.
A.y = 5
for _ in range(100):
    assert A.__dict__.get("y") == 5
del A.y
for _ in range(100):
    assert A.__dict__.get("y", "gone") == "gone"

# Writes are refused.
try:
    A.__dict__["z"] = 1
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError")


# A proxy over a dict subclass calls the subclass's own methods.
class Loud(dict):
    def get(self, k, default=None):
        return ("loud", k)


proxy = types.MappingProxyType(Loud(a=1))
for _ in range(300):
    assert proxy.get("a") == ("loud", "a")
    assert A.__dict__.get("x") == 1

print("ok")
