"""Method loads on a user class receiver (`C.s(x)`, `C.make()`).

Compiled code remembers a class's plain function, static method or class
method at the site while the class's and its metaclass's versions hold;
reassigning the attribute, swapping a static method for a class method,
another class at the site, a metaclass that reads attributes itself or
defines the name, and a descriptor with its own `__get__` all behave as
in CPython. Run it past the compile threshold.
"""


class C:
    @staticmethod
    def s(x):
        return ("s", x)

    @classmethod
    def make(cls):
        return ("make", cls.__name__)

    def plain(x):
        return ("plain", x)


class Sub(C):
    pass


class Meta(type):
    def __getattribute__(cls, name):
        if name == "s":
            return lambda x: ("meta", x)
        return super().__getattribute__(name)


class M(metaclass=Meta):
    @staticmethod
    def s(x):
        return ("s", x)


class Shadow(type):
    def s(cls, x):
        return ("shadow", x)


class N(metaclass=Shadow):
    pass


class Desc:
    calls = 0

    def __get__(self, obj, cls):
        Desc.calls += 1
        return lambda x: ("desc", Desc.calls, x)


class D:
    s = Desc()


def run(n, k):
    out = []
    for i in range(n):
        out.append((k.s(i), k.make(), k.plain(i)))
    return out


N_ = 2000
assert run(N_, C)[-1] == (("s", N_ - 1), ("make", "C"), ("plain", N_ - 1))
assert run(N_, Sub)[-1] == (("s", N_ - 1), ("make", "Sub"), ("plain", N_ - 1))


def swap(n):
    out = []
    for i in range(n):
        out.append(C.s(i))
    return out


assert swap(N_)[-1] == ("s", N_ - 1)
C.s = classmethod(lambda cls, x: ("cm", cls.__name__, x))
assert swap(5)[-1] == ("cm", "C", 4)
C.s = staticmethod(lambda x: ("new", x))
assert swap(5)[-1] == ("new", 4)
C.s = lambda x: ("fn", x)
assert swap(5)[-1] == ("fn", 4)
Meta.s = 1  # an unrelated metaclass change
del C.s
try:
    swap(1)
except AttributeError:
    pass
else:
    raise AssertionError("deleted attribute")


def via(k, n):
    out = []
    for i in range(n):
        out.append(k.s(i))
    return out


assert via(M, N_)[-1] == ("meta", N_ - 1)
assert via(N, N_)[-1] == ("shadow", N_ - 1)
r = via(D, N_)
assert r[-1] == ("desc", N_, N_ - 1) and Desc.calls == N_
print("ok")
