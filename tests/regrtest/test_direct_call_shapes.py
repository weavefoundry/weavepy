"""Compiled callers call more shapes directly, with the same semantics.

A compiled function's call of another compiled function runs the callee
from the caller's call helper and finishes its return there. That now
covers callees with cell variables (their own fresh cells), closures (the
function's cells), and calls with keyword arguments; a callee's
`RETURN_VALUE` hands its return straight back; and an eval-breaker
countdown that runs out with no thread waiting for the GIL starts over in
place. Each loop below runs long enough for its functions to compile.
"""

import sys
import threading
import traceback

N = 60000


def make_adder(k):
    def add(x):
        return x + k

    return add


def make_counter():
    n = 0

    def bump(by):
        nonlocal n
        n += by
        return n

    return bump


def cell_param(a, b):
    def get():
        return a + b

    a += 1
    return get()


def closures():
    total = 0
    bump = make_counter()
    for i in range(N):
        add = make_adder(i)
        total += add(1) + add(2)
        total += cell_param(i, 1)
        bump(2)
    expect = sum(2 * i + 3 for i in range(N)) + sum(i + 2 for i in range(N))
    assert total == expect, (total, expect)
    assert bump(0) == 2 * N


def kw_callee(a, b=1, *, c=2):
    return a * 100 + b * 10 + c


def kw_positional(a, scale=1):
    return a * scale


def kw_calls():
    t = 0
    for i in range(N):
        t += kw_positional(i, scale=2)
        t += kw_callee(1, b=2, c=3)
    assert t == sum(2 * i for i in range(N)) + 123 * N, t


class Dying:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Dying.log.append(self.name)


def holds_cell(d):
    def peek():
        return d.name

    return peek()


def del_timing():
    for i in range(N):
        Dying.log.clear()
        name = holds_cell(Dying(i))
        # The argument dies with the callee's cells, before the caller goes on.
        assert Dying.log == [i], (i, Dying.log)
        assert name == i


def frames_through_closures():
    def inner(depth):
        f = sys._getframe()
        return (f.f_code.co_name, f.f_back.f_code.co_name, depth)

    def outer(depth):
        return inner(depth)

    for i in range(N):
        assert outer(i) == ("inner", "outer", i)


def raising_closure():
    k = 3

    def boom(x):
        if x == N - 1:
            raise ValueError(x + k)
        return x

    def call(x):
        return boom(x)

    for i in range(N - 1):
        assert call(i) == i
    try:
        call(N - 1)
    except ValueError as e:
        names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert names[-2:] == ["call", "boom"], names
        assert e.args == (N - 1 + k,)
    else:
        raise AssertionError("no raise")


def recursion_limit():
    def down(n):
        def step():
            return n

        if n == 0:
            return step()
        return down(n - 1) + 1

    for _ in range(200):
        assert down(20) == 20
    old = sys.getrecursionlimit()
    sys.setrecursionlimit(200)
    try:
        down(500)
    except RecursionError:
        pass
    else:
        raise AssertionError("no RecursionError")
    finally:
        sys.setrecursionlimit(old)


def busy_thread_hand_off():
    # A compiled loop of direct calls that never blocks must still let a
    # thread waiting for the GIL in.
    flag = []

    def worker():
        flag.append(1)

    def spin(i):
        return i + 1

    t = threading.Thread(target=worker)
    t.start()
    i = 0
    while not flag:
        i = spin(i)
        if i > 50_000_000:
            raise AssertionError("the waiting thread never ran")
    t.join()


closures()
kw_calls()
del_timing()
frames_through_closures()
raising_closure()
recursion_limit()
busy_thread_hand_off()
print("direct call shapes: ok")
