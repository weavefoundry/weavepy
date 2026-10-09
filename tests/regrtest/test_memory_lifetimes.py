"""Lifetimes and lazily built state that memory savings depend on: a code
object dies with its last function, inline caches built only once code is
warm still see rebinding, generator stacks sized to their code still hold
deep expressions, and lazily decoded line numbers and function slots read
the same as eager ones."""

import dis
import gc
import inspect
import textwrap
import types
import weakref

# A function's code object dies with it: nothing the interpreter keeps for
# code that has run (tier-up counters, caches) holds it alive.
ns = {}
exec("def f(n):\n    t = 0\n    for i in range(n):\n        t += i\n    return t\n", ns)
f = ns.pop("f")
for _ in range(5):
    assert f(10) == 45
ref = weakref.ref(f.__code__)
del f, ns
assert ref() is None, "code object outlived its function"

code = compile("t = 0\nfor i in range(200):\n    t += i\n", "<lifetimes>", "exec")
for _ in range(20):
    exec(code, {})
ref = weakref.ref(code)
del code
gc.collect()
assert ref() is None


# Inline caches allocated only after warm-up still observe rebinding, both
# before and after the code is warm.
class A:
    def m(self):
        return "a"

    x = 1


class B:
    def m(self):
        return "b"

    x = 2


def call_m(o):
    return o.m() + str(o.x)


for n in (1, 2, 3, 50, 500):
    for _ in range(n):
        assert call_m(A()) == "a1"
        assert call_m(B()) == "b2"
A.m = lambda self: "A"
A.x = 10
assert call_m(A()) == "A10"
assert call_m(B()) == "b2"

G = 1


def read_g():
    return G


for _ in range(1000):
    assert read_g() == 1
G = 2
assert read_g() == 2
del G
try:
    read_g()
except NameError:
    pass
else:
    raise AssertionError("expected NameError")


# Generators keep stacks sized to their code; deep expressions around a
# yield, many suspended at once, and reuse after exhaustion all work.
def shallow(n):
    for i in range(n):
        yield i


def deep(n):
    for i in range(n):
        got = yield (i, (i + 1, (i + 2, (i + 3, (i + 4, [i, i, i, {i: (i, i)}])))))
        yield [got, (got, (got, (got, (got,))))]


def nested_calls(n):
    for i in range(n):
        yield max(min(i, 1 + abs(i - 1)), sum([i, i, i, len((i, i, i, i))]))


suspended = [shallow(3) for _ in range(1000)] + [deep(2) for _ in range(1000)]
for g in suspended:
    next(g)
assert all(next(g) == 1 for g in suspended[:1000])
for g in suspended[1000:]:
    assert g.send("x") == ["x", ("x", ("x", ("x", ("x",))))]
for round_ in range(3):
    assert list(shallow(5)) == [0, 1, 2, 3, 4]
    assert list(nested_calls(4)) == [4, 7, 10, 13]
    d = deep(1)
    assert next(d) == (0, (1, (2, (3, (4, [0, 0, 0, {0: (0, 0)}])))))
    assert d.send(7) == [7, (7, (7, (7, (7,))))]


async def coro(x):
    return [x, (x, (x, (x, (x, (x, x)))))]


c = coro(3)
try:
    c.send(None)
except StopIteration as e:
    assert e.value == [3, (3, (3, (3, (3, (3, 3)))))]


# Line numbers decoded on first use match the source.
func = textwrap.dedent
co = func.__code__
lines, first = inspect.getsourcelines(func)
last = first + len(lines) - 1
line_numbers = [line for _, _, line in co.co_lines() if line is not None]
assert line_numbers and first <= min(line_numbers) and max(line_numbers) <= last
starts = [line for _, line in dis.findlinestarts(co) if line is not None]
assert starts and all(first <= line <= last for line in starts)
try:
    textwrap.dedent(None)
except (TypeError, AttributeError) as e:
    tb = e.__traceback__
    while tb.tb_next is not None:
        tb = tb.tb_next
    assert tb.tb_frame.f_code.co_filename.endswith("textwrap.py")
    assert first <= tb.tb_lineno <= last or tb.tb_frame.f_code is not co
else:
    raise AssertionError("expected an error")


# Function slots built on first use read and write like eager ones.
def plain(a, b=2):
    "plain doc"
    return a + b


assert (plain.__name__, plain.__qualname__, plain.__doc__) == ("plain", "plain", "plain doc")
assert plain.__module__ == __name__
plain.__name__ = "renamed"
plain.__qualname__ = "Q.renamed"
plain.__defaults__ = (5,)
assert plain(1) == 6
assert plain.__dict__ == {}
assert (plain.__name__, plain.__qualname__) == ("renamed", "Q.renamed")
copy = types.FunctionType(plain.__code__, plain.__globals__, "copy", plain.__defaults__)
assert copy.__name__ == "copy" and copy(1) == 6

gen = (lambda: (yield))()
assert gen.__name__ == "<lambda>"
print("ok")
