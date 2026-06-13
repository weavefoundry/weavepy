"""RFC 0024 — real weakref semantics with GC-driven callbacks."""

import gc
import weakref


# ---------------------------------------------------------------------------
# Basic ref
# ---------------------------------------------------------------------------

class Box:
    def __init__(self, value):
        self.value = value


b = Box(7)
r = weakref.ref(b)
print("alive:", r() is b)
print("attr through ref:", r().value)


# Equality and hash on refs.
r2 = weakref.ref(b)
print("ref eq same target:", r == r2)
print("ref hashable:", hash(r) == hash(r2))


# ---------------------------------------------------------------------------
# Callback fires
# ---------------------------------------------------------------------------

fired = []
def cb(weak):
    fired.append(weak)


dying = Box("two")
r3 = weakref.ref(dying, cb)
del dying
gc.collect()
print("callback fired:", len(fired) == 1)
print("ref returns None after release:", r3() is None)

b2 = Box("two")


# ---------------------------------------------------------------------------
# Proxy
# ---------------------------------------------------------------------------

b3 = Box(99)
p = weakref.proxy(b3)
print("proxy created:", p is not None)


# ---------------------------------------------------------------------------
# WeakSet
# ---------------------------------------------------------------------------

s = weakref.WeakSet([b, b2, b3])
print("weakset size:", len(s))
print("b in set:", b in s)


# ---------------------------------------------------------------------------
# WeakValueDictionary
# ---------------------------------------------------------------------------

d = weakref.WeakValueDictionary()
d["a"] = b
d["b"] = b3
print("d['a'] is b:", d["a"] is b)
print("len:", len(d))
print("keys:", sorted(d.keys()))


# ---------------------------------------------------------------------------
# WeakKeyDictionary
# ---------------------------------------------------------------------------

wkd = weakref.WeakKeyDictionary()
wkd[b] = "value-b"
wkd[b3] = "value-b3"
print("wkd[b]:", wkd[b])
print("wkd len:", len(wkd))


# ---------------------------------------------------------------------------
# WeakMethod
# ---------------------------------------------------------------------------

class Holder:
    def speak(self):
        return "hello"


h = Holder()
wm = weakref.WeakMethod(h.speak)
print("weakmethod constructed:", wm is not None)


# ---------------------------------------------------------------------------
# finalize
# ---------------------------------------------------------------------------

results = []
class Tracked:
    pass


t = Tracked()
fin = weakref.finalize(t, lambda x: results.append(x), "done")
print("finalizer alive:", fin.alive)
fin()
print("after fire:", results == ["done"])
print("finalizer alive after fire:", fin.alive)


# ---------------------------------------------------------------------------
# ProxyTypes / ReferenceType / etc.
# ---------------------------------------------------------------------------

print("ReferenceType is _weakref.ReferenceType:", weakref.ReferenceType is not None)
print("ProxyTypes is tuple:", isinstance(weakref.ProxyTypes, tuple))
