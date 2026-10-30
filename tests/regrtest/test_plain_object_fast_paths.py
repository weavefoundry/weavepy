"""Operations on plain objects and classes that skip dunder dispatch.

Comparisons that come down to identity, a class's `__mro__`, `__bases__` and
`__module__`, an instance's `__class__`, `getattr` and `hasattr` of a class,
`issubclass` and `isinstance` under a metaclass, and `in` on a dict subclass
all answer without a call when nothing overrides them. Each case runs them
in loops long enough for every tier to compile them, overrides what they
assumed partway through, and prints what it saw. The output must be CPython's
in fresh interpreters with every tier on, with a low compile threshold, with a
low frame-JIT threshold, with the method tier off and with the compilers off.
"""

import os
import subprocess
import sys
import unittest

PRELUDE = r'''
import zlib

N = 300


def show(name, rows):
    text = repr(rows)
    print(name, len(rows), zlib.crc32(text.encode("utf-8", "surrogatepass")), text[:200])
'''

CASES = {}

CASES["identity_compares"] = r'''
import enum


class A:
    pass


class Sub(A):
    pass


class Color(enum.Enum):
    RED = 1
    GREEN = 2


class Meta(type):
    pass


class WithMeta(metaclass=Meta):
    pass


def f():
    pass


def g():
    pass


a, b, s = A(), A(), Sub()
ITEMS = [None, a, b, s, A, Sub, WithMeta, f, g, Color.RED, Color.GREEN, Color, 1, 1.5, True, "a",
         b"a", (1,), [1], {}, {1}, frozenset({1}), 10**30]


def compare_all(x, y):
    return (x == y, x != y, y == x, y != x)


def run():
    rows = []
    for _ in range(N):
        total = []
        for x in ITEMS:
            for y in ITEMS:
                total.append(compare_all(x, y))
        rows.append(sum(sum(t) for t in total))
    return rows


show("identity", run())
for x in (None, a, A, f, Color.RED):
    for name, op in (("<", lambda p, q: p < q), (">=", lambda p, q: p >= q)):
        try:
            print(name, op(x, b))
        except TypeError as e:
            print(name, e)
print(ITEMS[1] == ITEMS[1], ITEMS[1] != ITEMS[1], None == None, None != None)
'''

CASES["numbers_and_bytes"] = r'''
NUMS = [True, False, 0, 1, -1, 2**53, 2**53 + 1, -(2**53) - 1, 1.0, 0.0, -0.0, 0.5, 2.0**53,
        9007199254740993.0, float("inf"), float("-inf"), float("nan"), 2**63, 2**70, 2.0**70]
BYTES = [b"", b"a", b"ab", b"b", b"\xff", b"a\x00"]
SEQS = [(), (1,), (1, 2), (1, 2.0), (1, None), (None, 1), ("a",), [], [1], [1, 2], [None]]


def cmp_all(x, y):
    out = []
    for op in (lambda: x == y, lambda: x != y, lambda: x < y, lambda: x <= y, lambda: x > y,
               lambda: x >= y):
        try:
            out.append(op())
        except TypeError:
            out.append("TypeError")
    return out


def run(items):
    rows = []
    for _ in range(N // 3):
        rows.append([cmp_all(x, y) for x in items for y in items])
    return rows


show("nums", run(NUMS))
show("bytes", run(BYTES))
show("seqs", run(SEQS))
'''

CASES["eq_overrides_appear"] = r'''
class Late:
    pass


class LateNe:
    pass


class Base:
    pass


class Child(Base):
    pass


class MetaLate(type):
    pass


class C1(metaclass=MetaLate):
    pass


class C2(metaclass=MetaLate):
    pass


x, y = Late(), Late()
nx, ny = LateNe(), LateNe()
base, child = Base(), Child()


def probe():
    return (x == y, x != y, x == None, None == x, nx != ny, nx == ny, base == child, child == base,
            base != child, C1 == C2, C1 != C2, C1 == None)


rows = []
for i in range(N):
    if i == 100:
        Late.__eq__ = lambda self, other: "late-eq"
        LateNe.__ne__ = lambda self, other: "late-ne"
        Child.__eq__ = lambda self, other: "child-eq"
        MetaLate.__eq__ = lambda cls, other: "meta-eq"
    if i == 200:
        del Late.__eq__
        del LateNe.__ne__
        del Child.__eq__
        Base.__eq__ = lambda self, other: "base-eq"
        MetaLate.__ne__ = lambda cls, other: "meta-ne"
        del MetaLate.__eq__
    rows.append(probe())
show("overrides", rows)
print(rows[99], rows[100], rows[199], rows[200])
'''

CASES["operands_released"] = r'''
import weakref

log = []


class Fin:
    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        log.append("del " + self.tag)


class Target:
    pass


def make(tag):
    return Fin(tag)


def run():
    for i in range(N):
        log.append(("start", i))
        r = make("left") == None
        log.append(("after", r))
        r = None != make("right")
        log.append(("after", r))
        if make("branch") == 1:
            log.append("no")
        log.append("branched")
        r = [make("in-list")] == None
        log.append(("after-list", r))
        t = Target()
        ref = weakref.ref(t, lambda _: log.append("callback"))
        r = (t == None, ref() is t)
        del t
        log.append(("dropped", r))


run()
print(len(log), log[:24], log[-12:])
'''

CASES["class_getsets"] = r'''
class Base:
    pass


class Other:
    pass


class Child(Base):
    pass


class Grand(Child):
    pass


class Meta(type):
    pass


class WithMeta(Base, metaclass=Meta):
    pass


class MroMeta(type):
    @property
    def __mro__(cls):
        return "meta-mro"

    @property
    def __bases__(cls):
        return "meta-bases"

    __module__ = "meta-module"


class Patched(metaclass=MroMeta):
    pass


class TupleSub(tuple):
    pass


FromSub = type("FromSub", TupleSub((Base,)), {})


class ModInt:
    __module__ = 17


class Pick:
    pass


def names(t):
    return [c.__name__ for c in t]


def probe():
    return (names(Grand.__mro__), names(Grand.__bases__), Grand.__module__, names(WithMeta.__mro__),
            names(WithMeta.__bases__), WithMeta.__module__, Patched.__mro__, Patched.__bases__,
            Patched.__module__, type(FromSub.__bases__).__name__, ModInt.__module__,
            names(int.__mro__), names(int.__bases__), int.__module__, object.__bases__,
            names(Pick.__mro__), Pick.__module__, Grand.__mro__ == Grand.__mro__)


rows = []
for i in range(N):
    if i == 100:
        Child.__bases__ = (Other,)
        Pick.__module__ = "moved"
    if i == 200:
        Child.__bases__ = (Base,)
        Pick.__module__ = "back"
    try:
        rows.append(probe())
    except AttributeError as e:
        rows.append(str(e))
show("getsets", rows)
print(rows[99][0], rows[100][0], rows[200][0])
'''

CASES["instance_class"] = r'''
import weakref


class A:
    pass


class B:
    pass


class Shadow:
    pass


class ClassProp:
    @property
    def __class__(self):
        return int


class GetAttr:
    def __getattribute__(self, name):
        if name == "__class__":
            return str
        return object.__getattribute__(self, name)


class Late:
    pass


a = A()
s = Shadow()
s.__dict__["__class__"] = "shadowed"
late = Late()
target = A()
proxy = weakref.proxy(target)
cp = ClassProp()
ga = GetAttr()


def probe(i):
    return (a.__class__.__name__, s.__class__.__name__, cp.__class__.__name__,
            ga.__class__.__name__, late.__class__, proxy.__class__.__name__, (1).__class__.__name__,
            None.__class__.__name__, A.__class__.__name__, isinstance(cp, int))


rows = []
for i in range(N):
    if i == 100:
        a.__class__ = B
    if i == 200:
        a.__class__ = A
        Late.__getattribute__ = lambda self, name: "patched"
    rows.append(probe(i))
show("class", rows)
print(rows[99], rows[100], rows[200])
'''

CASES["class_getattr"] = r'''
class Descr:
    def __get__(self, obj, owner=None):
        return ("descr", obj is None)


class Data:
    def __get__(self, obj, owner=None):
        return "data"

    def __set__(self, obj, value):
        pass


class Plain:
    answer = 42
    text = "t"
    none = None
    box = [1]
    table = {"k": 1}
    kind = int
    desc = Descr()
    data = Data()
    obj = object()

    def method(self):
        return "m"

    @classmethod
    def cm(cls):
        return cls.__name__

    @staticmethod
    def sm():
        return "s"

    @property
    def prop(self):
        return "p"


class Child(Plain):
    pass


class MetaGet(type):
    meta_value = "meta-value"
    answer = "meta-loses"
    data = Data()

    def __getattr__(cls, name):
        return "getattr:" + name

    def meta_method(cls):
        return "meta-method"


class WithGet(metaclass=MetaGet):
    answer = "class-wins"
    data = "class-data"


class MetaAttrs(type):
    def __getattribute__(cls, name):
        if name == "intercepted":
            return "yes"
        return super().__getattribute__(name)


class WithAttrs(metaclass=MetaAttrs):
    pass


NAMES = ["answer", "text", "none", "box", "table", "kind", "desc", "data", "method", "cm", "sm",
         "prop", "missing", "mro", "__eq__", "__repr__", "__init__", "__new__", "__dict__x",
         "meta_value", "meta_method", "intercepted", "__name__", "__mro__"]
CLASSES = [Plain, Child, WithGet, WithAttrs, int, object, type]


def view(v):
    if callable(v) and not isinstance(v, type):
        return getattr(v, "__name__", "callable")
    if isinstance(v, (int, str, tuple, list, dict, type(None))):
        return repr(v)[:40]
    if isinstance(v, type):
        return v.__name__
    return type(v).__name__


def probe():
    out = []
    for c in CLASSES:
        for n in NAMES:
            d = getattr(c, n, "<default>")
            h = hasattr(c, n)
            try:
                e = view(getattr(c, n))
            except AttributeError as ex:
                e = "AttributeError: " + str(ex)
            out.append((view(d), h, e))
    return out


rows = []
for i in range(N // 3):
    if i == 50:
        Plain.answer = 43
        Plain.late = "late"
        MetaGet.late_meta = "late-meta"
    rows.append(probe())
show("getattr", rows)
print(rows[-1][:13])
'''

CASES["metaclass_checks"] = r'''
import abc
import enum


class Inherit(type):
    pass


class A(metaclass=Inherit):
    pass


class B(A):
    pass


class Unrelated:
    pass


class Hooks(type):
    def __subclasscheck__(cls, sub):
        return sub is int

    def __instancecheck__(cls, obj):
        return obj == 7


class H(metaclass=Hooks):
    pass


class Late(type):
    pass


class L(metaclass=Late):
    pass


class Ab(abc.ABC):
    pass


Ab.register(Unrelated)


class E(enum.Enum):
    X = 1


class F(enum.Flag):
    P = 1


def probe():
    out = [issubclass(B, A), issubclass(Unrelated, A), issubclass(A, B), isinstance(B(), A),
           isinstance(Unrelated(), A), issubclass(int, H), issubclass(str, H), isinstance(7, H),
           isinstance(8, H), issubclass(L, L), issubclass(int, L), isinstance(L(), L),
           isinstance(3, L), issubclass(Unrelated, Ab), isinstance(Unrelated(), Ab),
           issubclass(E, F), issubclass(F, enum.Flag), isinstance(E.X, E), isinstance(F.P, enum.Enum),
           issubclass(B, (int, A)), isinstance(3, (A, int))]
    for bad in (lambda: issubclass(3, A), lambda: issubclass(A, 3), lambda: isinstance(3, 3)):
        try:
            out.append(bad())
        except TypeError as e:
            out.append(str(e))
    return out


rows = []
for i in range(N):
    if i == 100:
        Late.__subclasscheck__ = lambda cls, sub: "truthy"
        Late.__instancecheck__ = lambda cls, obj: 0
    if i == 200:
        del Late.__subclasscheck__
        del Late.__instancecheck__
    rows.append(probe())
show("checks", rows)
print(rows[99][9:13], rows[100][9:13], rows[200][9:13])
'''

CASES["dict_subclass_contains"] = r'''
class D(dict):
    pass


class WithContains(dict):
    def __contains__(self, key):
        return key == "yes"


class Late(dict):
    pass


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        log.append("eq")
        return other == self.v


class Boom:
    def __hash__(self):
        raise ValueError("no hash")


log = []
d = D(a=1, b=2)
d[1] = "int"
d[Key("k")] = "key"
w = WithContains(no=1)
late = Late(a=1)


def probe():
    out = ["a" in d, "z" in d, 1 in d, 2 in d, 1.0 in d, True in d, "k" in d, Key("a") in d,
           "yes" in w, "no" in w, "a" in late, "yes" in late, "a" not in d, "z" not in d]
    try:
        Boom() in d
    except ValueError as e:
        out.append(str(e))
    try:
        [] in d
    except TypeError as e:
        out.append(str(e))
    return out


rows = []
for i in range(N):
    if i == 100:
        Late.__contains__ = lambda self, key: key == "yes"
    if i == 200:
        del Late.__contains__
    rows.append(probe())
show("contains", rows)
print(rows[99], rows[100], rows[200], len(log))
'''

CASES["format_keyerrors"] = r'''
def run():
    out = []
    for i in range(N):
        for f in (lambda: "%(a)s" % {}, lambda: "%(a)s %(b)d" % {"a": 1}, lambda: b"%(a)s" % {},
                  lambda: "{a}".format(), lambda: "{a}".format_map({}), lambda: "{a.b}".format()):
            try:
                out.append(f())
            except KeyError as e:
                out.append((repr(e), e.args, str(e)))
    return out


show("formats", run())
print(run()[:6])
'''

EXPECTED = {
    'identity_compares': "identity 300 1902506595 [1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1058, 1\n< '<' not supported between instances of 'NoneType' and 'A'\n>= '>=' not supported between instances of 'NoneType' and 'A'\n< '<' not supported between instances of 'A' and 'A'\n>= '>=' not supported between instances of 'A' and 'A'\n< '<' not supported between instances of 'type' and 'A'\n>= '>=' not supported between instances of 'type' and 'A'\n< '<' not supported between instances of 'function' and 'A'\n>= '>=' not supported between instances of 'function' and 'A'\n< '<' not supported between instances of 'Color' and 'A'\n>= '>=' not supported between instances of 'Color' and 'A'\nTrue False True False\n",
    'numbers_and_bytes': 'nums 100 485371766 [[[True, False, False, True, False, True], [False, True, False, False, True, True], [False, True, False, False, True, True], [True, False, False, True, False, True], [False, True, False, False, True, \nbytes 100 1296585994 [[[True, False, False, True, False, True], [False, True, True, True, False, False], [False, True, True, True, False, False], [False, True, True, True, False, False], [False, True, True, True, False, F\nseqs 100 1762798722 [[[True, False, False, True, False, True], [False, True, True, True, False, False], [False, True, True, True, False, False], [False, True, True, True, False, False], [False, True, True, True, False, F\n',
    'eq_overrides_appear': "overrides 300 2789300893 [(False, True, False, False, True, False, False, False, True, False, True, False), (False, True, False, False, True, False, False, False, True, False, True, False), (False, True, False, False, True, F\n(False, True, False, False, True, False, False, False, True, False, True, False) ('late-eq', False, 'late-eq', 'late-eq', 'late-ne', False, 'child-eq', 'child-eq', False, 'meta-eq', False, 'meta-eq') ('late-eq', False, 'late-eq', 'late-eq', 'late-ne', False, 'child-eq', 'child-eq', False, 'meta-eq', False, 'meta-eq') (False, True, False, False, True, False, 'base-eq', 'base-eq', False, False, 'meta-ne', False)\n",
    'operands_released': "3300 [('start', 0), 'del left', ('after', False), 'del right', ('after', True), 'del branch', 'branched', 'del in-list', ('after-list', False), 'callback', ('dropped', (False, True)), ('start', 1), 'del left', ('after', False), 'del right', ('after', True), 'del branch', 'branched', 'del in-list', ('after-list', False), 'callback', ('dropped', (False, True)), ('start', 2), 'del left'] [('dropped', (False, True)), ('start', 299), 'del left', ('after', False), 'del right', ('after', True), 'del branch', 'branched', 'del in-list', ('after-list', False), 'callback', ('dropped', (False, True))]\n",
    'class_getsets': "getsets 300 504660815 [(['Grand', 'Child', 'Base', 'object'], ['Child'], '__main__', ['WithMeta', 'Base', 'object'], ['Base'], '__main__', 'meta-mro', 'meta-bases', '__main__', 'TupleSub', 17, ['int', 'object'], ['object']\n['Grand', 'Child', 'Base', 'object'] ['Grand', 'Child', 'Other', 'object'] ['Grand', 'Child', 'Base', 'object']\n",
    'instance_class': "class 300 1299045803 [('A', 'Shadow', 'int', 'str', <class '__main__.Late'>, 'A', 'int', 'NoneType', 'type', True), ('A', 'Shadow', 'int', 'str', <class '__main__.Late'>, 'A', 'int', 'NoneType', 'type', True), ('A', 'Shad\n('A', 'Shadow', 'int', 'str', <class '__main__.Late'>, 'A', 'int', 'NoneType', 'type', True) ('B', 'Shadow', 'int', 'str', <class '__main__.Late'>, 'A', 'int', 'NoneType', 'type', True) ('A', 'Shadow', 'int', 'str', 'patched', 'A', 'int', 'NoneType', 'type', True)\n",
    'class_getattr': 'getattr 100 3406374653 [[(\'42\', True, \'42\'), ("\'t\'", True, "\'t\'"), (\'None\', True, \'None\'), (\'[1]\', True, \'[1]\'), ("{\'k\': 1}", True, "{\'k\': 1}"), (\'int\', True, \'int\'), ("(\'descr\', True)", True, "(\'descr\', True)"), ("\'data\'",\n[(\'43\', True, \'43\'), ("\'t\'", True, "\'t\'"), (\'None\', True, \'None\'), (\'[1]\', True, \'[1]\'), ("{\'k\': 1}", True, "{\'k\': 1}"), (\'int\', True, \'int\'), ("(\'descr\', True)", True, "(\'descr\', True)"), ("\'data\'", True, "\'data\'"), (\'method\', True, \'method\'), (\'cm\', True, \'cm\'), (\'sm\', True, \'sm\'), (\'property\', True, \'property\'), ("\'<default>\'", False, "AttributeError: type object \'Plain\' has no attribute \'missing\'")]\n',
    'metaclass_checks': "checks 300 625156900 [[True, False, False, True, False, True, False, True, False, True, False, True, False, True, True, False, True, True, True, True, True, 'issubclass() arg 1 must be a class', 'issubclass() arg 2 must b\n[True, False, True, False] [True, True, True, False] [True, False, True, False]\n",
    'dict_subclass_contains': 'contains 300 1163586197 [[True, False, True, False, True, True, True, True, True, False, True, False, False, True, \'no hash\', "cannot use \'list\' as a dict key (unhashable type: \'list\')"], [True, False, True, False, True, Tru\n[True, False, True, False, True, True, True, True, True, False, True, False, False, True, \'no hash\', "cannot use \'list\' as a dict key (unhashable type: \'list\')"] [True, False, True, False, True, True, True, True, True, False, False, True, False, True, \'no hash\', "cannot use \'list\' as a dict key (unhashable type: \'list\')"] [True, False, True, False, True, True, True, True, True, False, True, False, False, True, \'no hash\', "cannot use \'list\' as a dict key (unhashable type: \'list\')"] 600\n',
    'format_keyerrors': 'formats 1800 2769395197 [("KeyError(\'a\')", (\'a\',), "\'a\'"), ("KeyError(\'b\')", (\'b\',), "\'b\'"), ("KeyError(b\'a\')", (b\'a\',), "b\'a\'"), ("KeyError(\'a\')", (\'a\',), "\'a\'"), ("KeyError(\'a\')", (\'a\',), "\'a\'"), ("KeyError(\'a\')", (\'a\',), \n[("KeyError(\'a\')", (\'a\',), "\'a\'"), ("KeyError(\'b\')", (\'b\',), "\'b\'"), ("KeyError(b\'a\')", (b\'a\',), "b\'a\'"), ("KeyError(\'a\')", (\'a\',), "\'a\'"), ("KeyError(\'a\')", (\'a\',), "\'a\'"), ("KeyError(\'a\')", (\'a\',), "\'a\'")]\n',
}

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50", "WEAVEPY_JIT_SYNC": "1"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]


def run_case(name, extra=None):
    env = dict(os.environ, PYTHONHASHSEED="0", **(extra or {}))
    result = subprocess.run(
        [sys.executable, "-c", PRELUDE + CASES[name]],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class PlainObjectFastPathTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in CASES:
    setattr(PlainObjectFastPathTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
