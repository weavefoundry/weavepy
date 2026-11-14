"""A class's own `__getattribute__`, its `__getattr__` fallback, and
`object.__getattribute__` called by name behave the same at a hot site as at
a cold one: what they return and raise, what they fall back to, and what
happens when the hooks change while a loop is running."""

import threading

N = 4000


class Const:
    def __getattribute__(self, name):
        return name.upper()


c = Const()
for _ in range(N):
    assert c.spam == "SPAM" and c.__class__ == "__CLASS__" and c.__dict__ == "__DICT__"


class Delegating:
    cls_attr = "on class"

    def __init__(self):
        self.x = 1
        self.reads = 0

    def __getattribute__(self, name):
        if name != "reads" and name != "__dict__":
            object.__setattr__(self, "reads", object.__getattribute__(self, "reads") + 1)
        return object.__getattribute__(self, name)

    @property
    def prop(self):
        return self.x + 1

    def method(self):
        return "m"


d = Delegating()
for _ in range(N):
    assert d.x == 1 and d.cls_attr == "on class" and d.prop == 2 and d.method() == "m"
# `prop` reads `x` through the hook too.
assert d.reads == N * 5, d.reads
for _ in range(50):
    try:
        d.missing
    except AttributeError as e:
        assert "missing" in str(e)
    else:
        raise AssertionError("expected AttributeError")


class Fallback:
    present = 1

    def __init__(self):
        self.inst = 2
        self.asked = []

    def __getattr__(self, name):
        if name.startswith("no_"):
            raise AttributeError(name)
        self.asked.append(name)
        return "fallback:" + name

    @property
    def raising(self):
        raise AttributeError("from property")


f = Fallback()
for _ in range(N):
    assert f.present == 1 and f.inst == 2 and f.other == "fallback:other"
assert f.asked == ["other"] * N
# A property that raises AttributeError falls back as well.
assert f.raising == "fallback:raising"
for _ in range(50):
    try:
        f.no_such
    except AttributeError:
        pass
    else:
        raise AssertionError("expected AttributeError")
assert getattr(f, "no_x", "dflt") == "dflt" and not hasattr(f, "no_y") and hasattr(f, "yes")


class Both:
    def __getattribute__(self, name):
        if name == "real":
            return "real"
        raise AttributeError(name)

    def __getattr__(self, name):
        return "second chance: " + name


b = Both()
for _ in range(N):
    assert b.real == "real" and b.other == "second chance: other"


# Hooks of other shapes take the general path and mean the same.
class Star:
    def __getattribute__(self, *args):
        return args[0]


class Gen:
    def __getattr__(self, name, extra=3):
        return (name, extra)


s, g = Star(), Gen()
for _ in range(N):
    assert s.q == "q" and g.q == ("q", 3)


# Hooks that change under a running loop.
class Changing:
    pass


ch = Changing()
ch.a = 1
seen = []
for i in range(N):
    if i == 1000:
        Changing.__getattr__ = lambda self, name: "late:" + name
    if i == 2000:
        Changing.__getattribute__ = lambda self, name: "all:" + name
    if i == 3000:
        del Changing.__getattribute__
        del Changing.__getattr__
    try:
        seen.append((ch.a, ch.b))
    except AttributeError:
        seen.append((ch.a, None))
assert seen[0] == (1, None) and seen[999] == (1, None)
assert seen[1000] == (1, "late:b") and seen[1999] == (1, "late:b")
assert seen[2000] == ("all:a", "all:b") and seen[2999] == ("all:a", "all:b")
assert seen[3000] == (1, None) and seen[-1] == (1, None)


# Inherited hooks, and `super()` inside one.
class Base:
    def __getattribute__(self, name):
        return "base:" + name if name.startswith("b_") else super().__getattribute__(name)


class Child(Base):
    def __init__(self):
        self.own = 5

    def __getattr__(self, name):
        return "child:" + name


k = Child()
for _ in range(N):
    assert k.b_x == "base:b_x" and k.own == 5 and k.zzz == "child:zzz"


# `object.__getattribute__` by name, on every kind of attribute.
class Slotted:
    __slots__ = ("a", "b")
    klass = "k"

    def meth(self):
        return 1

    @property
    def p(self):
        return "prop"


class Plain:
    klass = "k"
    number = 7

    def meth(self):
        return 1

    @property
    def p(self):
        return "prop"


get = object.__getattribute__
sl = Slotted()
sl.a = 10
pl = Plain()
pl.x = 1
pl.number = 8
for _ in range(N):
    assert get(sl, "a") == 10 and get(sl, "klass") == "k" and get(sl, "p") == "prop"
    assert get(sl, "meth")() == 1 and get(pl, "meth").__self__ is pl
    assert get(pl, "x") == 1 and get(pl, "number") == 8 and get(pl, "klass") == "k"
    assert get(pl, "p") == "prop" and get(pl, "__class__") is Plain
    assert get(pl, "__dict__") == {"x": 1, "number": 8}
for obj, name in ((sl, "b"), (sl, "nope"), (pl, "nope")):
    for _ in range(50):
        try:
            get(obj, name)
        except AttributeError:
            pass
        else:
            raise AssertionError("expected AttributeError for %s" % name)
assert get(3, "real") == 3 and get(Plain, "__name__") == "Plain"
try:
    get(pl, 3)
except TypeError:
    pass
else:
    raise AssertionError("attribute name must be a string")

# `__setattr__` hooks: every store is the hook's call, whatever the hook
# does with it.
class Logging:
    def __init__(self):
        object.__setattr__(self, "log", [])

    def __setattr__(self, name, value):
        self.log.append((name, value))
        object.__setattr__(self, name, value)


lg = Logging()
for i in range(N):
    lg.x = i
    lg.y = -i
assert lg.x == N - 1 and lg.y == 1 - N and len(lg.log) == 2 * N
assert lg.log[:2] == [("x", 0), ("y", 0)]


class ViaDict:
    def __setattr__(self, name, value):
        self.__dict__[name.upper()] = value


vd = ViaDict()
for i in range(N):
    vd.k = i
assert vd.K == N - 1 and not hasattr(vd, "k")


class ReadOnly:
    __slots__ = ("a",)

    def __setattr__(self, name, value):
        raise AttributeError("read-only: " + name)


ro = ReadOnly()
for _ in range(200):
    try:
        ro.a = 1
    except AttributeError as e:
        assert str(e) == "read-only: a"
    else:
        raise AssertionError("expected AttributeError")
assert not hasattr(ro, "a")


class StarSet:
    def __setattr__(self, *args):
        object.__setattr__(self, args[0], args[1] * 2)


ss = StarSet()
for i in range(N):
    ss.v = i
assert ss.v == 2 * (N - 1)


class Late:
    pass


lt = Late()
got = []
for i in range(N):
    if i == 1000:
        Late.__setattr__ = lambda self, name, value: got.append((name, value))
    if i == 2000:
        del Late.__setattr__
    lt.z = i
assert lt.z == N - 1 and got[0] == ("z", 1000) and len(got) == 1000

import dataclasses


@dataclasses.dataclass(frozen=True)
class Frozen:
    a: int
    b: str = "b"


for i in range(N):
    fz = Frozen(i)
    assert fz.a == i and fz.b == "b"
for _ in range(200):
    try:
        fz.a = 0
    except dataclasses.FrozenInstanceError:
        pass
    else:
        raise AssertionError("expected FrozenInstanceError")

# A class's name at a hot site follows the class being renamed.
class Named:
    pass


def names_of(cls, n):
    out = []
    for i in range(n):
        if i == 1000:
            cls.__name__ = "Renamed"
        if i == 2000:
            cls.__qualname__ = "Outer.Renamed"
        if i == 3000:
            cls.__module__ = "elsewhere"
        out.append((cls.__name__, cls.__qualname__, cls.__module__, type(cls()).__name__))
    return out


nm = names_of(Named, N)
assert nm[0] == nm[999] == ("Named", "Named", "__main__", "Named"), nm[0]
assert nm[1000] == nm[1999] == ("Renamed", "Named", "__main__", "Renamed"), nm[1000]
assert nm[2000] == nm[2999] == ("Renamed", "Outer.Renamed", "__main__", "Renamed"), nm[2000]
assert nm[3000] == nm[-1] == ("Renamed", "Outer.Renamed", "elsewhere", "Renamed"), nm[3000]
kinds = [int, str, Named, dict, Const]
seen_names = [k.__name__ for _ in range(1000) for k in kinds]
assert seen_names[-5:] == ["int", "str", "Renamed", "dict", "Const"]

# Names of classes with other metaclasses: `ABCMeta` leaves them to `type`,
# and a metaclass that answers for itself is asked every time.
import abc


class AbstractBase(abc.ABC):
    pass


class Concrete(AbstractBase):
    pass


class NamingMeta(type):
    asked = 0

    def __getattribute__(cls, name):
        if name == "__name__":
            NamingMeta.asked += 1
            return "meta says " + type.__getattribute__(cls, "__qualname__")
        return type.__getattribute__(cls, name)


class Named2(metaclass=NamingMeta):
    pass


cc = Concrete()
for i in range(N):
    if i == N // 2:
        Concrete.__name__ = "Solid"
    want = "Concrete" if i < N // 2 else "Solid"
    assert type(cc).__name__ == want and Concrete.__name__ == want
    assert AbstractBase.__name__ == "AbstractBase" and Concrete.__module__ == "__main__"
    assert Named2.__name__ == "meta says Named2"
assert NamingMeta.asked == N

# Thread-local storage, which is built on these.
tl = threading.local()
tl.value = "main"
out = []


def worker():
    assert not hasattr(tl, "value")
    tl.value = "worker"
    for _ in range(N):
        assert tl.value == "worker"
    out.append(tl.value)


t = threading.Thread(target=worker)
t.start()
t.join()
for _ in range(N):
    assert tl.value == "main"
assert out == ["worker"] and tl.__dict__ == {"value": "main"}
print("ok")
