"""Python overrides seen by warm and compiled code.

Each case defines a Python override of something the interpreter has a
fast path for (a metaclass `__call__`, a descriptor, a dunder on a dict
key, a subclass's `__getitem__`, a method replaced on its class), runs it
in loops long enough for every tier to compile them, and prints what it
saw. The output must be CPython's in fresh interpreters with every tier
on, with a low compile threshold, with a low frame-JIT threshold, with
the method tier off and with the compilers off.
"""

import os
import subprocess
import sys
import unittest

PRELUDE = r'''
N = 300


class Check:
    """Loops that call through a method (the shape compiled loops take)."""

    def __init__(self):
        self.log = []

    def row(self):
        self.log.append([])

    def note(self, v):
        if not self.log:
            self.row()
        self.log[-1].append(v)

    def summary(self):
        """Each run of equal iterations once, with its length."""
        out = []
        for v in self.log:
            v = tuple(v)
            if not out or out[-1][0] != v:
                out.append([v, 0])
            out[-1][1] += 1
        return [tuple(x) for x in out]


ck = Check()
'''

CASES = {}

CASES["metaclass_call"] = r'''
class Meta(type):
    def __call__(cls, *args, **kwargs):
        return ("meta", cls.__name__, args)


class A(metaclass=Meta):
    def __init__(self, x):
        self.x = x


class B(A):
    pass


class Wrap(type):
    def __call__(cls, *args):
        inst = super().__call__(*args)
        inst.tag = "wrapped"
        return inst


class W(metaclass=Wrap):
    def __init__(self, x):
        self.x = x


class Late:
    def __init__(self, x):
        self.x = x


def run(n):
    for i in range(n):
        ck.row()
        ck.note(A(1))
        ck.note(B(i % 2))
        w = W(i)
        ck.note((w.x == i, w.tag))


def late(n):
    for i in range(n):
        ck.row()
        ck.note(type(Late(i)).__name__)


run(N)
late(N)
print(ck.summary()[:6])
ck.log.clear()


class LateMeta(type):
    def __call__(cls, *args):
        return "late-meta"


Late2 = LateMeta("Late2", (), {"__init__": lambda self, x: None})
for i in range(N):
    ck.row()
    ck.note(Late2(i))
print(ck.summary())
'''

CASES["class_hooks"] = r'''
class Base:
    seen = []

    def __init_subclass__(cls, tag=None, **kwargs):
        super().__init_subclass__(**kwargs)
        Base.seen.append(tag)


class Named:
    def __set_name__(self, owner, name):
        self.name = name

    def __get__(self, obj, objtype=None):
        if obj is None:
            return ("class", self.name)
        return (self.name, obj.v)


class G:
    def __class_getitem__(cls, item):
        return ("cgi", item)


def make(i):
    class S(Base, tag=i % 3):
        d = Named()
        v = i

    return S().d, S.d


def run(n):
    for i in range(n):
        ck.row()
        inst, cls = make(i)
        ck.note((inst == ("d", i), cls))
        ck.note(G[i % 2])
        ck.note(G[int])


run(N)
print(ck.summary()[:6], Base.seen[:6], len(Base.seen))
'''

CASES["getattribute_getattr"] = r'''
class GA:
    def __init__(self):
        self.x = 1

    def __getattribute__(self, name):
        if name == "x":
            return "intercepted"
        return object.__getattribute__(self, name)

    def method(self):
        return "method"


class GT:
    y = 5

    def __init__(self):
        self.z = 7

    def __getattr__(self, name):
        return "missing:" + name


class Plain:
    def __init__(self):
        self.a = 1


def run(n, ga, gt, p):
    for i in range(n):
        ck.row()
        ck.note((ga.x, ga.method(), gt.y, gt.z, gt.q))
        try:
            ck.note(p.b)
        except AttributeError:
            ck.note("AttributeError")


ga, gt, p = GA(), GT(), Plain()
run(N, ga, gt, p)
Plain.__getattr__ = lambda self, name: "late:" + name
run(N, ga, gt, p)
GA.__getattribute__ = lambda self, name: (
    "replaced" if name == "x" else object.__getattribute__(self, name)
)
run(5, ga, gt, p)
print(ck.summary())
'''

CASES["setattr_delattr"] = r'''
class SA:
    def __setattr__(self, name, value):
        object.__setattr__(self, name, value * 2)

    def __delattr__(self, name):
        object.__setattr__(self, name, "deleted")


class Plain:
    pass


def run(n, s, p):
    for i in range(n):
        ck.row()
        s.x = i
        ck.note(s.x == 2 * i)
        del s.x
        ck.note(s.x)
        p.x = i
        ck.note(p.x == i)


s, p = SA(), Plain()
run(N, s, p)
Plain.__setattr__ = lambda self, name, value: object.__setattr__(self, name, -1)
run(N, s, p)
ck.note(p.x)
print(ck.summary())
'''

CASES["descriptors"] = r'''
class DD:
    def __get__(self, obj, objtype=None):
        return "data" if obj is not None else "cls"

    def __set__(self, obj, value):
        obj.__dict__["store"] = value


class ND:
    def __get__(self, obj, objtype=None):
        return "nondata"


class C:
    dd = DD()
    nd = ND()
    nd2 = ND()


class SP:
    __slots__ = ("_v",)

    def __init__(self):
        self._v = 0

    @property
    def v(self):
        return self._v * 10

    @v.setter
    def v(self, x):
        self._v = x + 1


class MetaD(type):
    @property
    def kind(cls):
        return "meta-property"


class K(metaclass=MetaD):
    kind = "class-attr"


def run(n, c, sp):
    for i in range(n):
        ck.row()
        ck.note((c.dd, c.nd, c.nd2, C.dd, type(C.nd).__name__))
        c.dd = i
        ck.note(c.__dict__["store"] == i)
        sp.v = i
        ck.note(sp.v == (i + 1) * 10)
        ck.note(K.kind)


c = C()
c.__dict__["dd"] = "inst"
c.__dict__["nd"] = "inst"
sp = SP()
run(N, c, sp)
C.nd = property(lambda self: "prop")
run(N, c, sp)
print(ck.summary())
'''

CASES["eq_hash_lt"] = r'''
class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v % 3

    def __eq__(self, other):
        return isinstance(other, K) and self.v % 3 == other.v % 3


class L:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v > other.v

    def __repr__(self):
        return "L%d" % self.v


def run(n):
    for i in range(n):
        ck.row()
        d = {}
        for j in range(i % 7 + 3):
            d[K(j)] = j
        ck.note((len(d), d[K(i + 3)] % 3 == i % 3, K(i) in d, K(i) in [K(i + 6)]))
        xs = [L(j) for j in range(5)]
        ck.note((sorted(xs)[0].v, min(xs).v, max(xs).v))
        xs.sort(key=lambda x: x)
        ck.note(xs[0].v)
        ck.note([K(1), K(2)].index(K(4)))
        ck.note(sorted([3, 1, 2], key=L)[0])


run(N)
K.__eq__ = lambda self, other: False
d = {K(1): 1}
for i in range(N):
    ck.row()
    ck.note(K(1) in d)
print(ck.summary())
'''

CASES["number_protocols"] = r'''
class Ix:
    def __init__(self, v):
        self.v = v

    def __index__(self):
        return self.v


class Conv:
    def __int__(self):
        return 7

    def __float__(self):
        return 2.5

    def __bool__(self):
        return False


class Sized:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n

    def __iter__(self):
        return iter(range(self.n))

    def __contains__(self, x):
        return x == "yes"


def run(n):
    data = list(range(10))
    for i in range(n):
        ck.row()
        ix = Ix(i % 5)
        ck.note((data[ix], data[ix : ix.v + 2], len(range(ix)), hex(ix), [0] * ix == [0] * (i % 5)))
        c = Conv()
        ck.note((int(c), float(c), bool(c), not c, "t" if c else "f"))
        s = Sized(i % 4)
        ck.note((len(s), bool(s), list(s), sum(s), "yes" in s, 1 in s))


run(N)
print(ck.summary()[:30], len(ck.log))
'''

CASES["subclass_getitem"] = r'''
class LL(list):
    def __getitem__(self, i):
        return ("LL", list.__getitem__(self, i))


class DS(dict):
    def __getitem__(self, k):
        return ("DS", dict.__getitem__(self, k))


class DM(dict):
    def __missing__(self, k):
        return ("missing", k)


class SS(str):
    def __getitem__(self, i):
        return "SS"


class I(int):
    def __radd__(self, other):
        return "radd"

    def __add__(self, other):
        return "add"


class F(float):
    def __rmul__(self, other):
        return "rmul"


class St(str):
    def __radd__(self, other):
        return "st-radd"


def run(n, ll, ds, dm, ss):
    for i in range(n):
        ck.row()
        ck.note((ll[0], ll[-1], ds["a"], dm["a"], dm["b"], ss[0]))
        ck.note((1 + I(2), I(2) + 1, 2 * F(1.0), "x" + St("y"), i + I(1)))


run(N, LL([1, 2]), DS(a=1), DM(a=1), SS("abc"))
LL.__getitem__ = lambda self, i: "patched"
run(5, LL([1, 2]), DS(a=1), DM(a=1), SS("abc"))
print(ck.summary())
'''

CASES["context_managers"] = r'''
class Suppress:
    def __init__(self):
        self.entered = 0

    def __enter__(self):
        self.entered += 1
        return self

    def __exit__(self, et, ev, tb):
        return et is ValueError


class Swallow:
    def __enter__(self):
        return "value"

    def __exit__(self, *a):
        return 1


def run(n, s):
    for i in range(n):
        ck.row()
        with s as v:
            if i % 2:
                raise ValueError(i)
            ck.note(v is s)
        with Swallow() as v:
            ck.note(v)
            raise KeyError(i)
        try:
            with s:
                raise TypeError(i)
        except TypeError:
            ck.note("TypeError")


s = Suppress()
run(N, s)
print(ck.summary()[:8], s.entered)
'''

CASES["del_and_class_assign"] = r'''
dead = []


class D:
    def __init__(self, i):
        self.i = i

    def __del__(self):
        dead.append(self.i)


class A:
    def who(self):
        return "A"


class B:
    def who(self):
        return "B"


def run(n):
    for i in range(n):
        ck.row()
        x = D(i)
        x = None
        ck.note(dead[-1] == i)
        D(i + 1000)
        ck.note(dead[-1] == i + 1000)
        v = D(i + 2000).i
        ck.note(dead[-1] == v)
        a = A()
        ck.note(a.who())
        a.__class__ = B
        ck.note(a.who())
        a.__class__ = A


run(N)
print(ck.summary()[:4], len(dead))
'''

CASES["monkeypatch"] = r'''
class C:
    def m(self):
        return "old"

    def __len__(self):
        return 1


class Sub(C):
    pass


def f():
    return "f-old"


def run(n, o, s):
    for i in range(n):
        ck.row()
        ck.note((o.m(), C.m(o), s.m(), f(), len(o), len(s)))


o, s = C(), Sub()
run(N, o, s)
C.m = lambda self: "new"
run(N, o, s)
C.__len__ = lambda self: 2
run(N, o, s)
o.m = lambda: "instance"
run(N, o, s)


def f():
    return "f-new"


run(N, o, s)
del o.m
del C.__len__
try:
    run(1, o, s)
except TypeError:
    ck.note("TypeError")
print(ck.summary())
'''

CASES["container_subclass_protocols"] = r'''
class LI(list):
    def __iter__(self):
        return iter(["it"])

    def __len__(self):
        return 42

    def __contains__(self, x):
        return x == "c"


class TI(tuple):
    def __iter__(self):
        return iter(("t1", "t2"))


class DI(dict):
    def __iter__(self):
        return iter(["k"])

    def __contains__(self, k):
        return k == "c"

    def __setitem__(self, k, v):
        dict.__setitem__(self, k, ("set", v))

    def get(self, k, default=None):
        return "get-override"


class HS(str):
    def __hash__(self):
        return 1

    def __eq__(self, other):
        return isinstance(other, str) and other.lower() == self.lower()


def run(n, li, ti, di):
    for i in range(n):
        ck.row()
        a, b = ti
        d = {HS("Key"): i}
        di["x"] = i
        ck.note((list(li), [x for x in li], len(li), "c" in li, 5 in li, sum(1 for _ in li),
                 (a, b), list(ti), tuple(ti),
                 list(di), "c" in di, "x" in di, di["x"] == ("set", i), di.get("x"),
                 d.get("key") == i, HS("KEY") in d, sorted(li)))


run(N, LI([1, 2]), TI((1, 2)), DI())
print(ck.summary())
'''

CASES["operators_and_conversions"] = r'''
class V:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return "eq"

    def __ne__(self, other):
        return "ne"

    def __lt__(self, other):
        return "lt"

    def __neg__(self):
        return "neg"

    def __abs__(self):
        return "abs"

    def __invert__(self):
        return "inv"

    def __mul__(self, other):
        return "mul"

    def __rsub__(self, other):
        return "rsub"

    def __iadd__(self, other):
        self.v += other
        return self

    def __str__(self):
        return "str-V"

    def __format__(self, spec):
        return "fmt-" + spec

    def __repr__(self):
        return "repr-V"


class LA(list):
    def __iadd__(self, other):
        return "iadd"

    def __add__(self, other):
        return "add"


class IE(int):
    def __eq__(self, other):
        return True

    def __hash__(self):
        return 0

    def __bool__(self):
        return False


def run(n):
    for i in range(n):
        ck.row()
        v = V(i)
        w = v
        w += 2
        la = LA()
        la += [1]
        ck.note((v == 1, v != 1, v < 1, 1 > v, -v, abs(v), ~v, v * 2, 3 - v, w is v and v.v == i + 2,
                 str(v), f"{v}", f"{v:>3}", "%s" % v, repr([v]), la, LA() + [1],
                 IE(5) == 6, bool(IE(5)), not IE(5), [IE(1)].count(9), {IE(1): "a"}.get(7)))


run(N)
V.__neg__ = lambda self: "neg2"
run(3)
print(ck.summary())
'''

CASES["patching_dunders_hot"] = r'''
class P:
    def __init__(self, v):
        self.v = v

    def __add__(self, other):
        return "add1"

    def __getitem__(self, k):
        return "item1"

    def __call__(self, x):
        return "call1"

    def __iter__(self):
        return iter([1])

    def __bool__(self):
        return True


class Q(P):
    pass


def run(n, p, q):
    for i in range(n):
        ck.row()
        ck.note((p + 1, q + 1, p[0], q[0], p(1), q(1), list(p), list(q), bool(p), bool(q),
                 "t" if q else "f", type(P(i)).__name__, P(i).v == i))


p, q = P(0), Q(0)
run(N, p, q)
P.__add__ = lambda self, other: "add2"
P.__getitem__ = lambda self, k: "item2"
P.__call__ = lambda self, x: "call2"
P.__iter__ = lambda self: iter([2])
P.__bool__ = lambda self: False
run(N, p, q)
Q.__add__ = lambda self, other: "add3"
del P.__bool__
run(N, p, q)
P.__init__ = lambda self, v: setattr(self, "v", -v)
run(N, p, q)
print(ck.summary())
'''

CASES["instancecheck_and_metaclass_attrs"] = r'''
class Meta(type):
    def __instancecheck__(cls, inst):
        return inst == "yes"

    def __subclasscheck__(cls, sub):
        return sub is int

    def __getattr__(cls, name):
        return "meta:" + name

    def __len__(cls):
        return 9

    def __iter__(cls):
        return iter("ab")


class M(metaclass=Meta):
    real = "real"


class Gen:
    def __getitem__(self, i):
        if i >= 3:
            raise IndexError
        return i * 10


def run(n):
    for i in range(n):
        ck.row()
        ck.note((isinstance("yes", M), isinstance(1, M), issubclass(int, M), issubclass(str, M),
                 M.real, M.missing, len(M), list(M), list(Gen()), 20 in Gen(), sum(Gen())))


run(N)
print(ck.summary())
'''

CASES["slots_and_properties_patched"] = r'''
class S:
    __slots__ = ("a", "b")

    def __init__(self, a):
        self.a = a
        self.b = a * 2


class T(S):
    __slots__ = ("c",)

    def __init__(self, a):
        super().__init__(a)
        self.c = -a


def run(n):
    for i in range(n):
        ck.row()
        s = S(i)
        t = T(i)
        ck.note((s.a == i, s.b == 2 * i, t.c == -i, t.a == i))


run(N)
S.a_view = property(lambda self: "view")
S.b = property(lambda self: "shadowed-b", lambda self, v: None)
run(N)
print(ck.summary())
'''

CASES["late_hooks_on_warm_classes"] = r'''
class Base:
    pass


class Store(Base):
    def __init__(self, a, b):
        self.a = a
        self.b = b


class Getter:
    def __init__(self, v):
        self._v = v

    @property
    def v(self):
        return self._v


def run(n):
    for i in range(n):
        ck.row()
        s = Store(i, -i)
        g = Getter(i)
        ck.note((s.a == i, s.b == -i, sorted(vars(s)), g.v == i))


run(N)
Base.__setattr__ = lambda self, name, value: object.__setattr__(self, name, ("set", value))
run(N)
del Base.__setattr__
Store.b = property(lambda self: "prop-b", lambda self, v: None)
run(N)
Getter.v = property(lambda self: "new-getter")
run(N)
print(ck.summary())
'''

CASES["function_mutation"] = r'''
def leaf(x, y=1):
    return x + y


def other(x, y=1):
    return x * 100 + y


def with_kw(x, *, k=2):
    return x * k


def make(c):
    def inner(x):
        return x + c

    return inner


add = make(10)


def run(n):
    for i in range(n):
        ck.row()
        ck.note((leaf(1), leaf(1, 2), with_kw(3), add(1)))


run(N)
leaf.__defaults__ = (5,)
run(N)
leaf.__code__ = other.__code__
run(N)
with_kw.__kwdefaults__ = {"k": 7}
run(N)
add.__closure__[0].cell_contents = 20
run(N)
print(ck.summary())
'''

CASES["globals_and_builtins_shadowing"] = r'''
import math


def run(n, xs):
    for i in range(n):
        ck.row()
        ck.note((len(xs), abs(-3), math.floor(2.5), isinstance(1, int), str(1)))


xs = [1, 2]
run(N, xs)
len = lambda x: "len!"
run(N, xs)
math.floor = lambda x: "floor!"
run(N, xs)
import builtins
builtins.abs = lambda x: "abs!"
run(N, xs)
del len
isinstance = lambda a, b: "isinstance!"
run(N, xs)
builtins.str = lambda x: "str!"
run(N, xs)
print(ck.summary())
'''

CASES["bases_and_hash_changes"] = r'''
class A:
    def who(self):
        return "A"

    tag = "A-tag"


class B:
    def who(self):
        return "B"

    tag = "B-tag"


class C(A):
    pass


class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return self.v == other.v


class DM(dict):
    pass


def run(n, c, dm):
    for i in range(n):
        ck.row()
        try:
            h = {K(1): 1}[K(1)]
        except TypeError:
            h = "unhashable"
        ck.note((c.who(), c.tag, C.tag, h, dm["q"] if "q" in dm else dm.get("q", "absent")))
        try:
            ck.note(dm["zz"])
        except KeyError:
            ck.note("KeyError")


c, dm = C(), DM()
run(N, c, dm)
C.__bases__ = (B,)
run(N, c, dm)
K.__hash__ = None
run(N, c, dm)
DM.__missing__ = lambda self, k: "missing:" + k
run(N, c, dm)
print(ck.summary())
'''

CASES["instance_dict_changes"] = r'''
class C:
    x = "class-x"

    def __init__(self):
        self.y = 1

    def m(self):
        return "method"


def run(n, o):
    for i in range(n):
        ck.row()
        ck.note((o.x, o.y, o.m() if callable(o.m) else o.m))


o = C()
run(N, o)
vars(o)["x"] = "inst-x"
run(N, o)
o.__dict__["m"] = "not-callable"
run(N, o)
o.__dict__ = {"y": 2}
run(N, o)
del C.x
try:
    run(1, o)
except AttributeError:
    ck.note("AttributeError")
print(ck.summary())
'''

CASES["sort_and_membership_patched"] = r'''
class L:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v < other.v


class E:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return self.v == other.v

    __hash__ = object.__hash__


def run(n):
    for i in range(n):
        ck.row()
        xs = [L(3), L(1), L(2)]
        es = [E(1), E(2)]
        ck.note(([x.v for x in sorted(xs)], min(xs).v, E(2) in es, es.count(E(1)), E(3) in set(es)))


run(N)
L.__lt__ = lambda self, other: self.v > other.v
E.__eq__ = lambda self, other: True
run(N)
print(ck.summary())
'''

CASES["python_new"] = r'''
class R:
    def __new__(cls, x):
        if x:
            return object.__new__(C)
        return object.__new__(cls)

    def __init__(self, x):
        self.ran = x


class C(R):
    pass


class K:
    def __new__(cls, x):
        inst = object.__new__(cls)
        inst.v = x * 2
        return inst

    def __init__(self, x):
        self.w = x


class Late:
    def __init__(self, x):
        self.x = x


class T:
    def eq(self, a, b):
        return a == b

    def run(self, n):
        for i in range(n):
            ck.row()
            c = R(1)
            k = K(i)
            p = Late(i)
            ck.note((type(c).__name__, self.eq(c.ran, 1), (k.v, k.w) == (2 * i, i),
                     getattr(p, "tag", None)))


t = T()
t.run(N)


def new(cls, x):
    inst = object.__new__(cls)
    inst.tag = "new"
    return inst


Late.__new__ = new
t.run(N)
print(ck.summary())
'''

# Loops the method tier compiles: a run of integer work (which its
# policy wants) around single-operation statements that reach a Python
# override (see `t2_case`). Each entry is a whole program.
T2_PRELUDE = r'''
N = 400


class Ck:
    def __init__(self):
        self.log = []

    def note(self, v):
        self.log.append(v)

    def summary(self):
        out = []
        for v in self.log:
            if not out or out[-1][0] != v:
                out.append([v, 0])
            out[-1][1] += 1
        return [tuple(x) for x in out]


ck = Ck()
'''

T2_LOOP = r'''
class Runner:
    def run(self, n, x0, x1, x2):
        acc = 0
        for i in range(n):
            acc = acc + i
            acc = acc * 3
            acc = acc % 1000003
            acc = acc - 7
            acc = acc + 11
            acc = acc ^ 5
            acc = acc + 2 * i
            acc = acc % 999983
{body}
            ck.note(({names},))
        return acc


r = Runner()
'''


def t2_case(setup, stmts, after):
    body = "\n".join("            v%d = %s" % (k, s) for k, s in enumerate(stmts))
    names = ", ".join("v%d" % k for k in range(len(stmts)))
    return T2_PRELUDE + setup + T2_LOOP.format(body=body, names=names) + after + "\nprint(ck.summary())\n"


WHOLE = {}

WHOLE["t2_python_new"] = t2_case(r'''
class R:
    def __new__(cls, x):
        if x:
            return object.__new__(C)
        return object.__new__(cls)

    def __init__(self, x):
        self.ran = x


class C(R):
    pass


class K:
    def __new__(cls, x):
        inst = object.__new__(cls)
        inst.v = 5
        return inst

    def __init__(self, x):
        self.w = x


class Late:
    def __init__(self, x):
        self.x = x
''', ["type(R(1)) is C", "K(i).v", "isinstance(Late(i), Late)", "type(Late(i)).__name__"], r'''
r.run(N, 0, 0, 0)
Late.__new__ = lambda cls, x: "late-new"
r.run(N, 0, 0, 0)
''')

WHOLE["t2_metaclass_call"] = t2_case(r'''
class Meta(type):
    def __call__(cls, *args):
        return "meta"


class A(metaclass=Meta):
    def __init__(self, x):
        self.x = x


class P:
    def __init__(self, x):
        self.x = x
''', ["A(i)", "P(i).x == i"], r'''
r.run(N, 0, 0, 0)
''')

WHOLE["t2_attr_overrides"] = t2_case(r'''
class GA:
    def __init__(self):
        self.x = 1

    def __getattribute__(self, name):
        if name == "x":
            return "intercepted"
        return object.__getattribute__(self, name)


class GT:
    def __getattr__(self, name):
        return "missing"


class SA:
    def __setattr__(self, name, value):
        object.__setattr__(self, name, value + 1000)


class Pl:
    def __init__(self):
        self.a = 1


ga, gt, sa, pl = GA(), GT(), SA(), Pl()
''', ["ga.x", "gt.zz", "sa.__setattr__('q', i)", "sa.q - i", "pl.a"], r'''
r.run(N, 0, 0, 0)
Pl.a = property(lambda self: "prop-a")
r.run(N, 0, 0, 0)
''')

WHOLE["t2_dunders"] = t2_case(r'''
class V:
    def __init__(self, v):
        self.v = v

    def __add__(self, o):
        return "add"

    def __lt__(self, o):
        return "lt"

    def __eq__(self, o):
        return "eq"

    def __len__(self):
        return 3

    def __getitem__(self, k):
        return "item"

    def __bool__(self):
        return False

    def __index__(self):
        return 2

    def __hash__(self):
        return 1


class I(int):
    def __add__(self, o):
        return "int-add"

    def __radd__(self, o):
        return "int-radd"


class LL(list):
    def __getitem__(self, k):
        return "ll-item"

    def __len__(self):
        return 99


class DM(dict):
    def __missing__(self, k):
        return "missing"


v, ii, ll, dm, data = V(1), I(3), LL([1, 2, 3]), DM(), [10, 20, 30]
''', ["v + 1", "v < 1", "v == 1", "len(v)", "v[0]", "not v", "data[v]", "ii + 1", "1 + ii",
      "ll[0]", "len(ll)", "dm['k']", "i + ii"], r'''
r.run(N, 0, 0, 0)
V.__add__ = lambda self, o: "add2"
V.__len__ = lambda self: 4
LL.__getitem__ = lambda self, k: "ll-item2"
r.run(N, 0, 0, 0)
''')

WHOLE["t2_methods_patched"] = t2_case(r'''
class C:
    def m(self, x):
        return "m1"

    def __call__(self, x):
        return "called"


class S(C):
    pass


def f(x):
    return x * 2


o, s = C(), S()
''', ["o.m(i)", "s.m(i)", "C.m(o, 1)", "o(1)", "f(3)"], r'''
r.run(N, 0, 0, 0)
C.m = lambda self, x: "patched"
r.run(N, 0, 0, 0)
o.m = lambda x: "instance"
r.run(N, 0, 0, 0)


def f(x):
    return "f2"


r.run(N, 0, 0, 0)
C.__call__ = lambda self, x: "called2"
r.run(N, 0, 0, 0)
''')

WHOLE["t2_descriptors"] = t2_case(r'''
class DD:
    def __get__(self, obj, objtype=None):
        return "data"

    def __set__(self, obj, value):
        pass


class ND:
    def __get__(self, obj, objtype=None):
        return "nondata"


class C:
    dd = DD()
    nd = ND()

    def __init__(self):
        self.__dict__["dd"] = "inst"
        self.y = 3


c = C()
''', ["c.dd", "c.nd", "c.y", "C.nd"], r'''
r.run(N, 0, 0, 0)
C.y = DD()
r.run(N, 0, 0, 0)
''')

WHOLE["t2_with_and_class_assign"] = t2_case(r'''
class Sup:
    def __enter__(self):
        return "entered"

    def __exit__(self, *a):
        return True


class A:
    def who(self):
        return "A"


class B:
    def who(self):
        return "B"


sup, a = Sup(), A()
''', ["sup.__enter__()", "a.who()"], r'''
r.run(N, 0, 0, 0)
a.__class__ = B
r.run(N, 0, 0, 0)
''')

WHOLE["t2_keys_and_sorting"] = t2_case(r'''
class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v % 3

    def __eq__(self, o):
        return isinstance(o, K) and self.v % 3 == o.v % 3


class L:
    def __init__(self, v):
        self.v = v

    def __lt__(self, o):
        return self.v > o.v


d = {K(0): "zero", K(1): "one", K(2): "two"}
ls = [L(1), L(3), L(2)]
ks = [K(1), K(2)]
''', ["d.get(K(i)) is None", "K(i) in d", "K(4) in ks", "ks.count(K(5))", "sorted(ls)[0].v", "min(ls).v",
      "max(ls).v", "d.get(K(i + 1)) is None"], r'''
r.run(N, 0, 0, 0)
L.__lt__ = lambda self, o: self.v < o.v
K.__eq__ = lambda self, o: False
r.run(N, 0, 0, 0)
''')

WHOLE["t2_conversions"] = t2_case(r'''
class Conv:
    def __int__(self):
        return 7

    def __float__(self):
        return 2.5

    def __index__(self):
        return 1

    def __len__(self):
        return 4

    def __iter__(self):
        return iter([5, 6])

    def __contains__(self, x):
        return True

    def __bool__(self):
        return True


cv = Conv()
data = [10, 20, 30]
''', ["int(cv)", "float(cv)", "data[cv]", "len(cv)", "list(cv)", "sum(cv)", "3 in cv",
      "bool(cv)", "range(cv)[0]", "hex(cv)"], r'''
r.run(N, 0, 0, 0)
Conv.__index__ = lambda self: 2
Conv.__bool__ = lambda self: False
Conv.__int__ = lambda self: 8
r.run(N, 0, 0, 0)
''')

WHOLE["t2_subclass_containers"] = t2_case(r'''
class SS(str):
    def __getitem__(self, i):
        return "ss-item"

    def __len__(self):
        return 77

    def __add__(self, o):
        return "ss-add"


class DS(dict):
    def __getitem__(self, k):
        return "ds-item"

    def __contains__(self, k):
        return False


class TS(tuple):
    def __getitem__(self, i):
        return "ts-item"

    def __len__(self):
        return 5


ss, ds, ts = SS("abc"), DS(a=1), TS((1, 2))
''', ["ss[0]", "len(ss)", "ss + 'x'", "ds['a']", "'a' in ds", "ts[0]", "len(ts)", "ss.upper()"], r'''
r.run(N, 0, 0, 0)
SS.__getitem__ = lambda self, i: "ss-item2"
DS.__getitem__ = lambda self, k: "ds-item2"
r.run(N, 0, 0, 0)
''')

WHOLE["t2_del_and_globals"] = t2_case(r'''
dead = []


class D:
    def __init__(self, i):
        self.i = i

    def __del__(self):
        dead.append(self.i)


def helper(x):
    return "h1"
''', ["D(i).i == i", "dead[-1] == i", "helper(1)", "abs(-2)"], r'''
r.run(N, 0, 0, 0)


def helper(x):
    return "h2"


abs = lambda x: "abs2"
r.run(N, 0, 0, 0)
''')

WHOLE["t2_class_attrs_and_slots"] = t2_case(r'''
class C:
    tag = "t1"
    __slots__ = ("a",)

    def __init__(self, a):
        self.a = a

    @property
    def view(self):
        return "v1"


class Sub(C):
    __slots__ = ()


c, sc = C(1), Sub(2)
''', ["c.tag", "sc.tag", "c.a", "sc.a", "c.view", "sc.view", "C.tag"], r'''
r.run(N, 0, 0, 0)
C.tag = "t2"
C.view = property(lambda self: "v2")
r.run(N, 0, 0, 0)
Sub.tag = "sub-t"
r.run(N, 0, 0, 0)
''')


WHOLE["t2_finalizer_timing"] = t2_case(r'''
dead = []


class D:
    def __init__(self, i):
        self.i = i

    def __del__(self):
        dead.append(self.i)


def make(i):
    return D(i)
''', ["D(i).i == i", "dead[-1] == i", "make(i).i == i", "dead[-1] == i", "len(dead) == 2 * i + 2"], r'''
r.run(N, 0, 0, 0)
''')



ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    'metaclass_call': "[((('meta', 'A', (1,)), ('meta', 'B', (0,)), (True, 'wrapped')), 1), ((('meta', 'A', (1,)), ('meta', 'B', (1,)), (True, 'wrapped')), 1), ((('meta', 'A', (1,)), ('meta', 'B', (0,)), (True, 'wrapped')), 1), ((('meta', 'A', (1,)), ('meta', 'B', (1,)), (True, 'wrapped')), 1), ((('meta', 'A', (1,)), ('meta', 'B', (0,)), (True, 'wrapped')), 1), ((('meta', 'A', (1,)), ('meta', 'B', (1,)), (True, 'wrapped')), 1)]\n[(('late-meta',), 300)]\n",
    'class_hooks': "[(((True, ('class', 'd')), ('cgi', 0), ('cgi', <class 'int'>)), 1), (((True, ('class', 'd')), ('cgi', 1), ('cgi', <class 'int'>)), 1), (((True, ('class', 'd')), ('cgi', 0), ('cgi', <class 'int'>)), 1), (((True, ('class', 'd')), ('cgi', 1), ('cgi', <class 'int'>)), 1), (((True, ('class', 'd')), ('cgi', 0), ('cgi', <class 'int'>)), 1), (((True, ('class', 'd')), ('cgi', 1), ('cgi', <class 'int'>)), 1)] [0, 1, 2, 0, 1, 2] 300\n",
    'getattribute_getattr': "[((('intercepted', 'method', 5, 7, 'missing:q'), 'AttributeError'), 300), ((('intercepted', 'method', 5, 7, 'missing:q'), 'late:b'), 300), ((('replaced', 'method', 5, 7, 'missing:q'), 'late:b'), 5)]\n",
    'setattr_delattr': "[((True, 'deleted', True), 300), ((True, 'deleted', False), 299), ((True, 'deleted', False, -1), 1)]\n",
    'descriptors': "[((('data', 'inst', 'nondata', 'cls', 'str'), True, True, 'meta-property'), 300), ((('data', 'prop', 'nondata', 'cls', 'property'), True, True, 'meta-property'), 300)]\n",
    'eq_hash_lt': '[(((3, True, True, True), (4, 4, 0), 4, 0, 3), 300), ((False,), 300)]\n',
    'number_protocols': "[(((0, [0, 1], 0, '0x0', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((1, [1, 2], 1, '0x1', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((2, [2, 3], 2, '0x2', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((3, [3, 4], 3, '0x3', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((4, [4, 5], 4, '0x4', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((0, [0, 1], 0, '0x0', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((1, [1, 2], 1, '0x1', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((2, [2, 3], 2, '0x2', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((3, [3, 4], 3, '0x3', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((4, [4, 5], 4, '0x4', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((0, [0, 1], 0, '0x0', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((1, [1, 2], 1, '0x1', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((2, [2, 3], 2, '0x2', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((3, [3, 4], 3, '0x3', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((4, [4, 5], 4, '0x4', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((0, [0, 1], 0, '0x0', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((1, [1, 2], 1, '0x1', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((2, [2, 3], 2, '0x2', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((3, [3, 4], 3, '0x3', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((4, [4, 5], 4, '0x4', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((0, [0, 1], 0, '0x0', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((1, [1, 2], 1, '0x1', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((2, [2, 3], 2, '0x2', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((3, [3, 4], 3, '0x3', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((4, [4, 5], 4, '0x4', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((0, [0, 1], 0, '0x0', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1), (((1, [1, 2], 1, '0x1', True), (7, 2.5, False, True, 'f'), (2, True, [0, 1], 1, True, False)), 1), (((2, [2, 3], 2, '0x2', True), (7, 2.5, False, True, 'f'), (3, True, [0, 1, 2], 3, True, False)), 1), (((3, [3, 4], 3, '0x3', True), (7, 2.5, False, True, 'f'), (0, False, [], 0, True, False)), 1), (((4, [4, 5], 4, '0x4', True), (7, 2.5, False, True, 'f'), (1, True, [0], 0, True, False)), 1)] 300\n",
    'subclass_getitem': "[(((('LL', 1), ('LL', 2), ('DS', 1), 1, ('missing', 'b'), 'SS'), ('radd', 'add', 'rmul', 'st-radd', 'radd')), 300), ((('patched', 'patched', ('DS', 1), 1, ('missing', 'b'), 'SS'), ('radd', 'add', 'rmul', 'st-radd', 'radd')), 5)]\n",
    'context_managers': "[((True, 'value', 'TypeError'), 1), (('value', 'TypeError'), 1), ((True, 'value', 'TypeError'), 1), (('value', 'TypeError'), 1), ((True, 'value', 'TypeError'), 1), (('value', 'TypeError'), 1), ((True, 'value', 'TypeError'), 1), (('value', 'TypeError'), 1)] 600\n",
    'del_and_class_assign': "[((True, True, True, 'A', 'B'), 300)] 900\n",
    'monkeypatch': "[((('old', 'old', 'old', 'f-old', 1, 1),), 300), ((('new', 'new', 'new', 'f-old', 1, 1),), 300), ((('new', 'new', 'new', 'f-old', 2, 2),), 300), ((('instance', 'new', 'new', 'f-old', 2, 2),), 300), ((('instance', 'new', 'new', 'f-new', 2, 2),), 300), (('TypeError',), 1)]\n",
    'container_subclass_protocols': "[(((['it'], ['it'], 42, True, False, 1, ('t1', 't2'), ['t1', 't2'], ('t1', 't2'), ['k'], True, False, True, 'get-override', False, True, ['it']),), 300)]\n",
    'operators_and_conversions': "[((('eq', 'ne', 'lt', 'lt', 'neg', 'abs', 'inv', 'mul', 'rsub', True, 'str-V', 'fmt-', 'fmt->3', 'str-V', '[repr-V]', 'iadd', 'add', True, False, True, 1, None),), 300), ((('eq', 'ne', 'lt', 'lt', 'neg2', 'abs', 'inv', 'mul', 'rsub', True, 'str-V', 'fmt-', 'fmt->3', 'str-V', '[repr-V]', 'iadd', 'add', True, False, True, 1, None),), 3)]\n",
    'patching_dunders_hot': "[((('add1', 'add1', 'item1', 'item1', 'call1', 'call1', [1], [1], True, True, 't', 'P', True),), 300), ((('add2', 'add2', 'item2', 'item2', 'call2', 'call2', [2], [2], False, False, 'f', 'P', True),), 300), ((('add2', 'add3', 'item2', 'item2', 'call2', 'call2', [2], [2], True, True, 't', 'P', True),), 301), ((('add2', 'add3', 'item2', 'item2', 'call2', 'call2', [2], [2], True, True, 't', 'P', False),), 299)]\n",
    'instancecheck_and_metaclass_attrs': "[(((True, False, True, False, 'real', 'meta:missing', 9, ['a', 'b'], [0, 10, 20], True, 30),), 300)]\n",
    'slots_and_properties_patched': '[(((True, True, True, True),), 300), (((True, False, True, True),), 300)]\n',
    'late_hooks_on_warm_classes': "[(((True, True, ['a', 'b'], True),), 300), (((False, False, ['a', 'b'], True),), 300), (((True, False, ['a'], True),), 300), (((True, False, ['a'], False),), 300)]\n",
    'function_mutation': '[(((2, 3, 6, 11),), 300), (((6, 3, 6, 11),), 300), (((105, 102, 6, 11),), 300), (((105, 102, 21, 11),), 300), (((105, 102, 21, 21),), 300)]\n',
    'globals_and_builtins_shadowing': "[(((2, 3, 2, True, '1'),), 300), ((('len!', 3, 2, True, '1'),), 300), ((('len!', 3, 'floor!', True, '1'),), 300), ((('len!', 'abs!', 'floor!', True, '1'),), 300), (((2, 'abs!', 'floor!', 'isinstance!', '1'),), 300), (((2, 'abs!', 'floor!', 'isinstance!', 'str!'),), 300)]\n",
    'bases_and_hash_changes': "[((('A', 'A-tag', 'A-tag', 1, 'absent'), 'KeyError'), 300), ((('B', 'B-tag', 'B-tag', 1, 'absent'), 'KeyError'), 300), ((('B', 'B-tag', 'B-tag', 'unhashable', 'absent'), 'KeyError'), 300), ((('B', 'B-tag', 'B-tag', 'unhashable', 'absent'), 'missing:zz'), 300)]\n",
    'instance_dict_changes': "[((('class-x', 1, 'method'),), 300), ((('inst-x', 1, 'method'),), 300), ((('inst-x', 1, 'not-callable'),), 300), ((('class-x', 2, 'method'),), 300), (('AttributeError',), 1)]\n",
    'sort_and_membership_patched': '[((([1, 2, 3], 1, True, 1, False),), 300), ((([3, 2, 1], 3, True, 2, False),), 300)]\n',
    'python_new': "[((('C', True, True, None),), 300), ((('C', True, True, 'new'),), 300)]\n",
    't2_python_new': "[((True, 5, True, 'Late'), 400), ((True, 5, False, 'str'), 400)]\n",
    't2_metaclass_call': "[(('meta', True), 400)]\n",
    't2_attr_overrides': "[(('intercepted', 'missing', None, 1000, 1), 400), (('intercepted', 'missing', None, 1000, 'prop-a'), 400)]\n",
    't2_dunders': "[(('add', 'lt', 'eq', 3, 'item', True, 30, 'int-add', 'int-radd', 'll-item', 99, 'missing', 'int-radd'), 400), (('add2', 'lt', 'eq', 4, 'item', True, 30, 'int-add', 'int-radd', 'll-item2', 99, 'missing', 'int-radd'), 400)]\n",
    't2_methods_patched': "[(('m1', 'm1', 'm1', 'called', 6), 400), (('patched', 'patched', 'patched', 'called', 6), 400), (('instance', 'patched', 'patched', 'called', 6), 400), (('instance', 'patched', 'patched', 'called', 'f2'), 400), (('instance', 'patched', 'patched', 'called2', 'f2'), 400)]\n",
    't2_descriptors': "[(('data', 'nondata', 3, 'nondata'), 400), (('data', 'nondata', 'data', 'nondata'), 400)]\n",
    't2_with_and_class_assign': "[(('entered', 'A'), 400), (('entered', 'B'), 400)]\n",
    't2_keys_and_sorting': '[((False, True, True, 1, 3, 3, 1, False), 400), ((True, False, False, 0, 1, 1, 3, True), 400)]\n',
    't2_conversions': "[((7, 2.5, 20, 4, [5, 6], 11, True, True, 0, '0x1'), 400), ((8, 2.5, 30, 4, [5, 6], 11, True, False, 0, '0x2'), 400)]\n",
    't2_subclass_containers': "[(('ss-item', 77, 'ss-add', 'ds-item', False, 'ts-item', 5, 'ABC'), 400), (('ss-item2', 77, 'ss-add', 'ds-item2', False, 'ts-item', 5, 'ABC'), 400)]\n",
    't2_del_and_globals': "[((True, True, 'h1', 2), 400), ((True, True, 'h2', 'abs2'), 400)]\n",
    't2_class_attrs_and_slots': "[(('t1', 't1', 1, 2, 'v1', 'v1', 't1'), 400), (('t2', 't2', 1, 2, 'v2', 'v2', 't2'), 400), (('t2', 'sub-t', 1, 2, 'v2', 'v2', 't2'), 400)]\n",
    't2_finalizer_timing': '[((True, True, True, True, True), 400)]\n',
}


def source(name):
    if name in WHOLE:
        return WHOLE[name]
    return PRELUDE + CASES[name]


def run_case(name, extra=None):
    env = dict(os.environ, **(extra or {}))
    result = subprocess.run(
        [sys.executable, "-c", source(name)],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class OverrideFastPathTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in [*CASES, *WHOLE]:
    setattr(OverrideFastPathTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
