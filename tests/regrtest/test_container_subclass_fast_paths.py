"""`C()` of a dict, list or set subclass keeps CPython's construction
semantics when its base's allocator and `__init__` make the instance."""
import abc
import gc


class D(dict):
    pass


class L(list):
    pass


class S(set):
    pass


class DI(dict):
    def __init__(self):
        super().__init__()
        self.x = 1


class DN(dict):
    def __new__(cls):
        inst = super().__new__(cls)
        inst["new"] = True
        return inst


class Abstract(dict, abc.ABC):
    @abc.abstractmethod
    def f(self):
        pass


class InitLater(dict):
    pass


def build():
    return D(), L(), S()


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    d, l, s = build()
    assert type(d) is D and d == {} and not d, d
    assert type(l) is L and l == [] and not l, l
    assert type(s) is S and s == set() and not s, s
    d["k"] = i
    l.append(i)
    s.add(i)
    assert d == {"k": i} and l == [i] and s == {i}
    d.attr = i
    assert d.attr == i and vars(d) == {"attr": i}
    assert D(a=1) == {"a": 1} and L([1, 2]) == [1, 2] and S([3]) == {3}
    di = DI()
    assert di.x == 1 and di == {}
    assert DN() == {"new": True}
    # (`dict.__new__`, not `object.__new__`, allocates: CPython makes an
    # abstract dict subclass's instance.)
    assert type(Abstract()) is Abstract
    # A cycle through a fresh instance is collectable.
    d["self"] = d
    assert gc.is_tracked(d)
    if i == 2000:
        # A later `__init__` on the class takes over construction.
        InitLater.__init__ = lambda self: self.update(late=True)
    expected = {"late": True} if i >= 2000 else {}
    assert InitLater() == expected, (i, InitLater())
del d
assert gc.collect() >= 0
print("container subclass fast paths: ok")
