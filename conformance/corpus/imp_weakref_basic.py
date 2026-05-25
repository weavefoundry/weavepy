"""RFC 0024 — `weakref` module surface."""

import weakref


class Box:
    def __init__(self, value):
        self.value = value


b = Box(7)
r = weakref.ref(b)
print(r() is b)
print(r().value)


# Set with weak references
s = weakref.WeakSet([b])
print(b in s)
print(len(s))


# Dict with weak values
d = weakref.WeakValueDictionary()
d["a"] = b
print(d["a"] is b)


# Dict with weak keys
wkd = weakref.WeakKeyDictionary()
wkd[b] = "value"
print(wkd[b])


# finalize
done = []
fin = weakref.finalize(b, lambda: done.append("ran"))
print(fin.alive)
fin()
print(done)


# proxy
b2 = Box(99)
p = weakref.proxy(b2)
print(p is not None)
