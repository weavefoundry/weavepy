"""Small callees lowered into their compiled callers.

The frame JIT lowers a small callee's body into its caller. Each case runs
such a call long enough to inline it, then makes the body do something that
needs its frame: raise, read its frame, get traced, run a finalizer, hit the
recursion limit, or run with its code or defaults replaced. The output must
be CPython's in fresh interpreters with a low compile threshold (so the
inlined paths run), the default thresholds, and inlining off.
"""

import os
import subprocess
import sys
import unittest

CASES = {}

CASES["tracebacks"] = r'''
import traceback

N = 600


class C:
    def __init__(self, v):
        self.v = v

    def get(self, d):
        x = self.v
        y = d[x]
        return y + 1


def divide(a, b):
    c = a + 1
    return c / b


def outer(a, b):
    return divide(a, b) + 1


def run(o, d):
    total = 0
    for i in range(N):
        total += o.get(d)
    return total


def frames(tb):
    return [(f.name, f.lineno, f.line) for f in traceback.extract_tb(tb)]


print(run(C(1), {1: 2}))
try:
    run(C(9), {1: 2})
except KeyError as e:
    print(repr(e), frames(e.__traceback__))
try:
    run(C(1), {1: "s"})
except TypeError as e:
    print(repr(e), frames(e.__traceback__))
try:
    run(5, {})
except AttributeError as e:
    print(repr(e), frames(e.__traceback__))


def calls(b):
    total = 0.0
    for i in range(N):
        total += outer(i, b)
    return total


print(calls(2))
try:
    calls(0)
except ZeroDivisionError as e:
    print(repr(e), frames(e.__traceback__))


def caught(b):
    seen = 0
    for i in range(N):
        try:
            outer(i, b if i % 3 else 0)
        except ZeroDivisionError as e:
            seen += 1
            last = e
    return seen, frames(last.__traceback__)


print(caught(1))
'''

CASES["frames"] = r'''
import sys

N = 600


def leaf(a, b, flag):
    c = a + b
    if flag:
        f = sys._getframe()
        back = f.f_back
        return (f.f_code.co_name, f.f_lineno, sorted(f.f_locals), f.f_locals["c"],
                back.f_code.co_name, back.f_lineno, back.f_locals["a"])
    return c


def mid(a, flag):
    k = a * 2
    return leaf(k, 1, flag)


def run(n, at):
    out = None
    for i in range(n):
        r = mid(i, i == at)
        if i == at:
            out = r
    return out


print(run(N, N + 1))
print(run(N, N - 1))


class K:
    def m(self, x):
        y = x + 1
        if y < 0:
            return locals()
        return y


def locs(n):
    o = K()
    t = 0
    for i in range(n):
        t += o.m(i)
    d = o.m(-5)
    return t, sorted(k for k in d), d["x"], d["y"]


print(locs(N))


def names(x):
    return globals()["__name__"], x


def viaglobals(n):
    r = None
    for i in range(n):
        r = names(i)
    return r


print(viaglobals(N))


def stack_depth():
    n = 0
    f = sys._getframe()
    while f:
        n += 1
        f = f.f_back
    return n


def depth1():
    return stack_depth()


def viadepth(n):
    r = 0
    for i in range(n):
        r = depth1()
    return r


print(viadepth(N))
'''

CASES["trace_mid_loop"] = r'''
import sys

N = 600
events = []


def tracer(frame, event, arg):
    if frame.f_code.co_name in ("body", "other"):
        events.append((frame.f_code.co_name, event, frame.f_lineno))
    return tracer


def other(a):
    return a + 1


def body(a):
    b = a + 1
    c = other(b)
    return c * 2


def run(n, at):
    t = 0
    for i in range(n):
        if i == at:
            sys.settrace(tracer)
        if i == at + 2:
            sys.settrace(None)
        t += body(i)
    return t


print(run(N, N + 5))
print(run(N, N - 5))
print(events)
events.clear()


def setter(a):
    b = a * 3
    if a == 100:
        sys.settrace(tracer)
    return b


def other2(a):
    return setter(a) + 1


def run2(n):
    t = 0
    for i in range(n):
        t += other2(i)
        t += body(i)
        if i == 100:
            sys.settrace(None)
    return t


print(run2(N))
print(events)
'''

CASES["monitoring_mid_loop"] = r'''
import sys

N = 600
mon = sys.monitoring
TOOL = 3
seen = []


def on_start(code, offset):
    if code.co_name == "body":
        seen.append(("start", code.co_name))


def on_return(code, offset, value):
    if code.co_name == "body":
        seen.append(("return", value))


def body(a):
    b = a + 1
    return b * 2


def run(n, at):
    t = 0
    for i in range(n):
        if i == at:
            mon.use_tool_id(TOOL, "x")
            mon.register_callback(TOOL, mon.events.PY_START, on_start)
            mon.register_callback(TOOL, mon.events.PY_RETURN, on_return)
            mon.set_events(TOOL, mon.events.PY_START | mon.events.PY_RETURN)
        if i == at + 2:
            mon.set_events(TOOL, 0)
            mon.free_tool_id(TOOL)
        t += body(i)
    return t


print(run(N, N + 1))
print(run(N, N - 5))
print(seen)
'''

CASES["recursion_limit"] = r'''
import sys

N = 600


def leaf(a):
    return a + 1


def mid(a):
    return leaf(a) + 1


def warm(n):
    t = 0
    for i in range(n):
        t += mid(i)
    return t


print(warm(N))


def rec(k, target):
    if k == target:
        return warm_once()
    return rec(k + 1, target) + 1


def warm_once():
    return mid(1)


def probe():
    # The deepest `rec` that can still make its two inlined levels.
    base = len_stack()
    limit = sys.getrecursionlimit()
    results = []
    for target in range(limit - base - 8, limit - base + 2):
        try:
            results.append(rec(0, target))
        except RecursionError as e:
            results.append(str(e))
    return results


def len_stack():
    n = 0
    f = sys._getframe()
    while f:
        n += 1
        f = f.f_back
    return n


sys.setrecursionlimit(120)
for _ in range(3):
    print(probe())
sys.setrecursionlimit(1000)


def caller_at_limit(n):
    t = 0
    for i in range(n):
        t += leaf(i)
    sys.setrecursionlimit(len_stack() + 1)
    try:
        return t, leaf(1), mid(1)
    except RecursionError as e:
        return t, str(e)
    finally:
        sys.setrecursionlimit(1000)


print(caller_at_limit(N))
'''

CASES["rebinding"] = r'''
N = 600


def f(a, b=2):
    return a + b


def g(a, b=2):
    return a * b


def h(a, b=100, c=1000):
    return a + b + c


def call(n):
    t = 0
    for i in range(n):
        t += f(i)
    return t


print(call(N))
f.__defaults__ = (5,)
print(call(N))
f.__defaults__ = None
try:
    call(N)
except TypeError as e:
    print(e)
f.__defaults__ = (2,)
print(call(N))
f.__code__ = g.__code__
print(call(N))
f.__code__ = h.__code__
f.__defaults__ = (7, 8)
print(call(N))


def two(a):
    return a + 1


def rebinding_global(n):
    global two
    t = 0
    old = two
    for i in range(n):
        t += two(i)
        if i == n // 2:
            two = lambda a: -a
    two = old
    return t


print(rebinding_global(N))


class A:
    def m(self):
        return 1

    def n(self, x=3):
        return x


class B(A):
    def m(self):
        return 2


def method(o, n):
    t = 0
    for i in range(n):
        t += o.m() + o.n()
    return t


a, b = A(), B()
print(method(a, N), method(b, N), method(a, N))
A.m = lambda self: 10
print(method(a, N), method(b, N))
a.m = lambda: 100
print(method(a, N))
del a.m
A.n = lambda self, x=30: x
print(method(a, N))
a.__class__ = B
print(method(a, N))


def poly(xs, n):
    t = 0
    for i in range(n):
        t += xs[i % len(xs)].m()
    return t


print(poly([A(), B(), A(), B()], N))


# The same code under other globals.
K = 1


def read_global(x):
    return K + x


other = type(read_global)(read_global.__code__, {"K": 100}, "read_global")


def read(fn, n):
    t = 0
    for i in range(n):
        t += fn(1)
    return t


print(read(read_global, N), read(other, N), read(read_global, N))
K = 2
print(read(read_global, N))
del K
try:
    read(read_global, N)
except NameError as e:
    print(e)
K = 3


def shadow_len(x):
    return len(x)


def lens(n):
    t = 0
    for i in range(n):
        t += shadow_len("abc")
    return t


print(lens(N))
len = lambda x: 50
print(lens(N))
del len
print(lens(N))
try:
    shadow_len(5)
except TypeError as e:
    print(e)


class Swap:
    def m(self, x):
        return x + 1

    def other(self, x):
        return x + 100

    def __add__(self, o):
        return 1

    def __sub__(self, o):
        return 100


def swap_method(o, n):
    t = 0
    for i in range(n):
        t += o.m(i) + (o + o)
    return t


o = Swap()
print(swap_method(o, N))
Swap.m.__code__ = Swap.other.__code__
print(swap_method(o, N))
Swap.__add__.__code__ = Swap.__sub__.__code__
print(swap_method(o, N))
'''

CASES["release_order"] = r'''
N = 600
log = []


class D:
    def __init__(self, name):
        self.name = name

    def __del__(self):
        log.append(self.name)


def three_locals(i):
    x = D("x")
    y = D("y")
    z = D("z")
    return i


def params_and_locals(a, b):
    c = D("c")
    d = D("d")
    return 1


def rebound(a, b):
    a = b
    b = D("new")
    return 2


class M:
    def method(self, a, b):
        c = D("c")
        return 3


def show(run):
    seen = []
    for i in range(N):
        log.clear()
        run(i)
        if log not in seen:
            seen.append(list(log))
    print(seen)


def run_locals(i):
    three_locals(i)
    log.append("caller")


def run_params(i):
    params_and_locals(D("a"), D("b"))
    log.append("caller")


def run_rebound(i):
    rebound(D("a"), D("b"))
    log.append("caller")


m = M()


def run_method(i):
    m.method(D("a"), D("b"))
    log.append("caller")


show(run_locals)
show(run_params)
show(run_rebound)
show(run_method)
'''

CASES["finalizers"] = r'''
import gc
import weakref

N = 600
log = []


class D:
    def __init__(self, name):
        self.name = name

    def __del__(self):
        log.append(self.name)


def temp(i):
    x = D("x")
    y = i
    log.append("body")
    return i


def param(a, b):
    log.append("in")
    return 1


def rebinds(a):
    log.append("before")
    a = 5
    log.append("after")
    return a


def returns_arg(a):
    b = a
    return b


def replaced(i):
    x = D("first")
    x = D("second")
    log.append("mid")
    return i


def pop(i):
    D("dropped")
    log.append("popped")
    return i


def warm(fn, n, *args):
    t = 0
    for i in range(n):
        log.clear()
        t += fn(i, *args)
    return t


def run_temp(n):
    t = 0
    for i in range(n):
        log.clear()
        t += temp(i)
        log.append("caller")
    return t, list(log)


print(run_temp(N))


def run_param(n):
    for i in range(n):
        log.clear()
        param(D("a"), 1)
        log.append("caller")
    return list(log)


print(run_param(N))


def run_param_local(n):
    o = D("held")
    for i in range(n):
        log.clear()
        param(o, 1)
        log.append("caller")
    del o
    return list(log)


print(run_param_local(N))
print(log)


def run_rebind(n):
    for i in range(n):
        log.clear()
        rebinds(D("arg"))
        log.append("caller")
    return list(log)


print(run_rebind(N))


def run_rebind_local(n):
    o = D("local")
    for i in range(n):
        log.clear()
        rebinds(o)
        log.append("caller")
    return list(log)


print(run_rebind_local(N))


def run_returns(n):
    for i in range(n):
        log.clear()
        returns_arg(D("r"))
        log.append("caller")
    return list(log)


print(run_returns(N))


def run_replaced(n):
    for i in range(n):
        log.clear()
        replaced(i)
        log.append("caller")
    return list(log)


print(run_replaced(N))


def run_pop(n):
    for i in range(n):
        log.clear()
        pop(i)
        log.append("caller")
    return list(log)


print(run_pop(N))


class W:
    pass


def weak_gone(o):
    r = weakref.ref(o)
    return r


def run_weak(n):
    for i in range(n):
        r = weak_gone(W())
        assert r() is None
    return "dead"


print(run_weak(N))


frames_seen = []


class F:
    def __del__(self):
        import sys

        f = sys._getframe(1)
        frames_seen.append(f.f_code.co_name)


def drops(i):
    x = F()
    x = None
    return i


def run_frames(n):
    for i in range(n):
        frames_seen.clear()
        drops(i)
    return list(frames_seen)


print(run_frames(N))
'''

CASES["values_and_arguments"] = r'''
N = 600


def big(a, b):
    return a * b + a


def run_big(n):
    t = 0
    for i in range(n):
        t = big(i, 1 << 40)
    r = big(1 << 62, 1 << 62)
    return t, r


print(run_big(N))


def swap(a, b):
    a, b = b, a
    return a - b


def rebind(a, b):
    a = a + 1
    b = b * 2
    return a + b


def same(a, b):
    return a is b, a == b


def nothing(a):
    pass


def konst():
    return "constant"


def default_list(x, acc=[]):
    acc.append(x)
    return len(acc)


def run_args(n):
    r = []
    for i in range(n):
        r = [
            swap(i, 1),
            swap(1.5, i),
            rebind(i, 2),
            same(i, i),
            nothing(i),
            konst(),
            default_list(i),
            swap(True, 2),
            rebind(2.5, 1.5),
        ]
    return r


print(run_args(N))


class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def getx(self):
        return self.x

    def sety(self, v):
        self.y = v

    def norm(self):
        return self.x * self.x + self.y * self.y

    def add(self, o):
        return P(self.x + o.x, self.y + o.y)

    def rebind_self(self):
        self = P(1, 2)
        return self.x

    def idx(self, xs, i):
        return xs[i]

    def put(self, xs, i, v):
        xs[i] = v

    def mix(self, d, k):
        if k in d:
            return d[k]
        d[k] = self.x
        return -1

    def app(self, xs, v):
        xs.append(v)
        return len(xs)


def run_methods(n):
    p = P(1, 2)
    xs = [0] * 8
    d = {}
    t = 0
    for i in range(n):
        p.sety(i)
        t += p.getx() + p.norm() + p.rebind_self()
        t += p.idx(xs, i % 8)
        p.put(xs, i % 8, i)
        t += p.mix(d, i % 5)
        q = p.add(p)
        t += q.x
        t += p.app(xs, i)
        xs.pop()
    return t, xs, d


print(run_methods(N))


def errors(p, xs, n):
    out = []
    for args in ((p, xs, 99), (p, None, 0), (p, xs, "k")):
        try:
            out.append(P.idx(*args))
        except Exception as e:
            out.append(repr(e))
    return out


print(errors(P(1, 2), [1], N))


def call_error(n):
    p = P(1, 2)
    xs = [1, 2]
    seen = []
    for i in range(n):
        try:
            p.idx(xs, 5 if i % 100 == 0 else 1)
        except IndexError as e:
            seen.append(i)
    return seen[:4], len(seen)


print(call_error(N))


def wrong_args(n):
    r = None
    for i in range(n):
        r = nothing(i)
    fn = nothing
    for args in ((), (1, 2)):
        try:
            fn(*args)
        except TypeError as e:
            r = str(e)
            print(r)
    return r


print(wrong_args(N))


def gen(a):
    yield a
    yield a + 1


def closure(a):
    def inner(b):
        return a + b
    return inner


def make(n):
    t = 0
    for i in range(n):
        t += sum(gen(i))
        t += closure(i)(1)
    return t


print(make(N))


def keys(a, b=1, *, c=2):
    return a + b + c


def kwcalls(n):
    t = 0
    for i in range(n):
        t += keys(i) + keys(i, 2) + keys(i, c=3) + keys(b=4, a=i)
    return t


print(kwcalls(N))


class H:
    def __add__(self, o):
        return "dunder-add"

    def __eq__(self, o):
        return "dunder-eq"

    def __bool__(self):
        return False

    def __getitem__(self, k):
        return ("item", k)

    __hash__ = None


def combine(a, b):
    return a + b


def equal(a, b):
    return a == b


def item(a, k):
    return a[k]


def truthy(a):
    if a:
        return "yes"
    return "no"


def dunders(n):
    r = None
    for i in range(n):
        r = (combine(i, 1), equal(i, 1), item([1, 2], 0), truthy(i))
    h = H()
    return r, combine(h, 1), equal(h, 1), item(h, 3), truthy(h)


print(dunders(N))


@property
def _prop(self):
    return ("prop", self.x)


class Q:
    x = 3
    y = _prop

    def read(self):
        return self.y


def props(n):
    q = Q()
    r = None
    for i in range(n):
        r = q.read()
    return r


print(props(N))
'''

CASES["deep_exits"] = r'''
import sys

N = 600


class T:
    def __init__(self):
        self.v = 1
        self.items = [1, 2, 3]

    def a(self, x):
        y = x + self.v
        return self.b(y, 2) + 1

    def b(self, x, z):
        w = x * z
        return self.c(w) + z

    def c(self, q):
        r = q - 1
        if q == -1000:
            f = sys._getframe()
            names = []
            while f and len(names) < 5:
                names.append((f.f_code.co_name, f.f_lineno))
                f = f.f_back
            raise ValueError(names, sorted(sys._getframe().f_locals))
        return self.items[r % 3]

    def d(self, k):
        return self.items[k]


def run(n, x):
    t = T()
    s = 0
    for i in range(n):
        s += t.a(i)
    try:
        s += t.a(x)
    except ValueError as e:
        s = e.args
    return s


print(run(N, 5))
print(run(N, -501))


def tb(n):
    t = T()
    s = 0
    for i in range(n):
        s += t.d(i % 3)
    try:
        t.d(50)
    except IndexError as e:
        x = e.__traceback__
        while x:
            print(x.tb_frame.f_code.co_name, x.tb_lineno)
            x = x.tb_next
    return s


print(tb(N))


class Bad:
    v = "s"

    def __init__(self):
        self.items = []

    a = T.a
    b = T.b
    c = T.c


def leaves(n):
    t = T()
    s = 0
    for i in range(n):
        s += t.a(i)
    t.v = 2
    s += t.a(0)
    t.items = None
    try:
        t.a(0)
    except TypeError as e:
        s = str(e)
    return s


print(leaves(N))


def exc_info_in_callee():
    return sys.exc_info()[0]


def in_handler(n):
    r = None
    for i in range(n):
        try:
            raise KeyError(i)
        except KeyError:
            r = exc_info_in_callee()
    return r, exc_info_in_callee()


print(in_handler(N))
'''

CASES["operators"] = r'''
import sys
import traceback

N = 600


class V:
    def __init__(self, x):
        self.x = x

    def __add__(self, o):
        return V(self.x + o.x)

    def __sub__(self, o):
        if o.x == -1:
            return NotImplemented
        return V(self.x - o.x)

    def __mul__(self, k):
        return V(self.x * k)

    def __truediv__(self, o):
        return self.x / o.x

    def __rsub__(self, o):
        return "rsub"

    def __repr__(self):
        return "V(%r)" % (self.x,)


class W(V):
    def __radd__(self, o):
        return "W.radd"


class Z:
    x = -1

    def __rsub__(self, o):
        return "Z.rsub"


def add(a, b, n):
    r = None
    for i in range(n):
        r = a + b
    return r


def sub(a, b, n):
    r = None
    for i in range(n):
        r = a - b
    return r


def mul(a, k, n):
    r = None
    for i in range(n):
        r = a * k
    return r


def div(a, b, n):
    r = None
    for i in range(n):
        r = a / b
    return r


print(add(V(1), V(2), N), sub(V(5), V(2), N), mul(V(2), 3, N), mul(V(2), 1.5, N))
print(add(V(1), W(2), 1), add(W(1), V(2), 1), sub(V(5), V(-1), 1) if False else None)
# The right operand's reflected method, and a bare NotImplemented.
print(sub(V(5), Z(), 3))
try:
    sub(V(5), V(-1), 3)
except TypeError as e:
    print(e)


def not_impl(n):
    a, b, c = V(5), V(2), V(-1)
    out = None
    for i in range(n):
        out = a - b
    try:
        a - c
    except TypeError as e:
        return out, str(e)


print(not_impl(N))
try:
    div(V(1), V(0), 3)
except ZeroDivisionError as e:
    print([(f.name, f.lineno) for f in traceback.extract_tb(e.__traceback__)])


def div_hot(n):
    a, b = V(1), V(2)
    r = 0
    for i in range(n):
        r = a / b
    b.x = 0
    try:
        a / b
    except ZeroDivisionError as e:
        return r, [(f.name, f.lineno) for f in traceback.extract_tb(e.__traceback__)]


print(div_hot(N))


def patched(n):
    a, b = V(1), V(2)
    out = []
    for i in range(n):
        out = a + b
    V.__add__ = lambda s, o: "patched"
    r = a + b
    del V.__add__
    try:
        a + b
    except TypeError as e:
        return out, r, str(e)


print(patched(N))


class P:
    def __init__(self, x):
        self.x = x

    def __add__(self, o):
        f = sys._getframe()
        return (f.f_code.co_name, f.f_back.f_code.co_name, self.x + o.x)

    def __iadd__(self, o):
        self.x += o.x
        return self


def frames(n):
    a, b = P(1), P(2)
    r = None
    for i in range(n):
        r = a + b
    return r


print(frames(N))


def inplace(n):
    a, b = P(0), P(1)
    for i in range(n):
        a += b
    return a.x


print(inplace(N))


class Q:
    def __init__(self, x):
        self.x = x

    def __add__(self, o):
        return Q(self.x + o.x)

    def __del__(self):
        log.append(self.x)


log = []


def released(n):
    a = Q(1)
    for i in range(n):
        log.clear()
        Q(2) + a
        log.append("after")
    return list(log)


print(released(N))


class S:
    __slots__ = ("x",)

    def __init__(self, x):
        self.x = x

    def __add__(self, o):
        return self.x + o.x

    def __sub__(self, o):
        raise KeyError(self.x)


def slots(n):
    a, b = S(1), S(2)
    t = 0
    for i in range(n):
        t += a + b
    try:
        a - b
    except KeyError as e:
        return t, repr(e), [f.name for f in traceback.extract_tb(e.__traceback__)]


print(slots(N))
'''

CASES["handlers_and_containers"] = r'''
import sys
import traceback

N = 600


def lookup(d, k):
    try:
        return d[k]
    except KeyError:
        return -1


def safe(d, k):
    v = 0
    try:
        v = d[k]
    except KeyError as e:
        v = (sys.exc_info()[0].__name__, str(e))
    finally:
        d["finally"] = d.get("finally", 0) + 1
    return v


def reraises(d, k):
    try:
        return d[k]
    except KeyError:
        raise ValueError("no " + str(k))


def outer(d, k):
    try:
        return reraises(d, k) + 1
    except ValueError as e:
        return str(e), [f.name for f in traceback.extract_tb(e.__traceback__)]


def run(n):
    d = {i: i for i in range(10)}
    t = 0
    for i in range(n):
        t += lookup(d, i % 10)
    r = [lookup(d, 99), safe(d, 1), safe(d, 99), outer(d, 2), outer(d, 77)]
    for i in range(n):
        r[0] += lookup(d, 1000 + i)
    return t, r, d["finally"]


print(run(N))


class Ctx:
    def __init__(self):
        self.log = []

    def __enter__(self):
        self.log.append("enter")
        return self

    def __exit__(self, *exc):
        self.log.append(("exit", exc[0].__name__ if exc[0] else None))
        return exc[0] is KeyError


def with_body(c, d, k):
    with c:
        return d[k]


def run_with(n):
    c = Ctx()
    d = {1: 1}
    t = 0
    for i in range(n):
        c.log.clear()
        t += with_body(c, d, 1)
    log = list(c.log)
    c.log.clear()
    return t, log, with_body(c, d, 2), list(c.log)


print(run_with(N))


def slice_it(xs, a, b):
    return xs[a:b]


def store_slice(xs, a, b, v):
    xs[a:b] = v
    return len(xs)


def del_item(xs, i):
    del xs[i]
    return len(xs)


def comp(n):
    return [i * 2 for i in range(n)]


def dicts(k, v):
    return {k: v, "x": 1}


def del_local(a):
    b = a + 1
    del b
    try:
        return b
    except UnboundLocalError as e:
        return str(e)


def unpacks(t):
    a, *b = t
    return a, b


def sets(a, b):
    s = {a, b}
    return len(s), sorted(s)


class D:
    def __init__(self):
        self.x = 1

    def drop(self):
        del self.x
        return hasattr(self, "x")

    def put(self):
        self.x = 2


def run_containers(n):
    xs = list(range(10))
    r = None
    for i in range(n):
        r = (
            slice_it(xs, 1, 3),
            store_slice(xs, 0, 1, [0]),
            comp(3),
            dicts("k", i),
            del_local(i),
            unpacks((1, 2, 3)),
            sets(1, 2),
        )
        xs.append(i)
        del_item(xs, 0)
    d = D()
    for i in range(n):
        d.put()
        r2 = d.drop()
    return r, xs[:3], r2, comp(0), slice_it("abc", 1, None), unpacks([1])


print(run_containers(N))


def bad_slice(x):
    return x[1:2]


def run_bad(n):
    for i in range(n):
        bad_slice([1, 2, 3])
    try:
        bad_slice(5)
    except TypeError as e:
        return str(e), [f.name for f in traceback.extract_tb(e.__traceback__)]


print(run_bad(N))


def looped(xs):
    t = 0
    for x in xs:
        if x < 0:
            return "negative"
        t += x
    return t


def loops(n):
    r = None
    for i in range(n):
        r = looped([1, 2, 3]), looped((1, -1)), looped(range(3))
    def gen():
        yield 1
        yield 2
    return r, looped(gen()), looped({1: 2}), looped("")


print(loops(N))
'''

CASES["lifetimes_of_classes"] = r'''
import gc
import sys
import weakref

N = 600


def hot(o, n):
    t = 0
    for i in range(n):
        t += o.m(i) + (o + o)
    return t


def make():
    class C:
        def m(self, x):
            return x + 1

        def __add__(self, o):
            return 2

    return C


C = make()
print(hot(C(), N))
wc = weakref.ref(C)
del C
gc.collect()
print(wc() is None)
C2 = make()
print(hot(C2(), N))


class Self:
    def m(self, x):
        Self.m = other
        gc.collect()
        return x + 1

    def __add__(self, o):
        return 0


def other(self, x):
    return x + 100


print(hot(Self(), 3))


class Late:
    def m(self, x):
        if x == N - 10:
            replace_method()
        return x

    def __add__(self, o):
        return 0


def replace_method():
    Late.m = other
    gc.collect()
    return 1


print(hot(Late(), N))


class Plain:
    def m(self, x):
        return x

    def __add__(self, o):
        return 0


def set_on_class(o, k):
    type(o).K = k
    return k


def set_many(n):
    o = Plain()
    for i in range(n):
        set_on_class(o, i)
    return Plain.K


print(set_many(N))
print(hot(Plain(), N))


class Mutates:
    def m(self, x):
        self.__class__ = Other
        return self.who()

    def __add__(self, o):
        return 0

    def who(self):
        return 1


class Other(Mutates):
    def who(self):
        return 2

    def m(self, x):
        self.__class__ = Mutates
        return self.who() * 10


def flip(n):
    o = Mutates()
    t = 0
    for i in range(n):
        t += o.m(i)
    return t


print(flip(N))


class Del:
    def go(self):
        del self.v
        return hasattr(self, "v")


def dels(n):
    o = Del()
    r = None
    for i in range(n):
        o.v = 1
        r = o.go()
    return r


print(dels(N))
'''

CASES["properties"] = r'''
import sys
import traceback

N = 600


class P:
    def __init__(self, x):
        self._x = x

    @property
    def x(self):
        return self._x

    @property
    def doubled(self):
        return self.x * 2

    @property
    def raises(self):
        return self.missing

    @property
    def frame(self):
        f = sys._getframe()
        return f.f_code.co_name, f.f_back.f_code.co_name

    @property
    def fallback(self):
        raise AttributeError("from getter")

    def __getattr__(self, name):
        return "getattr:" + name


class Q:
    def __init__(self, x):
        self._x = x

    @property
    def x(self):
        if self._x < 0:
            raise ValueError("negative")
        return self._x

    @x.setter
    def x(self, v):
        self._x = v


def read(o, n):
    t = 0
    for i in range(n):
        t += o.x
    return t


def doubled(o, n):
    t = 0
    for i in range(n):
        t += o.doubled
    return t


def frame(o, n):
    r = None
    for i in range(n):
        r = o.frame
    return r


def odd(o, n):
    r = None
    for i in range(n):
        r = o.raises, o.fallback
    return r


print(read(P(1), N), doubled(P(2), N), frame(P(3), N), odd(P(4), N))
q = Q(1)
print(read(q, N))
q.x = -1
try:
    read(q, N)
except ValueError as e:
    print([(f.name, f.lineno) for f in traceback.extract_tb(e.__traceback__)])
q.x = 1
Q.x = 5
print(read(q, N))
del Q.x
q.x = 7
print(read(q, N))


class R:
    @property
    def x(self):
        return 1


class S:
    x = 2


def read2(o, n):
    t = 0
    for i in range(n):
        t += o.x
    return t


print(read2(R(), N), read2(S(), N), read2(R(), N))
r = R()
r.__dict__["x"] = 50
print(read2(r, N))
'''

ENVS = [
    {"WEAVEPY_FRAME_JIT_HOT": "20", "WEAVEPY_JIT_SYNC": "1"},
    {"WEAVEPY_FRAME_JIT_HOT": "20", "WEAVEPY_JIT_SYNC": "1", "WEAVEPY_TIER2": "0"},
    {},
    {"WEAVEPY_FRAME_JIT_INLINE": "0", "WEAVEPY_FRAME_JIT_HOT": "20", "WEAVEPY_JIT_SYNC": "1"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    'tracebacks': '1800\nKeyError(9) [(\'<module>\', 39, \'run(C(9), {1: 2})\'), (\'run\', 29, \'total += o.get(d)\'), (\'get\', 13, \'y = d[x]\')]\nTypeError(\'can only concatenate str (not "int") to str\') [(\'<module>\', 43, \'run(C(1), {1: "s"})\'), (\'run\', 29, \'total += o.get(d)\'), (\'get\', 14, \'return y + 1\')]\nAttributeError("\'int\' object has no attribute \'get\'") [(\'<module>\', 47, \'run(5, {})\'), (\'run\', 29, \'total += o.get(d)\')]\n90750.0\nZeroDivisionError(\'division by zero\') [(\'<module>\', 61, \'calls(0)\'), (\'calls\', 55, \'total += outer(i, b)\'), (\'outer\', 23, \'return divide(a, b) + 1\'), (\'divide\', 19, \'return c / b\')]\n(200, [(\'caught\', 70, \'outer(i, b if i % 3 else 0)\'), (\'outer\', 23, \'return divide(a, b) + 1\'), (\'divide\', 19, \'return c / b\')])\n',
    'frames': "None\n('leaf', 12, ['a', 'b', 'back', 'c', 'f', 'flag'], 1199, 'mid', 19, 599)\n(180300, ['self', 'x', 'y'], -5, -4)\n('__main__', 599)\n4\n",
    'trace_mid_loop': "361800\n361800\n[('body', 'call', 18), ('body', 'line', 19), ('body', 'line', 20), ('other', 'call', 14), ('other', 'line', 15), ('other', 'return', 15), ('body', 'line', 21), ('body', 'return', 21), ('body', 'call', 18), ('body', 'line', 19), ('body', 'line', 20), ('other', 'call', 14), ('other', 'line', 15), ('other', 'return', 15), ('body', 'line', 21), ('body', 'return', 21)]\n901500\n[('body', 'call', 18), ('body', 'line', 19), ('body', 'line', 20), ('other', 'call', 14), ('other', 'line', 15), ('other', 'return', 15), ('body', 'line', 21), ('body', 'return', 21)]\n",
    'monitoring_mid_loop': "360600\n360600\n[('start', 'body'), ('return', 1192), ('start', 'body'), ('return', 1194)]\n",
    'recursion_limit': "180900\n[112, 113, 114, 115, 116, 117, 'maximum recursion depth exceeded', 'maximum recursion depth exceeded', 'maximum recursion depth exceeded', 'maximum recursion depth exceeded']\n[112, 113, 114, 115, 116, 117, 'maximum recursion depth exceeded', 'maximum recursion depth exceeded', 'maximum recursion depth exceeded', 'maximum recursion depth exceeded']\n[112, 113, 114, 115, 116, 117, 'maximum recursion depth exceeded', 'maximum recursion depth exceeded', 'maximum recursion depth exceeded', 'maximum recursion depth exceeded']\n(180300, 2, 3)\n",
    'rebinding': "180900\n182700\nf() missing 1 required positional argument: 'b'\n180900\n359400\n188700\n-89099\n2400 3000 2400\n7800 3000\n61800\n24000\n19200\n3600\n1200 60600 1200\n1800\nname 'K' is not defined\n1800\n30000\n1800\nobject of type 'int' has no len()\n180900\n240300\n299700\n",
    'release_order': "[['z', 'y', 'x', 'caller']]\n[['d', 'c', 'b', 'a', 'caller']]\n[['a', 'new', 'b', 'caller']]\n[['c', 'b', 'a', 'caller']]\n",
    'finalizers': "(179700, ['body', 'x', 'caller'])\n['in', 'a', 'caller']\n['in', 'caller', 'held']\n['in', 'caller', 'held']\n['before', 'arg', 'after', 'caller']\n['before', 'after', 'caller']\n['r', 'caller']\n['first', 'mid', 'second', 'caller']\n['dropped', 'popped', 'caller']\ndead\n['drops']\n",
    'values_and_arguments': '(658607465038423, 21267647932558653971072598982912901120)\n[-598, 597.5, 604, (True, True), None, \'constant\', 600, 1, 6.5]\n(72004026, [592, 593, 594, 595, 596, 597, 598, 599], {0: 1, 1: 1, 2: 1, 3: 1, 4: 1})\n["IndexError(\'list index out of range\')", \'TypeError("\\\'NoneType\\\' object is not subscriptable")\', "TypeError(\'list indices must be integers or slices, not str\')"]\n([0, 100, 200, 300], 6)\nnothing() missing 1 required positional argument: \'a\'\nnothing() takes 1 positional argument but 2 were given\nnothing() takes 1 positional argument but 2 were given\n540300\n729000\n((600, False, 1, \'yes\'), \'dunder-add\', \'dunder-eq\', (\'item\', 3), \'no\')\n(\'prop\', 3)\n',
    'deep_exits': "3006\n([('c', 26), ('b', 18), ('a', 14), ('run', 41), ('<module>', 48)], ['f', 'names', 'q', 'r', 'self'])\ntb 57\nd 32\n1200\n'NoneType' object is not subscriptable\n(<class 'KeyError'>, None)\n",
    'operators': 'V(3) V(3) V(6) V(3.0)\nW.radd V(3) None\nZ.rsub\nunsupported operand type(s) for -: \'V\' and \'V\'\n(V(3), "unsupported operand type(s) for -: \'V\' and \'V\'")\n[(\'<module>\', 96), (\'div\', 69), (\'__truediv__\', 24)]\n(0.5, [(\'div_hot\', 108), (\'__truediv__\', 24)])\n(V(3), \'patched\', "unsupported operand type(s) for +: \'V\' and \'V\'")\n(\'__add__\', \'frames\', 3)\n600\n[2, 3, \'after\']\n(1800, \'KeyError(1)\', [\'slots\', \'__sub__\'])\n',
    'handlers_and_containers': '(2700, [-601, 1, (\'KeyError\', \'99\'), 3, (\'no 77\', [\'outer\', \'reraises\'])], 2)\n(600, [\'enter\', (\'exit\', None)], None, [\'enter\', (\'exit\', \'KeyError\')])\n(([590, 591], 10, [0, 2, 4], {\'k\': 599, \'x\': 1}, "cannot access local variable \'b\' where it is not associated with a value", (1, [2, 3]), (2, [1, 2])), [590, 591, 592], False, [], \'bc\', (1, []))\n("\'int\' object is not subscriptable", [\'run_bad\', \'bad_slice\'])\n((6, \'negative\', 3), 3, 1, 0)\n',
    'lifetimes_of_classes': '181500\nTrue\n181500\n204\n180600\n599\n179700\n3600\nFalse\n',
    'properties': "600 2400 ('frame', 'frame') ('getattr:missing', 'getattr:fallback')\n600\n[('<module>', 85), ('read', 55), ('x', 44)]\n3000\n4200\n600 1200 600\n600\n",
}


def run_case(name, extra=None):
    env = dict(os.environ, **(extra or {}))
    result = subprocess.run(
        [sys.executable, "-c", CASES[name]],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class InlineTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in CASES:
    setattr(InlineTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
