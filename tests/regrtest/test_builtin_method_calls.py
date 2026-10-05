"""Methods of builtin types called through the method-form load keep
CPython's behavior: results, errors and their tracebacks, bodies that run
Python code, sentinel-dispatched methods, and bound-method identity."""

import sys
import traceback


def loop(fn, n=300):
    # Enough repetitions for the call sites to specialize.
    out = None
    for _ in range(n):
        out = fn()
    return out


s = "café au lait"
assert loop(lambda: s.encode()) == b"caf\xc3\xa9 au lait"
assert loop(lambda: s.encode("utf-8")) == b"caf\xc3\xa9 au lait"
assert loop(lambda: s.encode("latin-1")) == b"caf\xe9 au lait"
assert loop(lambda: s.encode("ascii", "replace")) == b"caf? au lait"
assert loop(lambda: s.encode(encoding="utf8")) == b"caf\xc3\xa9 au lait"
try:
    s.encode("ascii")
except UnicodeEncodeError as e:
    assert e.start == 3
else:
    raise AssertionError("ascii encode of non-ASCII text")

for n in (0, 1, -1, 255, 256, -(2 ** 63), 2 ** 63 - 1, 2 ** 100, -(2 ** 100)):
    assert loop(lambda: n.bit_length(), 3) == len(bin(abs(n))) - 2 - (n == 0), n
    assert loop(lambda: n.bit_count(), 3) == bin(n).count("1"), n
assert True.bit_length() == 1 and False.bit_count() == 0


class Eq:
    calls = 0

    def __eq__(self, other):
        Eq.calls += 1
        return other == 2


lst = [1, 2, Eq(), 2]
assert loop(lambda: lst.count(2), 10) == 3
assert Eq.calls == 10


class Boom:
    def __eq__(self, other):
        raise RuntimeError("boom")


def raising():
    return [Boom()].count(1)


for _ in range(5):
    try:
        raising()
    except RuntimeError as e:
        frames = traceback.extract_tb(e.__traceback__)
        assert frames[-1].name == "__eq__", frames
        assert any(f.name == "raising" for f in frames), frames
    else:
        raise AssertionError("count() swallowed the error")


def missing():
    return [1, 2].index(5)


for _ in range(5):
    try:
        missing()
    except ValueError as e:
        assert e.__traceback__.tb_next.tb_frame.f_code.co_name == "missing"
    else:
        raise AssertionError("index() of a missing item")

# Python code a method body runs sees the calling frame on the stack.
class Peek:
    seen = None

    def __eq__(self, other):
        Peek.seen = sys._getframe(1).f_code.co_name
        return False


def peeking_caller():
    return [Peek()].count(1)


for _ in range(300):
    peeking_caller()
assert Peek.seen == "peeking_caller", Peek.seen

# Sentinel-dispatched methods still work in the method-call shape.
it = iter([1, 2, 3])
next(it)
assert loop(lambda: it.__reduce__(), 5)[2] == 1
assert loop(lambda: "{}-{}".format(1, "a"), 5) == "1-a"
assert loop(lambda: "{x}".format_map({"x": 3}), 5) == "3"


def gen():
    yield 1
    yield 2


g = gen()
assert loop(lambda: g.send(None), 1) == 1

# Bound methods of builtins: a fresh one per access, equal to each other,
# and the unbound descriptor is shared.
assert s.upper is not s.upper and s.upper == s.upper
assert str.upper is str.upper
assert (5).bit_length.__self__ == 5
print("ok")
