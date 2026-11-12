"""Reads of protocol dunders and native methods off classes from compiled
loops give what the full attribute lookup gives: a builtin type's slot
wrappers and method descriptors (`dict.__setitem__`, `list.append`,
`object.__setattr__`), a subclass's inherited ones, a heap class's own
functions, a dunder assigned or deleted on the class (and on a base) between
reads, a metaclass that defines the name, `type`'s data descriptors
(`__name__`, `__dict__`, `__doc__`), and calls through what was read."""


class D(dict):
    pass


class E(dict):
    def __setitem__(self, k, v):
        dict.__setitem__(self, k, v * 2)


class Meta(type):
    def __getitem__(cls, k):
        return ("meta", k)


class M(metaclass=Meta):
    pass


class Base:
    pass


class Sub(Base):
    pass


def reads(n):
    out = 0
    for i in range(n):
        out += dict.__setitem__ is D.__setitem__
        out += E.__setitem__ is not dict.__setitem__
        out += list.append is list.append
        out += object.__setattr__ is Sub.__setattr__
        out += type(D()).__len__ is dict.__len__
        out += D.__name__ == "D"
        out += "__module__" in D.__dict__
        d = D()
        D.__setitem__(d, "k", i)
        out += d["k"] == i
        e = E()
        e["k"] = i
        out += e["k"] == 2 * i
        lst = []
        list.append(lst, i)
        out += lst == [i]
    return out


for _ in range(4):
    assert reads(300) == 300 * 10, reads(300)


def meta_reads(n):
    out = []
    for i in range(n):
        out.append(M[i])
    return out


for _ in range(3):
    assert meta_reads(50) == [("meta", i) for i in range(50)]


def sub_len(n):
    t = 0
    for i in range(n):
        f = Sub.__len__ if hasattr(Sub, "__len__") else None
        t += 0 if f is None else f(Sub())
    return t


for _ in range(3):
    assert sub_len(100) == 0
Base.__len__ = lambda self: 3
assert sub_len(100) == 300
del Base.__len__
assert sub_len(100) == 0


def set_read(n):
    vals = []
    for i in range(n):
        vals.append(D.__setitem__)
    return vals


for _ in range(3):
    assert all(v is dict.__setitem__ for v in set_read(100))


def mine(self, k, v):
    dict.__setitem__(self, k, -1)


D.__setitem__ = mine
try:
    assert all(v is mine for v in set_read(50))
    d = D()
    d["a"] = 5
    assert d["a"] == -1
finally:
    del D.__setitem__
assert all(v is dict.__setitem__ for v in set_read(50))
print("ok")
