"""Hot loops next to unsupported code compile as regions, exactly.

Each function mixes a hot numeric loop with constructs the native tier
leaves to the interpreter (nested comprehensions, lambdas, `with` blocks,
generators), so the loop runs natively on its own while the code around
it doesn't. Every case is checked against values computed independently,
and the deopt paths (an exception raised inside the loop, a type change
mid-loop, tracing switched on mid-loop, an inspected frame) are exercised
after the loop got hot.
"""

import contextlib
import sys
import traceback


def sor(n, cycles, omega=1.25):
    # A nested comprehension the native tier doesn't take, then the
    # region that matters.
    g = [[(i * j) % 7 / 7.0 for j in range(n)] for i in range(n)]
    for _ in range(cycles):
        for i in range(1, n - 1):
            gi, gim, gip = g[i], g[i - 1], g[i + 1]
            for j in range(1, n - 1):
                gi[j] = omega * 0.25 * (gim[j] + gip[j] + gi[j - 1] + gi[j + 1]) + (1 - omega) * gi[j]
    return g[n // 2][n // 2]


def sor_reference(n, cycles, omega=1.25):
    g = []
    for i in range(n):
        row = []
        for j in range(n):
            row.append((i * j) % 7 / 7.0)
        g.append(row)
    for _ in range(cycles):
        for i in range(1, n - 1):
            for j in range(1, n - 1):
                g[i][j] = (omega * 0.25 * (g[i - 1][j] + g[i + 1][j] + g[i][j - 1] + g[i][j + 1])
                           + (1 - omega) * g[i][j])
    return g[n // 2][n // 2]


for n in (8, 40, 41):
    assert sor(n, 6) == sor_reference(n, 6), n


def outer_lambda(rows):
    # The outer loop builds a closure each iteration (interpreted); the
    # inner loops run natively, entered once per outer iteration.
    total = 0.0
    for r in range(len(rows)):
        key = max(range(len(rows[r])), key=lambda c: rows[r][c])
        row = rows[r]
        for c in range(len(row)):
            total += row[c] * key
    return total


rows = [[float((r * 31 + c * 17) % 23) for c in range(50)] for r in range(60)]
expected = 0.0
for r in range(60):
    key = max(range(50), key=lambda c: rows[r][c])
    for c in range(50):
        expected += rows[r][c] * key
assert outer_lambda(rows) == expected


events = []


@contextlib.contextmanager
def recording(tag):
    events.append(("enter", tag))
    yield tag
    events.append(("exit", tag))


def with_block(n):
    total = 0
    for k in range(n):
        with recording(k) as tag:
            acc = 0
            for i in range(200):
                acc += i * tag
        total += acc
    return total


events.clear()
assert with_block(30) == sum(sum(i * k for i in range(200)) for k in range(30))
assert events == [e for k in range(30) for e in (("enter", k), ("exit", k))]


def after_unsupported(n):
    names = {str(i): i for i in range(n)}  # dict comprehension: interpreted
    s = 0
    for i in range(n):
        s += i * i
    rest = [k for k in names if int(k) % 2]  # after the loop: interpreted
    return s, len(rest)


assert after_unsupported(5000) == (sum(i * i for i in range(5000)), 2500)


# An exception raised inside a region loop carries the loop's line.
def raises_in_region(xs, bad):
    seen = [x for x in xs]  # interpreted prefix
    total = 0
    for i in range(len(seen)):
        total += 10 // (seen[i] - bad)  # the raising line
    return total


xs = list(range(1, 4000))
assert raises_in_region(xs, -1) == sum(10 // (x + 1) for x in xs)
try:
    raises_in_region(xs, 3000)
except ZeroDivisionError:
    tb = traceback.extract_tb(sys.exc_info()[2])
    assert tb[-1].name == "raises_in_region"
    assert tb[-1].line == "total += 10 // (seen[i] - bad)  # the raising line", tb[-1].line
else:
    raise AssertionError("expected ZeroDivisionError")


# A region whose operand changes type mid-loop resumes in the interpreter.
def mixed(values):
    pairs = [(v, v) for v in values]  # interpreted prefix
    total = 0.0
    for i in range(len(pairs)):
        a, b = pairs[i]
        total = total + a * b
    return total


vals = [float(i % 13) for i in range(5000)]
assert mixed(vals) == sum(v * v for v in vals)
vals[4000] = 7  # an int among the floats
vals[4500] = 2 ** 70  # and a big one
assert mixed(vals) == sum(v * v for v in vals)


# Tracing switched on from inside a hot region loop: the native code hands
# back to the interpreter, and frames called afterwards are traced.
def traced_callee(i):
    return i + 1


def toggles_tracing(n, at):
    lst = [i for i in range(n)]  # interpreted prefix
    total = 0
    for i in range(n):
        if i == at:
            sys.settrace(tracer)
        total += traced_callee(lst[i])
    sys.settrace(None)
    return total


lines = []


def tracer(frame, event, arg):
    if frame.f_code.co_name == "traced_callee" and event == "call":
        lines.append(frame.f_locals["i"])
    return None


lines.clear()
assert toggles_tracing(3000, 2990) == sum(range(1, 3001))
assert lines == list(range(2990, 3000)), lines


# Python code inspecting a native frame's locals sees, and writes, them.
def poke(i):
    sys._getframe(1).f_locals["limit"] = i + 1
    return i


def inspected(producer, limit, n):
    data = [i for i in range(n)]  # interpreted prefix
    acc = 0
    hits = 0
    for i in range(n):
        if producer(data[i]) >= limit:
            hits += 1
        acc += 1
    return hits, acc


def checks_acc(i):
    return 0 if sys._getframe(1).f_locals["acc"] == i else 10 ** 9


assert inspected(checks_acc, 1, 3000) == (0, 3000)
assert inspected(lambda i: i, 0, 3000) == (3000, 3000)
assert inspected(poke, 0, 50) == (0, 50)


# Generators with an interpreted prefix around a hot loop.
def gen(n):
    squares = {i: i * i for i in range(10)}  # interpreted
    for i in range(n):
        yield i * squares[i % 10]


assert sum(gen(4000)) == sum(i * ((i % 10) ** 2) for i in range(4000))

print("JIT regions: ok")
