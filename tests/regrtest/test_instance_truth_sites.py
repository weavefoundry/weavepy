"""Truth tests of instances keep Python's semantics at hot sites.

Compiled code answers a truth test of an instance in line when its class
(as it stands) has neither `__bool__` nor `__len__`, as CPython's
always-true `TO_BOOL` does. Each of the things that change the answer
must still be seen at a site that already answered: the class gaining
`__bool__` or `__len__`, a base class gaining one, a subclass that
defines one, `__class__` assignment, the method going away again, and a
metaclass's `__bool__` (which instances don't use). An instance whose
last reference the test drops is still finalized.
"""

import os
import subprocess
import sys

if sys.implementation.name == "weavepy" and "WEAVEPY_FRAME_JIT_HOT" not in os.environ:
    env = dict(os.environ, WEAVEPY_FRAME_JIT_HOT="50")
    sys.exit(subprocess.run([sys.executable, __file__, *sys.argv[1:]], env=env).returncode)

N = 3000


class Base:
    pass


class Plain(Base):
    __slots__ = ("x",)

    def __init__(self):
        self.x = 1


class Falsy(Base):
    def __bool__(self):
        return False


class Empty(Base):
    def __len__(self):
        return 0


class Meta(type):
    def __bool__(cls):
        return False


class WithMeta(metaclass=Meta):
    pass


def truths(objs):
    out = []
    for o in objs:
        if o:
            out.append(1)
        else:
            out.append(0)
    return out


def count_true(o, n):
    t = 0
    for _ in range(n):
        if o:
            t += 1
    return t


def holder_truth(h, n):
    t = 0
    for _ in range(n):
        if h.x:
            t += 1
    return t


class Holder:
    def __init__(self, x):
        self.x = x


def check(got, want, what):
    assert got == want, "%s: %r != %r" % (what, got, want)


# A hot site that answers plain instances.
p = Plain()
check(count_true(p, N), N, "plain")

# The class gains `__bool__`, then loses it.
Plain.__bool__ = lambda self: False
check(count_true(p, N), 0, "class gained __bool__")
del Plain.__bool__
check(count_true(p, N), N, "class lost __bool__")

# A base class gains `__len__`.
check(count_true(p, N), N, "before base __len__")
Base.__len__ = lambda self: 0
check(count_true(p, N), 0, "base gained __len__")
del Base.__len__
check(count_true(p, N), N, "base lost __len__")

# Subclasses that define them, mixed at one site.
objs = [Plain(), Falsy(), Empty(), Plain(), Base()] * (N // 5)
check(truths(objs), [1, 0, 0, 1, 1] * (N // 5), "mixed classes")

# `__class__` assignment.
class A:
    pass


class B:
    def __bool__(self):
        return False


a = A()
check(count_true(a, N), N, "before __class__ assignment")
a.__class__ = B
check(count_true(a, N), 0, "after __class__ assignment")
a.__class__ = A
check(count_true(a, N), N, "back to the first class")

# A metaclass's `__bool__` doesn't apply to instances.
w = WithMeta()
check(count_true(w, N), N, "metaclass __bool__")

# Through an attribute load (a fresh reference each time).
h = Holder(Plain())
check(holder_truth(h, N), N, "attribute")
h.x = Falsy()
check(holder_truth(h, N), 0, "attribute now falsy")

# An instance whose last reference the truth test drops is finalized
# (the in-line answer only takes a shared instance).
log = []


class Dying:
    def __del__(self):
        log.append("del")


def fresh_truth(n):
    out = 0
    for _ in range(n):
        if Dying():
            out += 1
    return out


check(fresh_truth(N), N, "fresh instances")
check(len(log), N, "each finalized")

# `__bool__` returning a non-bool raises.
class Bad:
    def __bool__(self):
        return 1


bad = Bad()
try:
    count_true(bad, 1)
except TypeError:
    pass
else:
    raise AssertionError("__bool__ returning int must raise")

print("instance truth sites ok")
