"""A compiled `x.__reduce_ex__(proto)` (as `copy` and `pickle` call it, as a
method or through a saved bound method) gives `object`'s default
reduction for a plain instance, and every override still has its say:
a class `__reduce__`, `__getstate__`, `__getnewargs__`, `__slots__`, and
the protocols below 2."""

import copy
import copyreg
import pickle


class Plain:
    def __init__(self, a):
        self.a = a
        self.b = [a]


class Empty:
    pass


class Reducing(Plain):
    def __reduce__(self):
        return (Plain, (99,))


class State(Plain):
    def __getstate__(self):
        return {"a": -1, "b": []}


class NewArgs(Plain):
    def __new__(cls, a=0):
        return super().__new__(cls)

    def __getnewargs__(self):
        return (self.a,)


class Slotted:
    __slots__ = ("x",)

    def __init__(self, x):
        self.x = x


def reduce_method(objs, proto, n):
    out = None
    for _ in range(n):
        out = [o.__reduce_ex__(proto) for o in objs]
    return out


def reduce_saved(obj, proto, n):
    f = obj.__reduce_ex__
    out = None
    for _ in range(n):
        out = f(proto)
    return out


p = Plain(3)
r = reduce_method([p], 4, 300)[0]
assert r[0] is copyreg.__newobj__, r
assert r[1] == (Plain,), r
assert r[2] == {"a": 3, "b": [3]} and r[2] is p.__dict__, r
assert r[3:] == (None, None), r

r = reduce_saved(Empty(), 2, 300)
assert r == (copyreg.__newobj__, (Empty,), None, None, None), r

assert reduce_saved(Reducing(1), 4, 300) == (Plain, (99,))
assert reduce_saved(State(1), 4, 300)[2] == {"a": -1, "b": []}
r = reduce_saved(NewArgs(5), 4, 300)
assert r[1] == (NewArgs, 5), r
r = reduce_saved(Slotted(7), 4, 300)
assert r[2] == (None, {"x": 7}), r
r = reduce_saved(Plain(2), 1, 300)
assert r[0] is copyreg._reconstructor, r

for _ in range(200):
    c = copy.deepcopy(Plain(4))
assert c.a == 4 and c.b == [4] and type(c) is Plain
assert copy.deepcopy(Reducing(1)).a == 99
assert copy.deepcopy(State(1)).a == -1
assert pickle.loads(pickle.dumps(Plain(6))).b == [6]

print("ok")
