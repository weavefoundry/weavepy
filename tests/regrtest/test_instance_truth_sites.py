"""Truth tests of instances keep Python's semantics at hot sites.

Compiled code answers a truth test of an instance in line when its class
(as it stands) has neither `__bool__` nor `__len__`, as CPython's
always-true `TO_BOOL` does. Each of the things that change the answer
must still be seen at a site that already answered: the class gaining
`__bool__` or `__len__`, a base class gaining one, a subclass that
defines one, `__class__` assignment, the method going away again, and a
metaclass's `__bool__` (which instances don't use).

A class's own truth is its metaclass's `__bool__` or `__len__` (then
True), in every boolean context: `if`, `while`, `not`, `and`/`or`,
`bool()`, `any()`/`all()` and `filter(None, ...)`. An instance whose last
reference a truth test holds is finalized at the test, before the next
statement. Under WeavePy the checks run under each of the JIT settings
the override harness uses.
"""

import os
import subprocess
import sys

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50", "WEAVEPY_JIT_SYNC": "1"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

if sys.implementation.name == "weavepy" and "TRUTH_SITES_CHILD" not in os.environ:
    for e in ENVS:
        env = dict(os.environ, TRUTH_SITES_CHILD="1", **e)
        r = subprocess.run([sys.executable, __file__, *sys.argv[1:]], env=env)
        if r.returncode:
            sys.exit("failed under %r" % (e,))
    print("instance truth sites ok")
    sys.exit(0)

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
    prompt = 0
    for _ in range(n):
        log.clear()
        if Dying():
            out += 1
            if log:
                prompt += 1
    return out, prompt


def fresh_not(n):
    prompt = 0
    for _ in range(n):
        log.clear()
        b = not Dying()
        if log and not b:
            prompt += 1
    return prompt


def fresh_bool(n):
    prompt = 0
    for _ in range(n):
        log.clear()
        b = bool(Dying())
        if log and b:
            prompt += 1
    return prompt


def fresh_while(n):
    prompt = 0
    for _ in range(n):
        log.clear()
        while Dying():
            break
        if log:
            prompt += 1
    return prompt


check(fresh_truth(N), (N, N), "finalized at `if`")
check(fresh_not(N), N, "finalized at `not`")
check(fresh_bool(N), N, "finalized at bool()")
check(fresh_while(N), N, "finalized at `while`")


# A class's truth is its metaclass's.
class FalsyMeta(type):
    def __bool__(cls):
        return False


class EmptyMeta(type):
    def __len__(cls):
        return 0


class SizedMeta(type):
    def __len__(cls):
        return 3


class BadMeta(type):
    def __bool__(cls):
        return 1


class F(metaclass=FalsyMeta):
    pass


class E(metaclass=EmptyMeta):
    pass


class S(metaclass=SizedMeta):
    pass


class FSub(F):
    pass


def class_truths(classes, n):
    out = None
    for _ in range(n):
        got = []
        for c in classes:
            if c:
                got.append(1)
            else:
                got.append(0)
            got.append(int(not c))
            got.append(bool(c))
            got.append(c and 1)
            got.append(c or 1)
            k = 0
            while c:
                k += 1
                break
            got.append(k)
        got.append(list(filter(None, classes)))
        got.append(any(classes))
        got.append(all(classes))
        got.append(any([F, E]))
        out = got
    return out


want = []
for c, t in ((F, False), (E, False), (S, True), (FSub, False), (Plain, True)):
    want += [int(t), int(not t), t, 1 if t else c, c if t else 1, int(t)]
want += [[S, Plain], True, False, False]
check(class_truths([F, E, S, FSub, Plain], N), want, "class truth")


# The metaclass gains or loses `__bool__` after the sites ran.
class LateMeta(type):
    pass


class L(metaclass=LateMeta):
    pass


def class_count(c, n):
    t = 0
    for _ in range(n):
        if c:
            t += 1
    return t


check(class_count(L, N), N, "metaclass without __bool__")
LateMeta.__bool__ = lambda cls: False
check(class_count(L, N), 0, "metaclass gained __bool__")
del LateMeta.__bool__
check(class_count(L, N), N, "metaclass lost __bool__")

# A metaclass `__bool__` returning a non-bool raises.
class Worse(metaclass=BadMeta):
    pass


try:
    class_count(Worse, 1)
except TypeError:
    pass
else:
    raise AssertionError("metaclass __bool__ returning int must raise")

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

if "TRUTH_SITES_CHILD" not in os.environ:
    print("instance truth sites ok")
