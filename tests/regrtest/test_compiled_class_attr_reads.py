"""A class attribute read through an instance from compiled code keeps
CPython's results when the read is served from the site's caches: a
scalar or a method bound to the receiver, until the instance shadows the
name, the class's value or method changes, the instance changes class,
its `__dict__` is published and written, or a subclass overrides it."""


class C:
    flag = True
    limit = 3

    def m(self):
        return ("C.m", id(self))

    def read(self):
        return self.flag, self.limit

    def bound(self):
        return self.m


class D(C):
    flag = False

    def m(self):
        return ("D.m", id(self))


def reads(objs, n):
    out = []
    for _ in range(n):
        for o in objs:
            out.append(o.read())
    return out[-len(objs):]


def bounds(objs, n):
    out = []
    for _ in range(n):
        for o in objs:
            out.append(o.bound()())
    return out[-len(objs):]


c, c2, d = C(), C(), D()
assert reads([c, c2, d], 2000) == [(True, 3), (True, 3), (False, 3)]
assert bounds([c, d], 2000) == [("C.m", id(c)), ("D.m", id(d))]

# The bound method is a fresh one bound to its receiver.
b1, b2 = c.bound(), c.bound()
assert b1 == b2 and b1 is not b2 and b1.__self__ is c and b1.__func__ is C.m

# The instance shadows the scalar and the method.
c2.flag = "own"
c2.m = lambda: "instance m"
assert reads([c, c2], 10) == [(True, 3), ("own", 3)]
assert bounds([c, c2], 10) == [("C.m", id(c)), "instance m"]

# The class changes its value and its method.
C.limit = 7
C.m = lambda self: ("new C.m", id(self))
assert reads([c, d], 10) == [(True, 7), (False, 7)]
assert bounds([c], 10) == [("new C.m", id(c))]

# The instance changes class.
c.__class__ = D
assert reads([c], 10) == [(False, 7)]
assert bounds([c], 10) == [("D.m", id(c))]
c.__class__ = C

# A published `__dict__`, written through.
c.__dict__["limit"] = "dict"
assert reads([c], 10) == [(True, "dict")]
del c.__dict__["limit"]
assert reads([c], 10) == [(True, 7)]

# Deleting the instance's shadow brings the class's back.
del c2.flag
del c2.m
assert reads([c2], 10) == [(True, 7)]
assert bounds([c2], 10) == [("new C.m", id(c2))]
print("ok")
