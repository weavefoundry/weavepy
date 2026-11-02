"""Compiled calls of leaf functions through a plain `CALL` keep Python's
semantics.

The frame JIT evaluates a leaf function (`lambda a, b: a + b`, `return
env[k]`) in place at a call site whether or not the callee has compiled
code of its own: a function stored on an instance (`self.fn(a, b)`, read
through the site's recorded position), a global, a local, many leaves at
one site, and keyword calls. Each of the things those caches don't cover
must still be seen: the attribute changing or going away, a class
property taking over the name, the global rebound, `__code__` and
`__defaults__` rebound, a surplus positional argument, a bailing leaf,
and the recursion limit.
"""

import sys


class Op:
    def __init__(self, fn):
        self.fn = fn

    def run(self, a, b):
        return self.fn(a + 0, b)

    def fn(self, a, b):
        return 1000


def add(a, b):
    return a + b


def get(env, k):
    return env[k]


def kw(a, b=2, *, c=3):
    return a + b + c


def through_attr(ops, n):
    t = 0
    for i in range(n):
        for op in ops:
            t += op.run(i, 1)
    return t


def through_global(n):
    t = 0
    for i in range(n):
        t += add(i + 0, 1)
    return t


def through_local(f, n):
    t = 0
    for i in range(n):
        t += f(i + 0, 1)
    return t


def through_subscr(env, n):
    t = 0
    for _ in range(n):
        t += get(env, "x") + get(env, "y")
    return t


def through_kw(n):
    t = 0
    for i in range(n):
        t += kw(i, c=1)
    return t


N = 3000
expect = sum(range(N)) + N

# Operator tables: many leaf callees at one site.
OPS = [lambda a, b: a + b, lambda a, b: a - b, lambda a, b: a * b,
       lambda a, b: a > b, lambda a, b: a < b, lambda a, b: b]
ops = [Op(f) for f in OPS]
want = sum(sum(f(i, 1) for f in OPS) for i in range(N))
assert through_attr(ops, N) == want
assert through_global(N) == expect
assert through_local(lambda a, b: a + b, N) == expect
assert through_subscr({"x": 4, "y": 5}, N) == 9 * N
assert through_kw(N) == sum(range(N)) + 3 * N

# The instance attribute changes, then goes away (the class's method).
op = Op(lambda a, b: a + b)
assert through_attr([op], 10) == sum(range(10)) + 10
op.fn = lambda a, b: a * b
assert through_attr([op], 10) == sum(range(10))
del op.fn
assert through_attr([op], 10) == 10000


# A class property takes the name over (a data descriptor wins).
class Prop(Op):
    pass


p = Prop(lambda a, b: a + b)
assert through_attr([p], 100) == sum(range(100)) + 100
Prop.fn = property(lambda self: (lambda a, b: -1))
assert through_attr([p], 10) == -10
del Prop.fn
assert through_attr([p], 10) == sum(range(10)) + 10

# The global rebound.
saved_add = add
add = lambda a, b: a - b  # noqa: E731
assert through_global(10) == sum(range(10)) - 10
add = saved_add
assert through_global(10) == sum(range(10)) + 10

# `__code__` and `__defaults__` rebound.
add.__code__ = (lambda a, b: a * b).__code__
assert through_global(10) == sum(range(10))
add.__code__ = (lambda a, b: a + b).__code__
kw.__defaults__ = (10,)
assert through_kw(10) == sum(range(10)) + 10 * 10 + 10
kw.__defaults__ = (2,)
kw.__kwdefaults__ = {"c": 3}
assert through_kw(10) == sum(range(10)) + 3 * 10

# A leaf whose native code declines (an overflow, other operand types)
# still answers.
pair = Op(lambda a, b: a + b)


def mixed(n, big):
    out = []
    for i in range(n):
        out.append(pair.fn(big + i, big))
    return out


assert mixed(N, 1)[-1] == N + 1
assert mixed(3, 2**62) == [2**63, 2**63 + 1, 2**63 + 2]
assert [pair.fn("x" * k, "y") for k in range(3)] == ["y", "xy", "xxy"]


# A surplus positional argument raises.
def surplus(f, n):
    for i in range(n):
        f(i, 1)
    return f(1, 2, 3)


try:
    surplus(lambda a, b: a + b, N)
except TypeError as e:
    assert "positional argument" in str(e), e
else:
    raise AssertionError("a surplus positional argument bound")


# The recursion limit still applies.
def deep(n):
    if n == 0:
        return add(1, 2)
    return deep(n - 1)


old = sys.getrecursionlimit()
for _ in range(200):
    deep(5)
sys.setrecursionlimit(60)
try:
    deep(200)
except RecursionError:
    pass
else:
    raise AssertionError("no RecursionError")
finally:
    sys.setrecursionlimit(old)

print("ok")
