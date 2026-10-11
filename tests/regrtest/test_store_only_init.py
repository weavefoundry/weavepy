# A class whose `__init__` only stores its parameters (or constants) into
# `self` constructs by those stores alone once its store sites have
# proven them plain `__dict__` stores. Every way the class or the function
# can change under that shortcut must still be honored.

import gc
import weakref

N = 300


class V:
    def __init__(self, x, y, z=7):
        self.x = x
        self.y = y
        self.z = z
        self.k = 5
        self.label = "v"
        self.me = self
        self.again = x


def build(n):
    return [V(i, -i) for i in range(n)]


for v in build(N)[-3:]:
    assert v.me is v
    assert v.z == 7 and v.k == 5 and v.label == "v" and v.again == v.x
    assert list(vars(v)) == ["x", "y", "z", "k", "label", "me", "again"], vars(v)

# A default given explicitly, and the argument stored twice.
for i in range(N):
    v = V([i], 2, z=None) if i % 2 else V([i], 2, None)
    assert v.z is None and v.x is v.again and v.x == [i]

# Arity errors still raise from the call.
for args in [(), (1,), (1, 2, 3, 4)]:
    try:
        V(*args)
    except TypeError as e:
        assert "argument" in str(e), e
    else:
        raise AssertionError(args)

# A property installed later on the class intercepts the store.
seen = []


def set_y(self, value):
    seen.append(value)
    self.__dict__["_y"] = value


build(N)
V.y = property(lambda self: self.__dict__["_y"], set_y)
v = V(1, "yy")
assert seen == ["yy"] and v.y == "yy" and "y" not in vars(v)
del V.y
for v in build(N)[-2:]:
    assert "y" in vars(v)

# A `__setattr__` added later sees every store.
calls = []


def tracing_setattr(self, name, value):
    calls.append(name)
    object.__setattr__(self, name, value)


V.__setattr__ = tracing_setattr
V(1, 2)
assert calls == ["x", "y", "z", "k", "label", "me", "again"], calls
del V.__setattr__
calls.clear()
build(N)
assert calls == []


# A subclass sharing the base's `__init__`, built in alternation.
class W(V):
    pass


for i in range(N):
    a, b = V(i, 0), W(i, 1)
    assert type(a) is V and type(b) is W and b.me is b and b.y == 1

# Replacing `__init__`'s code or its defaults takes effect at once.
build(N)


def other(self, x, y, z=7):
    self.x = -x


V.__init__.__code__ = other.__code__
v = V(3, 4)
assert vars(v) == {"x": -3}, vars(v)


class D:
    def __init__(self, a, b=1):
        self.a = a
        self.b = b


for i in range(N):
    assert D(i).b == 1
D.__init__.__defaults__ = (99,)
assert D(0).b == 99

# Instances still die by reference count, and the class with its last.
for _ in range(2):
    class T:
        def __init__(self, a):
            self.a = a

    for i in range(N):
        T(i)
    r = weakref.ref(T)
    del T
    gc.collect()
    assert r() is None

print("test_store_only_init: OK")
