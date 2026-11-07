"""Exception classes constructed from compiled code keep CPython's
semantics: `BaseException`'s `args`, a `StopIteration`'s `value`, a
subclass `__init__` that calls `super().__init__()`, and the chaining a
raise adds."""
import gc


class Plain(Exception):
    pass


class Stop(StopIteration):
    pass


class WithInit(Exception):
    def __init__(self, code, msg):
        super().__init__(msg)
        self.code = code


class NoSuper(Exception):
    def __init__(self, a):
        self.a = a


class Slotted(Exception):
    __slots__ = ("extra",)


class Holder:
    pass


def build(i):
    return Plain(i, "x"), Stop(i), WithInit(i, "m"), NoSuper(i), Slotted(i), Plain()


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    p, s, w, n, sl, empty = build(i)
    assert type(p) is Plain and p.args == (i, "x"), p.args
    assert str(p) == "(%d, 'x')" % i, str(p)
    assert s.value == i and s.args == (i,), (s.value, s.args)
    assert w.args == ("m",) and w.code == i, (w.args, w.code)
    assert str(w) == "m"
    # An `__init__` that skips `super().__init__()` still has the
    # constructor's arguments as `args` (BaseException_new seeds them).
    assert n.args == (i,) and n.a == i, (n.args, n.a)
    assert sl.args == (i,), sl.args
    sl.extra = i
    assert sl.extra == i
    assert empty.args == () and str(empty) == ""


# A non-atomic argument can close a cycle through the instance; the
# collector must still reclaim it.
def cyclic():
    h = Holder()
    e = Plain(h)
    h.e = e
    return e


for _ in range(3000):
    cyclic()
gc.collect()
assert not any(isinstance(o, Holder) for o in gc.get_objects())


# Raised from compiled code: the traceback, `__context__` and `from None`.
def lookup(d, k):
    try:
        return d[k]
    except KeyError:
        raise WithInit(404, "missing %s" % k) from None


def guarded(d, k):
    try:
        return lookup(d, k)
    except WithInit as e:
        assert e.__suppress_context__ is True
        assert isinstance(e.__context__, KeyError)
        assert e.__cause__ is None
        assert e.__traceback__ is not None
        return -e.code


for i in range(3000):
    assert guarded({}, i) == -404


# A re-raise inside a handler chains the handled exception exactly once,
# and an exception raised while handling itself doesn't chain to itself.
def chain(i):
    try:
        try:
            raise Plain(i)
        except Plain as a:
            raise Stop(i)
    except Stop as b:
        return b


for i in range(3000):
    b = chain(i)
    assert isinstance(b.__context__, Plain) and b.__context__.args == (i,)
    assert b.__context__.__context__ is None


def self_raise():
    e = Plain(1)
    try:
        try:
            raise e
        except Plain:
            raise e
    except Plain as got:
        return got


for _ in range(3000):
    got = self_raise()
    assert got.__context__ is None

print("ok")
