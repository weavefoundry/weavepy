"""Direct calls between compiled functions keep every observable behavior:
the frame chain, tracebacks, locals, finalizer timing, the recursion limit,
and warnings' stack levels, from hot callees that never leave native code
and from ones that do, and the frame chain that Python code their helpers
run (a property, an `__eq__`, a finalizer) observes."""

import gc
import sys
import traceback
import warnings


class Point:
    def __init__(self, x):
        self.x = x

    def get(self):
        return self.x

    def get_default(self, extra=0):
        return self.x + extra

    def chained(self, k):
        return self.get() + self.get_default(k)


def leaf(a, b):
    return a + b


def middle(a):
    return leaf(a, 1) * 2


def outer(a):
    return middle(a) + middle(a + 1)


def frames_up():
    f = sys._getframe(1)
    names = []
    while f is not None and len(names) < 4:
        names.append(f.f_code.co_name)
        f = f.f_back
    return names


def callee_with_frame():
    return frames_up()


def caller_of_frame():
    return callee_with_frame()


def raise_deep(n):
    if n == 0:
        raise ValueError("bottom")
    return raise_deep(n - 1)


def catch_deep():
    try:
        raise_deep(3)
    except ValueError as e:
        return [fs.name for fs in traceback.extract_tb(e.__traceback__)]


def show_locals(a, b):
    c = a + b
    return sorted(sys._getframe().f_locals)


finalized = []


class Fin:
    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        finalized.append(self.tag)


def make_and_drop(tag):
    obj = Fin(tag)
    return obj.tag


def recurse(n):
    return 0 if n == 0 else 1 + recurse(n - 1)


def warn_up():
    warnings.warn("careful", UserWarning, stacklevel=2)
    return 1


def calls_warn():
    return warn_up()


def gen_items(n):
    for i in range(n):
        yield i


def sums_generator(n):
    total = 0
    for v in gen_items(n):
        total += v
    return total


def builds(n):
    return Point(n).chained(1)


def hot_calls():
    p = Point(5)
    for i in range(3000):
        assert outer(i) == 2 * (i + 1) + 2 * (i + 2)
        assert p.chained(i) == 10 + i
        assert p.get_default() == 5
        assert builds(i) == 2 * i + 1
        assert sums_generator(4) == 6


def introspection():
    for _ in range(300):
        assert caller_of_frame() == ["callee_with_frame", "caller_of_frame", "introspection", "<module>"]
        assert catch_deep() == ["catch_deep", "raise_deep", "raise_deep", "raise_deep", "raise_deep"]
        assert show_locals(1, 2) == ["a", "b", "c"]


def finalizers():
    # A finalizer of an object a callee drops runs by the time the
    # callee's caller goes on.
    for i in range(500):
        finalized.clear()
        assert make_and_drop(i) == i
        assert finalized == [i], finalized


def recursion():
    # Deep direct recursion still hits the recursion limit, and recovers.
    limit = sys.getrecursionlimit()
    for _ in range(3):
        try:
            recurse(limit + 50)
        except RecursionError:
            pass
        else:
            raise AssertionError("no RecursionError")
    assert recurse(200) == 200


def warning_levels():
    # A warning's stack level names the caller's caller.
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        for _ in range(300):
            calls_warn()
    assert caught and all(w.filename == __file__ for w in caught)
    lines = {w.lineno for w in caught}
    assert len(lines) == 1, lines


def chain():
    """The function names from the caller up, to the module."""
    f = sys._getframe(1)
    names = []
    while f is not None:
        names.append(f.f_code.co_name)
        f = f.f_back
    return names[: names.index("<module>") + 1]


class Probe:
    """Python code a callee's helpers run (a getter, an `__eq__`, a
    finalizer), each recording the frame chain it sees."""

    seen = []

    @property
    def value(self):
        Probe.seen.append(chain())
        return 1

    @property
    def failing(self):
        Probe.seen.append(chain())
        raise KeyError("probe")

    def __eq__(self, other):
        Probe.seen.append(chain())
        return True

    __hash__ = object.__hash__


class Dying:
    def __del__(self):
        Probe.seen.append(chain())


def reads_property(p):
    return p.value + 1


def compares(p, q):
    return p == q


def drops(x):
    d = Dying()
    del d
    return x


def catches(p):
    try:
        return p.failing
    except KeyError:
        return -1


def counts(n):
    for i in range(n):
        yield i


def walks_generator(n):
    return sum(counts(n)) + len(chain())


def via_property(p):
    return reads_property(p)


def python_from_helpers():
    # Python code that a lazily running callee's helpers run sees the whole
    # chain, the callee included, in order.
    p, q = Probe(), Probe()
    for _ in range(400):
        Probe.seen.clear()
        assert reads_property(p) == 2
        assert compares(p, q)
        assert drops(3) == 3
        assert catches(p) == -1
        assert via_property(p) == 2
        assert Probe.seen == [
            ["value", "reads_property", "python_from_helpers", "<module>"],
            ["__eq__", "compares", "python_from_helpers", "<module>"],
            ["__del__", "drops", "python_from_helpers", "<module>"],
            ["failing", "catches", "python_from_helpers", "<module>"],
            ["value", "reads_property", "via_property", "python_from_helpers", "<module>"],
        ], Probe.seen
        assert walks_generator(5) == 10 + 3


for _ in range(3):
    hot_calls()
    introspection()
    finalizers()
    recursion()
    warning_levels()
    python_from_helpers()

gc.collect()
print("ok")
