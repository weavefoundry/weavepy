"""A missing attribute read from compiled code, whose `AttributeError` the
native code raises in place, keeps CPython's results: the message, `name`
and `obj`, the traceback's line, the context of a raise in an `except`
block, a class's `__getattr__`, an unset `__slots__` member, an attribute
deleted later, a receiver from the stack, and one whose last reference
dies with the raise."""
import sys


class Obj:
    a = 1


class Hooked:
    def __getattr__(self, name):
        return name * 2


class Slotted:
    __slots__ = ("x",)


class Dies:
    died = 0

    def __del__(self):
        Dies.died += 1


class Holder:
    def __init__(self):
        self.inner = Obj()


def miss(o):
    try:
        return o.missing
    except AttributeError as e:
        tb = e.__traceback__
        line = tb.tb_lineno - miss.__code__.co_firstlineno
        return (str(e), e.name, e.obj is o, line, e.__context__)


def miss_in_handler(o):
    try:
        {}[1]
    except KeyError:
        try:
            o.missing
        except AttributeError as e:
            return type(e.__context__).__name__


def miss_stack(h):
    try:
        return h.inner.missing
    except AttributeError as e:
        return (e.name, type(e.obj).__name__)


def miss_dying():
    try:
        Dies().missing
    except AttributeError as e:
        return e.name


def run(i):
    o = Obj()
    assert miss(o) == ("'Obj' object has no attribute 'missing'", "missing", True, 2, None), miss(o)
    o.missing = i
    assert miss(o) == i
    del o.missing
    assert miss(o)[1] == "missing"
    assert miss(Hooked()) == "missingmissing"
    s = miss(Slotted())
    assert s[0].endswith("'Slotted' object has no attribute 'missing'") and s[1] == "missing", s
    assert miss_in_handler(o) == "KeyError"
    assert miss_stack(Holder()) == ("missing", "Obj")
    before = Dies.died
    assert miss_dying() == "missing"
    assert sys.exc_info() == (None, None, None)
    assert Dies.died >= before


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
del Dies.__del__
print("ok")
