"""The raise/catch fast paths, the one-step AttributeError and
StopIteration constructions, and compiled `with` statements keep CPython's
observable behavior: handled-exception nesting, chaining, exception fields,
special-method lookup and its invalidation."""

import sys
import threading
import contextlib


# --- Handled-exception nesting (sys.exc_info, bare raise, context) -------

def nested_handlers():
    out = []
    for i in range(300):
        try:
            raise KeyError(i)
        except KeyError as outer:
            assert sys.exc_info()[1] is outer
            try:
                raise ValueError(i)
            except ValueError as inner:
                assert sys.exc_info()[1] is inner
                assert inner.__context__ is outer
            assert sys.exc_info()[1] is outer
            try:
                raise
            except KeyError as again:
                assert again is outer
        assert sys.exc_info() == (None, None, None)
        out.append(i)
    return len(out)


assert nested_handlers() == 300


def reraiser():
    try:
        {}["k"]
    except KeyError:
        raise


for _ in range(200):
    try:
        reraiser()
    except KeyError as e:
        assert e.args == ("k",)
        tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        assert names == ["<module>", "reraiser"], names


# The handled exception dies at the end of its handler.
class Fin(Exception):
    dead = 0

    def __del__(self):
        Fin.dead += 1


for i in range(100):
    try:
        raise Fin()
    except Fin:
        pass
    assert Fin.dead == i + 1, (i, Fin.dead)


# except* still splits and rebinds the handled exception.
try:
    raise ExceptionGroup("g", [ValueError(1), KeyError(2)])
except* ValueError as eg:
    assert isinstance(sys.exc_info()[1], ExceptionGroup)
    assert [type(x) for x in eg.exceptions] == [ValueError]
except* KeyError as eg:
    assert [type(x) for x in eg.exceptions] == [KeyError]


# --- AttributeError ------------------------------------------------------

class Plain:
    cls_attr = 1


class WithGetattr:
    def __getattr__(self, name):
        return name.upper()


class Slotted:
    __slots__ = ("x",)


p = Plain()
for _ in range(300):
    try:
        p.missing
    except AttributeError as e:
        assert e.name == "missing" and e.obj is p
        assert e.args == ("'Plain' object has no attribute 'missing'",), e.args
        assert str(e) == "'Plain' object has no attribute 'missing'"
    else:
        raise AssertionError("no AttributeError")
    assert getattr(p, "missing", 7) == 7
    assert not hasattr(p, "missing")
    assert WithGetattr().abc == "ABC"
    s = Slotted()
    try:
        s.x
    except AttributeError as e:
        assert e.name == "x"
    try:
        s.y
    except AttributeError as e:
        assert e.name == "y" and e.obj is s
p.missing = 5
assert p.missing == 5
del p.missing
try:
    p.missing
except AttributeError as e:
    assert e.obj is p


# --- StopIteration from a generator's return -----------------------------

def gen_ret(v):
    yield 1
    return v


for v in (None, 0, "x", (1, 2)):
    g = gen_ret(v)
    next(g)
    try:
        next(g)
    except StopIteration as e:
        assert e.value == v
        assert e.args == (() if v is None else (v,)), e.args
    else:
        raise AssertionError("no StopIteration")


# --- `with` statements ---------------------------------------------------

class CM:
    def __init__(self):
        self.log = []

    def __enter__(self):
        self.log.append("enter")
        return self

    def __exit__(self, t, v, tb):
        self.log.append(("exit", t))
        return False


class Suppress:
    def __enter__(self):
        return 1

    def __exit__(self, t, v, tb):
        return t is not None and issubclass(t, KeyError)


def run_with(n):
    cm = CM()
    sup = Suppress()
    lock = threading.Lock()
    rlock = threading.RLock()
    total = 0
    for i in range(n):
        with cm as c:
            assert c is cm
            total += 1
        with sup as one:
            total += one
            {}[i]
        with lock, rlock:
            with rlock:
                total += 1
        with contextlib.suppress(KeyError):
            {}[i]
    assert not lock.locked()
    return total, len(cm.log)


assert run_with(3000) == (9000, 6000)


# The special-method site follows class changes.
class Swap:
    def __enter__(self):
        return "a"

    def __exit__(self, *a):
        return False


def enter_value(obj):
    with obj as v:
        return v


for i in range(2000):
    assert enter_value(Swap()) == "a"
Swap.__enter__ = lambda self: "b"
assert enter_value(Swap()) == "b"
del Swap.__enter__
try:
    enter_value(Swap())
except TypeError as e:
    assert "context manager" in str(e), e
else:
    raise AssertionError("missing __enter__ accepted")


# An instance attribute never stands in for the special method.
class OnInstance:
    def __enter__(self):
        return "class"

    def __exit__(self, *a):
        return False


o = OnInstance()
o.__enter__ = lambda: "instance"
for _ in range(500):
    assert enter_value(o) == "class"


# An exception in the body reaches __exit__ with its traceback.
class Seen:
    def __enter__(self):
        return self

    def __exit__(self, t, v, tb):
        self.got = (t, v, tb)
        return True


for _ in range(200):
    s = Seen()
    with s:
        raise ValueError("body")
    t, v, tb = s.got
    assert t is ValueError and isinstance(v, ValueError) and tb is v.__traceback__


@contextlib.contextmanager
def tagged(log, tag):
    log.append(tag)
    try:
        yield len(log)
    finally:
        log.pop()


log = []
for i in range(500):
    with tagged(log, i) as k:
        assert k == 1 and log == [i]
assert log == []
print("ok")
