"""`x.__class__` of a temporary that dies at the read.

`super(C, obj).__class__` reads the class of a proxy nothing else holds.
Compiled code reads it and releases the proxy in place; a temporary with a
finalizer dies at the read too, and its `__del__` runs before the next
statement, as CPython runs it. Run it past the compile threshold.
"""

log = []


class D:
    def __del__(self):
        log.append("del")


class B:
    def m(self):
        return "B.m"


class C(B):
    def m(self):
        return super().m() + "+C"


def proxies(o):
    for _ in range(3000):
        assert super(C, o).__class__ is super
        s = super(C, o)
        assert s.m() == "B.m" and s.__self__ is o and s.__thisclass__ is C
        assert s.__self_class__ is C


def temporaries():
    for _ in range(3000):
        del log[:]
        cls = D().__class__
        log.append("after")
        assert cls is D and log == ["del", "after"], log


proxies(C())
temporaries()
assert C().m() == "B.m+C"
