"""Direct calls between compiled functions run on per-thread records that do
most of a call's bookkeeping only if something looks. Whatever looks, from
inside such a call, sees exactly what an ordinary call shows."""

import gc
import sys
import threading
import traceback
import weakref

HOT = 6000


def warm(fn, *args):
    """Run `fn` hot enough that it and its callees compile."""
    for _ in range(HOT):
        fn(*args)


# ---- tracebacks, line numbers, frames ----


def leaf_raises(flag):
    z = 1
    del z
    if flag:
        raise ValueError("boom")
    return 1


def mid_raises(flag):
    z = 1
    del z
    return leaf_raises(flag) + 1


def top_raises(flag):
    z = 1
    del z
    return mid_raises(flag) + 1


warm(top_raises, False)
try:
    top_raises(True)
except ValueError as e:
    names = []
    lines = []
    tb = e.__traceback__
    while tb is not None:
        names.append(tb.tb_frame.f_code.co_name)
        lines.append(tb.tb_lineno - tb.tb_frame.f_code.co_firstlineno)
        tb = tb.tb_next
    assert names[-3:] == ["top_raises", "mid_raises", "leaf_raises"], names
    assert lines[-3:] == [3, 3, 4], lines
else:
    raise AssertionError("no raise")


def caught_below(flag):
    try:
        return leaf_raises(flag)
    except ValueError:
        return -1


warm(caught_below, False)
assert caught_below(True) == -1
warm(caught_below, False)
assert sys.exception() is None


def getframe_back(a, b):
    z = a
    del z
    f = sys._getframe(0)
    assert f.f_code.co_name == "getframe_back"
    assert f.f_locals["a"] == a and f.f_locals["b"] == b
    assert f.f_back.f_code.co_name == "getframe_caller"
    assert f.f_back.f_locals["x"] == a
    return f.f_lineno - f.f_code.co_firstlineno


def getframe_caller(x):
    z = x
    del z
    return getframe_back(x, x + 1) + 1


warm(getframe_caller, 3)
assert getframe_caller(3) == 9


def stack_names_leaf():
    z = 0
    del z
    return [f.name for f in traceback.extract_stack()][-3:]


def stack_names_mid():
    z = 0
    del z
    return stack_names_leaf()


def stack_names_top():
    return stack_names_mid()


warm(stack_names_top)
assert stack_names_top() == ["stack_names_top", "stack_names_mid", "stack_names_leaf"]


# ---- frames that outlive their call ----


def keeps_frame(a):
    b = [a]
    return sys._getframe(0), b


def calls_keeps(a):
    z = a
    del z
    return keeps_frame(a)


warm(calls_keeps, 1)
f, b = calls_keeps(7)
g, c = calls_keeps(8)
assert f.f_locals["a"] == 7 and f.f_locals["b"] is b
assert g.f_locals["a"] == 8 and g.f_locals["b"] is c
assert f is not g


def locals_escape(a):
    d = a + 1
    return locals()


def calls_escape(a):
    z = a
    del z
    return locals_escape(a)


warm(calls_escape, 1)
assert calls_escape(4) == {"a": 4, "d": 5}


# ---- __code__ reassignment ----


def code_a():
    z = 0
    del z
    return "a"


def code_b():
    z = 0
    del z
    return "b"


def calls_code_a():
    z = 0
    del z
    return code_a()


warm(calls_code_a)
assert calls_code_a() == "a"
saved = code_a.__code__
code_a.__code__ = code_b.__code__
assert calls_code_a() == "b"
code_a.__code__ = saved
assert calls_code_a() == "a"


def swaps_own_code(n):
    # The running call's code goes away from the function while it runs.
    if n:
        swaps_own_code.__code__ = fresh_code()
    z = [1]
    del z
    return "old"


def fresh_code():
    src = "def swaps_own_code(n):\n    z = 0\n    del z\n    return 'new'\n"
    ns = {}
    exec(src, ns)
    return ns["swaps_own_code"].__code__


def calls_swaps(n):
    z = 0
    del z
    return swaps_own_code(n)


warm(calls_swaps, 0)
assert calls_swaps(1) == "old"
gc.collect()
assert calls_swaps(0) == "new"


# ---- defaults and closures rebound under a warm site ----


def with_default(a, b=2):
    z = a
    del z
    return b


def calls_default(a):
    z = a
    del z
    return with_default(a)


warm(calls_default, 1)
assert calls_default(1) == 2
with_default.__defaults__ = (5,)
assert calls_default(1) == 5
with_default.__defaults__ = None
try:
    calls_default(1)
except TypeError:
    pass
else:
    raise AssertionError("missing argument not reported")
with_default.__defaults__ = (2,)


def make_closure(k):
    def inner(a):
        z = a
        del z
        return k + a
    return inner


closures = [make_closure(1), make_closure(100)]


def calls_closures(n):
    t = 0
    for c in closures:
        t += c(n)
    return t


warm(calls_closures, 1)
assert calls_closures(1) == 103


def cell_param(a):
    def read():
        return a
    z = 0
    del z
    return read()


def calls_cell_param(a):
    z = 0
    del z
    return cell_param(a)


warm(calls_cell_param, 3)
assert calls_cell_param(3) == 3


# ---- finalizers ----

events = []


class Finalized:
    def __init__(self, name):
        self.name = name

    def __del__(self):
        events.append(self.name)


def takes_finalized(obj):
    z = 0
    del z
    return 1


def passes_finalized(name):
    takes_finalized(Finalized(name))
    events.append("after " + name)
    return 1


warm(passes_finalized, "warm")
events.clear()
passes_finalized("x")
assert events == ["x", "after x"], events


def local_finalized(name):
    obj = Finalized(name)
    z = 0
    del z
    return 1


def calls_local_finalized(name):
    local_finalized(name)
    events.append("after " + name)


warm(calls_local_finalized, "warm")
events.clear()
calls_local_finalized("y")
assert events == ["y", "after y"], events


def returns_arg(obj):
    z = 0
    del z
    return obj


class Tracked:
    pass


def weak_gone():
    o = Tracked()
    r = weakref.ref(o)
    returns_arg(o)
    del o
    return r() is None


warm(weak_gone)
assert weak_gone()


# ---- recursion ----


def recurses(n):
    z = 0
    del z
    return 0 if n == 0 else recurses(n - 1) + 1


warm(recurses, 10)
assert recurses(400) == 400


def endless(n):
    z = 0
    del z
    return endless(n + 1)


limit = sys.getrecursionlimit()
sys.setrecursionlimit(300)
try:
    for _ in range(3):
        try:
            endless(0)
        except RecursionError as e:
            assert "maximum recursion depth" in str(e)
        else:
            raise AssertionError("no RecursionError")


    def depth_of(n):
        try:
            return depth_of(n + 1)
        except RecursionError:
            return n

    deep = {depth_of(0) for _ in range(3)}
    assert len(deep) == 1, deep
finally:
    sys.setrecursionlimit(limit)
assert recurses(50) == 50


# ---- tracing and monitoring switched on in the middle of a call ----

trace_log = []


def tracer(frame, event, arg):
    trace_log.append((event, frame.f_code.co_name))
    return tracer


def traced_leaf(on):
    z = 0
    del z
    if on:
        sys.settrace(tracer)
    return 1


def traced_after():
    z = 0
    del z
    return 1


def traced_mid(on):
    z = 0
    del z
    r = traced_leaf(on)
    return r + traced_after()


warm(traced_mid, False)
trace_log.clear()
assert traced_mid(True) == 2
sys.settrace(None)
# (Only frames entered from then on are traced.)
assert trace_log[0] == ("call", "traced_after"), trace_log
assert ("return", "traced_after") in trace_log, trace_log
assert all(name == "traced_after" for _, name in trace_log), trace_log
warm(traced_mid, False)
assert traced_mid(False) == 2

mon = sys.monitoring
TOOL = 3
mon_log = []


def mon_leaf(on):
    z = 0
    del z
    if on:
        mon.use_tool_id(TOOL, "records")
        mon.register_callback(
            TOOL, mon.events.PY_RETURN, lambda code, off, val: mon_log.append(code.co_name)
        )
        mon.set_events(TOOL, mon.events.PY_RETURN)
    return 1


def mon_mid(on):
    z = 0
    del z
    return mon_leaf(on) + 1


warm(mon_mid, False)
assert mon_mid(True) == 2
mon.set_events(TOOL, 0)
mon.free_tool_id(TOOL)
assert "mon_leaf" in mon_log and "mon_mid" in mon_log, mon_log
warm(mon_mid, False)
assert mon_mid(False) == 2


# ---- generators and coroutines around direct calls ----


def gen_callee(a):
    z = 0
    del z
    return a + 1


def gen_body(n):
    for i in range(n):
        yield gen_callee(i)


def drain_gen(n):
    return sum(gen_body(n))


warm(drain_gen, 4)
assert drain_gen(4) == 10


def gen_getframe():
    z = 0
    del z
    return sys._getframe(1).f_code.co_name


def gen_looks():
    yield gen_getframe()


def drain_looks():
    return next(gen_looks())


warm(drain_looks)
assert drain_looks() == "gen_looks"


# ---- the collector during a call ----


class Node:
    def __init__(self):
        self.me = self


def collects(a):
    n = Node()
    del n
    return gc.collect(0) >= 0 and a


def calls_collects(a):
    z = [a]
    return collects(z[0])


for _ in range(500):
    calls_collects(1)
assert calls_collects(2) == 2


# ---- threads: each has its own records ----

results = []


def thread_leaf(a):
    z = 0
    del z
    return a + 1


def thread_mid(a):
    z = 0
    del z
    return thread_leaf(a) + 1


def thread_body(k):
    t = 0
    for i in range(HOT):
        t += thread_mid(i)
    results.append((k, t))


threads = [threading.Thread(target=thread_body, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
for t in threads:
    t.join()
want = sum(i + 2 for i in range(HOT))
assert sorted(results) == [(k, want) for k in range(4)], results


# ---- many arguments, polymorphic sites, methods ----


def many(a, b, c, d, e, f, g, h, i, j):
    z = 0
    del z
    return a + j


def calls_many():
    z = 0
    del z
    return many(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)


warm(calls_many)
assert calls_many() == 11


class A:
    def who(self, x):
        z = 0
        del z
        return "A", x


class B:
    def who(self, x):
        z = 0
        del z
        return "B", x


class C:
    def who(self, x, y=9):
        z = 0
        del z
        return "C", x, y


objs = [A(), B(), C()]


def calls_who(n):
    return [o.who(n) for o in objs]


warm(calls_who, 1)
assert calls_who(2) == [("A", 2), ("B", 2), ("C", 2, 9)]


# ---- the same with bodies compiled at once (tier 2 off), so that every call
# above runs directly ----

FORCED = "--forced"
if FORCED not in sys.argv and sys.implementation.name == "weavepy":
    import os
    import subprocess

    env = dict(os.environ)
    env.update(
        WEAVEPY_JIT="0",
        WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
        WEAVEPY_FRAME_JIT_HOT="50",
        WEAVEPY_JIT_SYNC="1",
    )
    r = subprocess.run([sys.executable, __file__, FORCED], env=env, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr

print("direct call records ok")
