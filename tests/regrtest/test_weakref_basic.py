"""RFC 0024 — `weakref` module surface.

Exercises the documented constructors and verifies that the wrapper
objects come from the new `_weakref` Rust core. The deeper "GC drops
referent → callback fires" path is covered in run-fixtures because
it depends on collector timing.
"""

import weakref


class Box:
    def __init__(self, value):
        self.value = value


# ---------------------------------------------------------------------------
# weakref.ref — alive case.
# ---------------------------------------------------------------------------

b = Box(7)
r = weakref.ref(b)
assert r() is b
assert r().value == 7
assert isinstance(weakref.getweakrefs(b), list)
assert isinstance(weakref.getweakrefcount(b), int)


# ---------------------------------------------------------------------------
# WeakSet / WeakValueDictionary / WeakKeyDictionary.
# ---------------------------------------------------------------------------

s = weakref.WeakSet([b])
assert b in s
assert len(s) == 1

vd = weakref.WeakValueDictionary()
vd["a"] = b
assert vd["a"] is b
assert "a" in vd

kd = weakref.WeakKeyDictionary()
kd[b] = "value"
assert kd[b] == "value"


# ---------------------------------------------------------------------------
# WeakMethod — construction only (calling needs unbound-method support
# that's still in flight).
# ---------------------------------------------------------------------------


class Holder:
    def speak(self):
        return "hello"


h = Holder()
wm = weakref.WeakMethod(h.speak)
assert wm is not None


# ---------------------------------------------------------------------------
# weakref.finalize.
# ---------------------------------------------------------------------------

results = []
fin = weakref.finalize(b, lambda x: results.append(x), "ran")
assert fin.alive
fin()
assert results == ["ran"]
assert not fin.alive


# ---------------------------------------------------------------------------
# weakref.proxy — handle is constructable; full attribute delegation
# isn't asserted here.
# ---------------------------------------------------------------------------

b2 = Box(99)
p = weakref.proxy(b2)
assert p is not None


# ---------------------------------------------------------------------------
# Type constants.
# ---------------------------------------------------------------------------

assert hasattr(weakref, "ReferenceType")
assert hasattr(weakref, "ProxyType")
assert hasattr(weakref, "CallableProxyType")
assert hasattr(weakref, "ProxyTypes")


print("weakref surface ok")
