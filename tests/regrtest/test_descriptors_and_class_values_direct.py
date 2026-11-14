"""Reads that compiled code serves through a descriptor written in Python
(`enum`'s `name` and `value`, any class with `__get__` and `__set__`), a
metaclass's `__getitem__` (`Color["RED"]`), and a class's own container
attributes give what the attribute protocol gives every time: through
replaced descriptors, methods and metaclass attributes, raises, receivers
that die at the read, and rebinding under a running loop."""

import enum
import types

N = 3000
died = []


class Typed:
    """A data descriptor that counts its reads."""

    def __init__(self, default):
        self.default = default
        self.reads = 0

    def __set_name__(self, owner, name):
        self.slot = "_" + name

    def __get__(self, instance, owner):
        self.reads += 1
        if instance is None:
            return self
        return instance.__dict__.get(self.slot, self.default)

    def __set__(self, instance, value):
        instance.__dict__[self.slot] = value


class Record:
    size = Typed(0)
    label = Typed("none")

    def __init__(self, size=None):
        if size is not None:
            self.size = size


def total(rec, n):
    acc = 0
    for _ in range(n):
        acc += rec.size
    return acc


def labels(rec, n):
    out = None
    for _ in range(n):
        out = rec.label
    return out


for _ in range(3):
    r = Record(3)
    before = Record.__dict__["size"].reads
    assert total(r, N) == 3 * N
    assert Record.__dict__["size"].reads == before + N
    assert total(Record(), N) == 0
    assert labels(r, N) == "none"
    r.label = "set"
    assert labels(r, N) == "set"

# The instance's own entry of the same name doesn't shadow a data
# descriptor, and does shadow one that stops being a data descriptor.
r = Record(5)
r.__dict__["size"] = 99
assert total(r, N) == 5 * N
saved = Typed.__set__
del Typed.__set__
assert total(r, N) == 99 * N
Typed.__set__ = saved
assert total(r, N) == 5 * N
del r.__dict__["size"]


# The same through a class that extends a builtin one, and a descriptor
# class that becomes one after its instance is in place.
class Table(dict):
    size = Typed(8)


class Late:
    pass


class Slotless:
    late = Late()


t = Table()
t.__dict__["size"] = 99
assert total(t, N) == 8 * N
del Typed.__set__
assert total(t, N) == 99 * N
Typed.__set__ = saved
assert total(t, N) == 8 * N


def late(obj, n):
    out = None
    for _ in range(n):
        out = obj.late
    return out


s = Slotless()
assert late(s, N) is Slotless.__dict__["late"]
Late.__get__ = lambda self, instance, owner: "got"
assert late(s, N) == "got"
s.__dict__["late"] = "own"
assert late(s, N) == "own"
Late.__set__ = lambda self, instance, value: None
assert late(s, N) == "got"
del Late.__get__, Late.__set__
assert late(s, N) == "own"


# A descriptor replaced on the class, and a `__get__` replaced on the
# descriptor's class, under a running loop.
class Fixed:
    def __init__(self, value):
        self.value = value

    def __get__(self, instance, owner):
        return self.value

    def __set__(self, instance, value):
        raise AttributeError("read-only")


class Holder:
    attr = Fixed(1)


def swap_descriptor(n):
    h = Holder()
    seen = []
    for i in range(n):
        if i == n // 3:
            Holder.attr = Fixed(2)
        if i == 2 * n // 3:
            Holder.attr = 7
        seen.append(h.attr)
    return seen


seen = swap_descriptor(N)
assert seen == [1] * (N // 3) + [2] * (2 * N // 3 - N // 3) + [7] * (N - 2 * N // 3)
Holder.attr = Fixed(1)


def swap_get(n):
    h = Holder()
    seen = []
    original = Fixed.__get__
    for i in range(n):
        if i == n // 2:
            Fixed.__get__ = lambda self, instance, owner: -self.value
        seen.append(h.attr)
    Fixed.__get__ = original
    return seen


seen = swap_get(N)
assert seen == [1] * (N // 2) + [-1] * (N - N // 2)
assert Holder().attr == 1


# A raise from `__get__`: an `AttributeError` is the read's own, or goes to
# the class's `__getattr__`; anything else passes through.
class Failing:
    def __init__(self, exc):
        self.exc = exc

    def __get__(self, instance, owner):
        raise self.exc("from get")

    def __set__(self, instance, value):
        pass


class Strict:
    missing = Failing(AttributeError)
    broken = Failing(KeyError)


class Lenient(Strict):
    def __getattr__(self, name):
        return "fallback:" + name


def failing(obj, n):
    attr = key = 0
    for _ in range(n):
        try:
            obj.missing
        except AttributeError as e:
            assert str(e) == "from get"
            attr += 1
        try:
            obj.broken
        except KeyError:
            key += 1
    return attr, key


assert failing(Strict(), N) == (N, N)


def lenient(obj, n):
    hits = 0
    for _ in range(n):
        assert obj.missing == "fallback:missing"
        try:
            obj.broken
        except KeyError:
            hits += 1
    return hits


assert lenient(Lenient(), N) == N
assert hasattr(Lenient(), "missing") and not hasattr(Strict(), "missing")
assert getattr(Strict(), "missing", "default") == "default"


# `__get__` in other shapes: a default for the owner, extra parameters, a
# generator function, a method that calls on.
class Shapes:
    class Defaulted:
        def __get__(self, instance, owner=None):
            return ("defaulted", owner is type(instance))

        def __set__(self, instance, value):
            pass

    class Variadic:
        def __get__(self, *args):
            return len(args)

        def __delete__(self, instance):
            pass

    class Generating:
        def __get__(self, instance, owner):
            yield instance
            yield owner

        def __set__(self, instance, value):
            pass

    class Chained:
        def __get__(self, instance, owner):
            return instance.first[0], instance.second

        def __set__(self, instance, value):
            pass

    first = Defaulted()
    second = Variadic()
    third = Generating()
    fourth = Chained()


def shapes(n):
    s = Shapes()
    for _ in range(n):
        assert s.first == ("defaulted", True)
        assert s.second == 2
        assert list(s.third) == [s, Shapes]
        assert s.fourth == ("defaulted", 2)
    return True


assert shapes(N)


# A receiver that only the read holds is released when the read ends.
class Mortal:
    size = Typed(4)

    def __del__(self):
        died.append("mortal")


def mortal(n):
    for i in range(n):
        assert Mortal().size == 4
        assert len(died) == i + 1
    return True


assert mortal(N)
died.clear()


# `enum`'s own descriptors, and `types.DynamicClassAttribute`.
class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 3


class Perm(enum.IntFlag):
    R = 4
    W = 2
    X = 1


class Dynamic:
    def __init__(self):
        self._x = 10

    @types.DynamicClassAttribute
    def x(self):
        return self._x


def enums(n):
    d = Dynamic()
    for _ in range(n):
        assert Color.RED.name == "RED" and Color.BLUE.value == 3
        assert Color["GREEN"] is Color.GREEN and Color(1) is Color.RED
        assert Perm.R.name == "R" and (Perm.R | Perm.W).value == 6
        assert Perm["X"] is Perm.X and Perm(4) is Perm.R
        assert d.x == 10
    return True


assert enums(N)


def enum_misses(n):
    hits = 0
    for _ in range(n):
        try:
            Color["PURPLE"]
        except KeyError as e:
            assert e.args == ("PURPLE",)
            hits += 1
        try:
            Color(9)
        except ValueError:
            hits += 1
    return hits


assert enum_misses(N) == 2 * N
try:
    Dynamic.x
except AttributeError:
    pass
else:
    raise AssertionError("a DynamicClassAttribute read through its class")


# A metaclass's `__getitem__`, replaced and removed under a running loop.
class Registry(type):
    def __getitem__(cls, key):
        return cls.table[key]


class Shop(metaclass=Registry):
    table = {"a": 1, "b": 2}


def registry(n):
    seen = []
    original = Registry.__getitem__
    for i in range(n):
        if i == n // 2:
            Registry.__getitem__ = lambda cls, key: key.upper()
        seen.append(Shop["a"])
    Registry.__getitem__ = original
    return seen


seen = registry(N)
assert seen == [1] * (N // 2) + ["A"] * (N - N // 2)


def registry_gone(n):
    hits = 0
    for i in range(n):
        try:
            Shop["zz"]
        except KeyError:
            hits += 1
    del Registry.__getitem__
    try:
        Shop["a"]
    except TypeError:
        hits += 1
    return hits


original = Registry.__getitem__
assert registry_gone(N) == N + 1
Registry.__getitem__ = original


# A class's own containers, read through a class under a metaclass: the
# object bound now, through rebinding, a metaclass that starts answering
# for the name, and one that takes over attribute reads.
class Meta(type):
    pass


class Config(metaclass=Meta):
    options = {"depth": 1}
    names = ["a"]
    pair = (1, 2)
    tags = {"x"}


class Derived(Config):
    pass


def containers(cls, n):
    acc = 0
    for _ in range(n):
        acc += cls.options["depth"] + len(cls.names) + cls.pair[1] + len(cls.tags)
    return acc


for cls in (Config, Derived):
    assert containers(cls, N) == 5 * N
Config.names.append("b")
Config.options["depth"] = 10
assert containers(Derived, N) == 15 * N


def rebinding(n):
    seen = []
    for i in range(n):
        if i == n // 2:
            Config.options = {"depth": 100}
        seen.append(Derived.options["depth"])
    return seen


seen = rebinding(N)
assert seen == [10] * (N // 2) + [100] * (N - N // 2)
Derived.options = {"depth": -1}
assert containers(Derived, N) == (-1 + 2 + 2 + 1) * N
assert containers(Config, N) == (100 + 2 + 2 + 1) * N
del Derived.options


def meta_answers(n):
    seen = []
    for i in range(n):
        if i == n // 2:
            Meta.options = property(lambda cls: {"depth": 7})
        seen.append(Config.options["depth"])
    del Meta.options
    return seen


seen = meta_answers(N)
assert seen == [100] * (N // 2) + [7] * (N - N // 2)
assert Config.options["depth"] == 100


def meta_hook(n):
    seen = []
    for i in range(n):
        if i == n // 2:
            Meta.__getattribute__ = lambda cls, name: (5, 6) if name == "pair" else type.__getattribute__(cls, name)
        seen.append(Config.pair[1])
    del Meta.__getattribute__
    return seen


seen = meta_hook(N)
assert seen == [2] * (N // 2) + [6] * (N - N // 2)
assert Config.pair == (1, 2)

print("ok")
