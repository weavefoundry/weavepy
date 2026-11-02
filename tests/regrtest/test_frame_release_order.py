"""The order in which a finished frame releases what it holds.

CPython's `_PyFrame_ClearLocals` walks a frame from the top of its operand
stack down to its first local: the stack, then the free variables, then the
cell variables that aren't arguments, then the locals, an argument's cell in
its argument's slot. Finalizers and weakref callbacks see that order on every
way out of a frame, so each case below runs one way out in a loop long enough
for every tier to compile it, and prints each distinct order it saw (so a tier
that differs shows as a second order). The output must be CPython's in fresh
interpreters with every tier on, with a low compile threshold, with a low
frame-JIT threshold, with the method tier off and with the compilers off.

Running this file under CPython checks the expected output itself.
"""

import os
import subprocess
import sys
import unittest

PRELUDE = r'''
import gc
import sys

N = 300
log = []


class D:
    def __init__(self, n):
        self.n = n

    def __del__(self):
        log.append(self.n)


def show(fn):
    """Each distinct finalization order of `fn`, in the order first seen."""
    seen = []
    for _ in range(N):
        log.clear()
        try:
            fn()
        except ValueError:
            log.append("raised")
        order = " ".join(log)
        if order not in seen:
            seen.append(order)
    log.clear()
    print(seen)
'''

CASES = {}

CASES["plain_return"] = r'''
def none():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
def value():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    return 1
def local():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    return b
def deleted():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    del b
    log.append("del")
def rebound():
    a, b, c = D("a"), D("b"), D("c")
    b = None
    log.append("rebound")
show(none)
show(value)
show(lambda: local() and log.append("got"))
show(deleted)
show(rebound)
'''

CASES["arguments"] = r'''
def leaf(a, b, c):
    return 1
def empty(a, b, c):
    pass
def body(a, b, c):
    x = a
    return x is b
def more(a, b, c):
    d = D("d")
def default(a, b=None, c=None):
    return 1
def keywords(a, b=None, *, c=None):
    return 1
def star(*args):
    return 1
def both(a, *args, b=None, **kw):
    return 1
def locals_after_star(a, *args):
    b = D("b")
class C:
    def m(self, a, b):
        return 1
    @staticmethod
    def s(a, b):
        return 1
    @classmethod
    def k(cls, a, b):
        return 1
    def __init__(self, a=None, b=None):
        pass
    def __call__(self, a, b):
        return 1
c = C()
show(lambda: leaf(D("a"), D("b"), D("c")))
show(lambda: empty(D("a"), D("b"), D("c")))
show(lambda: body(D("a"), D("b"), D("c")))
show(lambda: more(D("a"), D("b"), D("c")))
show(lambda: default(D("a")))
show(lambda: default(D("a"), D("b")))
show(lambda: keywords(D("a"), b=D("b"), c=D("c")))
show(lambda: keywords(D("a"), c=D("c"), b=D("b")))
show(lambda: star(D("a")))
show(lambda: both(D("a"), D("x"), b=D("c"), d=D("d")))
show(lambda: locals_after_star(D("a"), D("x")))
show(lambda: leaf(*(D("a"), D("b"), D("c"))))
show(lambda: leaf(*[D("a"), D("b"), D("c")]))
show(lambda: leaf(**{"a": D("a"), "b": D("b"), "c": D("c")}))
show(lambda: c.m(D("a"), D("b")))
show(lambda: c.s(D("a"), D("b")))
show(lambda: c.k(D("a"), D("b")))
show(lambda: C(D("a"), D("b")))
show(lambda: c(D("a"), D("b")))
show(lambda: leaf(leaf(D("a"), D("b"), D("c")), D("d"), D("e")))
'''

# A native caller's call of a Python function: the callee's frame is the
# same, but the path to it isn't.
CASES["native_callers"] = r'''
def locals_too(a, b):
    c = D("c")
show(lambda: list(map(lambda x: locals_too(D("a"), D("b")), [1])))
show(lambda: sorted([1], key=lambda x: locals_too(D("a"), D("b"))))
show(lambda: next(map(lambda x: locals_too(D("a"), D("b")), [1])))
'''

CASES["unwinding"] = r'''
def raises():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    raise ValueError
def nested():
    x, y = D("x"), D("y")
    raises()
def caught_in_caller():
    try:
        nested()
    except ValueError:
        log.append("caught")
    log.append("after")
def caught_here():
    a, b = D("a"), D("b")
    try:
        c, d = D("c"), D("d")
        raise ValueError
    except ValueError:
        log.append("caught")
    log.append("after")
def in_finally():
    a, b = D("a"), D("b")
    try:
        c, d = D("c"), D("d")
        return 1
    finally:
        log.append("finally")
def in_with():
    class CM:
        def __enter__(self):
            return D("e")
        def __exit__(self, *a):
            log.append("exit")
    a = D("a")
    with CM() as e:
        b = D("b")
        return 1
def on_stack_list():
    a, b = D("a"), D("b")
    x = [D("s"), D("t"), 1 / 0]
def on_stack_call():
    a, b = D("a"), D("b")
    print(D("s"), D("t"), 1 / 0)
def stack_caught(fn):
    try:
        fn()
    except ZeroDivisionError:
        log.append("caught")
    log.append("after")
def on_stack_caught_here():
    a, b = D("a"), D("b")
    try:
        x = [D("s"), D("t"), 1 / 0]
    except ZeroDivisionError:
        log.append("caught")
def return_in_for():
    a = D("a")
    for x in [D("i")]:
        b = D("b")
        return 1
show(raises)
show(nested)
show(caught_in_caller)
show(caught_here)
show(in_finally)
show(in_with)
show(lambda: stack_caught(on_stack_list))
show(lambda: stack_caught(on_stack_call))
show(on_stack_caught_here)
show(return_in_for)
'''

CASES["generators"] = r'''
def g():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    yield 1
def args(a, b, c):
    yield
def looped():
    a, b = D("a"), D("b")
    for i in range(3):
        yield i
def caught():
    a, b = D("a"), D("b")
    try:
        yield 1
    except GeneratorExit:
        log.append("exit")
        c = D("c")
def stacked():
    a, b = D("a"), D("b")
    x = [D("s"), (yield 1), D("t")]
def stacked_top():
    a, b = D("a"), D("b")
    x = [D("s"), D("t"), (yield 1)]
def closed():
    it = g(); next(it); it.close(); log.append("after")
def dropped():
    it = g(); next(it); del it; log.append("after")
def exhausted():
    it = g(); next(it); next(it, None); log.append("after")
def stopped():
    it = g(); next(it)
    try:
        next(it)
    except StopIteration:
        log.append("stop")
    log.append("after")
def for_loop():
    for _ in g():
        pass
    log.append("after")
def for_break():
    for _ in g():
        break
    log.append("after")
def unstarted_dropped():
    it = args(D("a"), D("b"), D("c")); del it; log.append("after")
def unstarted_closed():
    it = args(D("a"), D("b"), D("c")); it.close(); log.append("after")
def thrown():
    it = g(); next(it)
    try:
        it.throw(ValueError)
    except ValueError:
        log.append("caught")
    log.append("after")
def looped_closed():
    it = looped(); next(it); next(it); it.close(); log.append("after")
def looped_dropped():
    it = looped(); next(it); next(it); del it; log.append("after")
def handler_closed():
    it = caught(); next(it); it.close(); log.append("after")
def handler_dropped():
    it = caught(); next(it); del it; log.append("after")
def stack_dropped():
    it = stacked(); next(it); del it; log.append("after")
def stack_closed():
    it = stacked(); next(it); it.close(); log.append("after")
def stack_top_dropped():
    it = stacked_top(); next(it); del it; log.append("after")
def stack_top_closed():
    it = stacked_top(); next(it); it.close(); log.append("after")
def genexpr():
    it = (D("x") for a in [D("a")]); next(it); del it; log.append("after")
def delegating():
    def outer():
        x, y = D("x"), D("y")
        yield from g()
    it = outer(); next(it); del it; log.append("after")
def delegating_closed():
    def outer():
        x, y = D("x"), D("y")
        yield from g()
    it = outer(); next(it); it.close(); log.append("after")
def with_frame():
    it = g(); next(it); it.gi_frame; del it; log.append("after")
def with_frame_closed():
    it = g(); next(it); it.gi_frame; it.close(); log.append("after")
def with_frame_exhausted():
    it = g(); next(it); it.gi_frame; next(it, None); log.append("after")
show(closed)
show(dropped)
show(exhausted)
show(stopped)
show(for_loop)
show(for_break)
show(unstarted_dropped)
show(unstarted_closed)
show(thrown)
show(looped_closed)
show(looped_dropped)
show(handler_closed)
show(handler_dropped)
show(stack_dropped)
show(stack_closed)
show(stack_top_dropped)
show(stack_top_closed)
show(genexpr)
show(delegating)
show(delegating_closed)
show(with_frame)
show(with_frame_closed)
show(with_frame_exhausted)
'''

# A compiled loop's locals live in its native activation until it exits.
CASES["function_hot_body"] = r'''
class Box:
    def __init__(self, n):
        self.n = n
    def get(self):
        return self.n
def looped(raising):
    a, b, c = D("a"), Box(1), D("c")
    total = 0
    for i in range(500):
        total += b.get()
        d = a
    e = D("e")
    if raising:
        raise ValueError
    return total
def caught():
    try:
        looped(True)
    except ValueError:
        log.append("caught")
    log.append("after")
def exits_mid_loop():
    a, b, c = D("a"), Box(1), D("c")
    for i in range(500):
        b.get()
        if i == 400:
            return i
N = 40
show(lambda: looped(False))
show(caught)
show(exits_mid_loop)
'''

CASES["generator_hot_body"] = r'''
def counter(n):
    a, b = D("a"), D("b")
    total = 0
    for i in range(n):
        total += i
        if i % 50 == 49:
            yield total
    c = D("c")
    yield total
def run(how):
    it = counter(200)
    next(it)
    next(it)
    if how == "close":
        it.close()
    elif how == "drop":
        del it
    else:
        for _ in it:
            pass
    log.append("after")
show(lambda: run("close"))
show(lambda: run("drop"))
show(lambda: run("exhaust"))
'''

CASES["coroutines"] = r'''
async def inner():
    x, y = D("x"), D("y")
    return 1
async def outer():
    a, b = D("a"), D("b")
    await inner()
    c = D("c")
def run(co):
    try:
        co.send(None)
    except StopIteration:
        log.append("done")
async def waits():
    a, b = D("a"), D("b")
    await Y()
class Y:
    def __await__(self):
        yield
def closed():
    co = waits(); co.send(None); co.close(); log.append("after")
def dropped():
    co = waits(); co.send(None); del co; log.append("after")
show(lambda: run(outer()))
show(closed)
show(dropped)
'''

CASES["cells"] = r'''
def inner_dead():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    def inner():
        return b, d
def kept():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    return lambda: (b, d)
def keep_then_drop():
    fn = kept(); log.append("returned"); del fn; log.append("dropped")
def cell_argument(p, q, r):
    a, b = D("a"), D("b")
    def inner():
        return q
    del inner
def cell_arguments(p, q, r, s):
    a = D("a")
    def inner():
        return p, s
    del inner
def all_cells():
    z, y, x, w = D("z"), D("y"), D("x"), D("w")
    def inner():
        return z, y, x, w
    del inner
def crossed():
    p, q = D("p"), D("q")
    def i1():
        return q
    def i2():
        return p
def assigned_late():
    a = D("a")
    def inner():
        return b
    b = D("b")
    c = D("c")
def raises_with_cells(p, q):
    a, b = D("a"), D("b")
    def inner():
        return b, q
    del inner
    raise ValueError
def gen_with_cells(p, q):
    a, b = D("a"), D("b")
    def inner():
        return b, q
    del inner
    yield 1
def gen_cells_closed():
    it = gen_with_cells(D("p"), D("q")); next(it); it.close(); log.append("after")
def gen_cells_dropped():
    it = gen_with_cells(D("p"), D("q")); next(it); del it; log.append("after")
def gen_cells_exhausted():
    it = gen_with_cells(D("p"), D("q")); next(it); next(it, None); log.append("after")
def free_vars():
    a = D("a"); b = D("b")
    def inner():
        x = D("x")
        y = D("y")
        return a, b
    return inner
def run_free():
    fn = free_vars(); fn(); log.append("after"); del fn; log.append("dropped")
def free_and_cell():
    a = D("a"); b = D("b")
    def inner(p):
        x = D("x")
        def deep():
            return a, x, p, b
        del deep
    return inner
def run_free_and_cell():
    fn = free_and_cell(); fn(D("p")); log.append("after"); del fn; log.append("dropped")
def last_ref_to_function():
    a = D("a"); b = D("b")
    def inner():
        nonlocal fn
        x = D("x")
        fn = None
        return a, b
    fn = inner
    del inner
    fn()
    log.append("after")
show(inner_dead)
show(keep_then_drop)
show(lambda: cell_argument(D("p"), D("q"), D("r")))
show(lambda: cell_arguments(D("p"), D("q"), D("r"), D("s")))
show(all_cells)
show(crossed)
show(assigned_late)
show(lambda: raises_with_cells(D("p"), D("q")))
show(gen_cells_closed)
show(gen_cells_dropped)
show(gen_cells_exhausted)
show(run_free)
show(run_free_and_cell)
show(last_ref_to_function)
'''

CASES["frame_objects"] = r'''
def returned():
    a, b, c, d = D("a"), D("b"), D("c"), D("d")
    return sys._getframe()
def with_cells(p, q):
    a, b = D("a"), D("b")
    def i():
        return b, q
    c = D("c")
    return sys._getframe()
def raises(p):
    a, b = D("a"), D("b")
    def i():
        return b
    del i
    raise ValueError
def clear_returned():
    fr = returned(); log.append("got"); fr.clear(); log.append("cleared")
def drop_returned():
    fr = returned(); log.append("got"); del fr; log.append("dropped")
def clear_cells():
    fr = with_cells(D("p"), D("q")); log.append("got"); fr.clear(); log.append("cleared")
def drop_cells():
    fr = with_cells(D("p"), D("q")); log.append("got"); del fr; log.append("dropped")
def clear_traceback():
    try:
        raises(D("p"))
    except ValueError as e:
        tb = e.__traceback__
    log.append("caught")
    fr = tb.tb_next.tb_frame
    del tb
    log.append("kept")
    fr.clear()
    log.append("cleared")
def drop_traceback():
    try:
        raises(D("p"))
    except ValueError as e:
        tb = e.__traceback__
    log.append("caught")
    del tb
    log.append("dropped")
def clear_generator_frame():
    def gen(p):
        a, b = D("a"), D("b")
        yield 1
    it = gen(D("p"))
    it.gi_frame.clear()
    log.append("cleared")
def finished_generator_frame():
    def gen(p):
        a, b = D("a"), D("b")
        log.append("in")
        yield sys._getframe()
    it = gen(D("p"))
    fr = next(it)
    next(it, None)
    log.append("finished")
    del fr
    log.append("dropped")
def locals_read():
    a, b, c = D("a"), D("b"), D("c")
    sys._getframe().f_locals
def locals_held():
    a, b, c = D("a"), D("b"), D("c")
    return sys._getframe().f_locals
def hold_locals():
    held = locals_held(); log.append("got"); del held; log.append("dropped")
show(clear_returned)
show(drop_returned)
show(clear_cells)
show(drop_cells)
show(clear_traceback)
show(drop_traceback)
show(clear_generator_frame)
show(finished_generator_frame)
show(locals_read)
show(hold_locals)
'''

# The objects die in this order for weakref callbacks too, and a finalizer
# that looks at the other locals' finalizers sees them run one at a time.
CASES["weakrefs_and_reentrant"] = r'''
import weakref
class W:
    pass
def weak():
    a, b, c = W(), W(), W()
    return [weakref.ref(a, lambda r: log.append("a")),
            weakref.ref(b, lambda r: log.append("b")),
            weakref.ref(c, lambda r: log.append("c"))]
def run_weak():
    refs = weak()
    log.append("back")
class Calls(D):
    def __del__(self):
        log.append(self.n)
        inner()
def inner():
    x, y = D("x"), D("y")
def outer():
    a, b = Calls("a"), Calls("b")
show(run_weak)
show(outer)
'''

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50", "WEAVEPY_JIT_SYNC": "1"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    'plain_return': "['d c b a']\n['d c b a']\n['d c a b got']\n['b del d c a']\n['b rebound c a']\n",
    'arguments': "['c b a']\n['c b a']\n['c b a']\n['d c b a']\n['a']\n['b a']\n['c b a']\n['c b a']\n['a']\n['d x c a']\n['b x a']\n['c b a']\n['c b a']\n['c b a']\n['b a']\n['b a']\n['b a']\n['b a']\n['b a']\n['c b a e d']\n",
    'native_callers': "['c b a']\n['c b a']\n['c b a']\n",
    'unwinding': "['raised d c b a']\n['raised d c b a y x']\n['caught d c b a y x after']\n['caught after d c b a']\n['finally d c b a']\n['exit b e a']\n['t s caught b a after']\n['t s caught b a after']\n['t s caught b a']\n['b i a']\n",
    'generators': "['d c b a after']\n['d c b a after']\n['d c b a after']\n['d c b a stop after']\n['d c b a after']\n['d c b a after']\n['c b a after']\n['c b a after']\n['caught d c b a after']\n['b a after']\n['b a after']\n['exit c b a after']\n['exit c b a after']\n['s b a after']\n['s b a after']\n['t s b a after']\n['t s b a after']\n['x a after']\n['d c b a y x after']\n['d c b a y x after']\n['d c b a after']\n['d c b a after']\n['d c b a after']\n",
    'function_hot_body': "['e c a']\n['caught e c a after']\n['c a']\n",
    'generator_hot_body': "['b a after']\n['b a after']\n['c b a after']\n",
    'coroutines': "['y x c b a done']\n['b a after']\n['b a after']\n",
    'cells': "['d b c a']\n['c a returned d b dropped']\n['b a r q p']\n['a s r q p']\n['z y x w']\n['p q']\n['c b a']\n['raised b a q p']\n['b a q p after']\n['b a q p after']\n['b a q p after']\n['y x after b a dropped']\n['x p after b a dropped']\n['x after b a']\n",
    'frame_objects': "['got d c b a cleared']\n['got d c b a dropped']\n['got c b a q p cleared']\n['got c b a q p dropped']\n['caught kept b a p cleared']\n['caught b a p dropped']\n['p cleared']\n['in finished b a p dropped']\n['c b a']\n['got c b a dropped']\n",
    'weakrefs_and_reentrant': "['c b a back']\n['b y x a y x']\n",
}


def source(name):
    return PRELUDE + CASES[name]


def run_case(name, extra=None):
    env = dict(os.environ, **(extra or {}))
    result = subprocess.run(
        [sys.executable, "-c", source(name)],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class FrameReleaseOrderTests(unittest.TestCase):
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
    setattr(FrameReleaseOrderTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
