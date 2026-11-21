"""Method calls and native-method reads at compiled sites.

`self.fn()` of a callable an instance holds in its own dict answers from
where the site last found it, also for an instance whose dict was
materialized (its names assigned out of the class's order, as a
`threading.Condition` over a lock with `_release_save` does): a replaced,
deleted or class-shadowed value, and a class changed mid-loop, behave as
in CPython. Reading a native method off a built-in subclass's instance
(`lock.__exit__`, to call with `*args`) binds it each time, unless the
instance or its class shadows it; a lock's `__exit__` returns `None` and
raises for an unheld lock. Run it past the compile threshold.
"""

import threading


class Holder:
    def __init__(self, order):
        for name in order:
            setattr(self, name, (lambda n: lambda: n)(name))

    def call(self, n):
        t = []
        for _ in range(n):
            t.append(self.b())
        return t


first = Holder("ab")
late = Holder("ba")
N = 3000
assert first.call(N) == ["b"] * N
assert late.call(N) == ["b"] * N
late.b = lambda: "new"
assert late.call(3) == ["new"] * 3
late.__dict__["b"] = lambda: "dict"
assert late.call(3) == ["dict"] * 3
del late.b
Holder.b = lambda self: "class"
assert late.call(3) == ["class"] * 3
del Holder.b


class Desc:
    def __get__(self, obj, owner=None):
        return lambda: "descr"

    def __set__(self, obj, v):
        raise AttributeError


other = Holder("ba")
assert other.call(N) == ["b"] * N
Holder.b = Desc()
assert other.call(3) == ["descr"] * 3
del Holder.b


def notify_many(c, n):
    with c:
        for _ in range(n):
            c.notify()


notify_many(threading.Condition(threading.Lock()), N)
notify_many(threading.Condition(threading.RLock()), N)
notify_many(threading.Condition(), N)


def exits(lk, n):
    args = (None, None, None)
    out = []
    for _ in range(n):
        lk.__enter__()
        out.append(lk.__exit__(*args))
    return out


class SubDict(dict):
    pass


def reads(d, n):
    got = []
    for _ in range(n):
        f = d.get
        got.append(f("k", 0))
    return got


assert exits(threading.Lock(), N) == [None] * N
assert exits(threading.RLock(), N) == [None] * N
for lock in (threading.Lock(), threading.RLock()):
    try:
        lock.__exit__(None, None, None)
    except RuntimeError:
        pass
    else:
        raise AssertionError("exit of an unheld lock")
d = SubDict(k=1)
assert reads(d, N) == [1] * N
d.get = lambda k, default=None: "own"
assert reads(d, 3) == ["own"] * 3
e = SubDict(k=2)
assert reads(e, 3) == [2] * 3
SubDict.get = lambda self, k, default=None: "class"
assert reads(e, 3) == ["class"] * 3
print("ok")
