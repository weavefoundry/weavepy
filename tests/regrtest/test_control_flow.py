"""Smoke test: if/for/while/try/break/continue/with."""

x = 5
if x > 0:
    sign = "positive"
elif x < 0:
    sign = "negative"
else:
    sign = "zero"
assert sign == "positive"

acc = 0
for i in range(10):
    if i == 5:
        break
    acc += i
assert acc == 0 + 1 + 2 + 3 + 4

acc = 0
for i in range(5):
    if i == 2:
        continue
    acc += i
assert acc == 0 + 1 + 3 + 4

n = 10
fib = []
a, b = 0, 1
while a < n:
    fib.append(a)
    a, b = b, a + b
assert fib == [0, 1, 1, 2, 3, 5, 8]

# try/except/else/finally
seen = []
try:
    seen.append("try")
    1 / 0
except ZeroDivisionError as e:
    seen.append("except:" + type(e).__name__)
else:
    seen.append("else")
finally:
    seen.append("finally")
assert seen == ["try", "except:ZeroDivisionError", "finally"]

# raise + chain
try:
    try:
        raise ValueError("inner")
    except ValueError as e:
        raise RuntimeError("outer") from e
except RuntimeError as e:
    assert isinstance(e.__cause__, ValueError)
    assert str(e.__cause__) == "inner"

# with-statement
import io
buf = io.StringIO()
with buf as fp:
    fp.write("hello")
assert buf.getvalue() == "hello"

# nested loops + else
found = None
for i in range(5):
    for j in range(5):
        if i * j == 12:
            found = (i, j)
            break
    else:
        continue
    break
assert found == (3, 4)

# nested function-local control flow
def looped(n):
    out = []
    for i in range(n):
        for j in range(n):
            if j > i:
                break
            out.append((i, j))
    return out


assert looped(3) == [(0, 0), (1, 0), (1, 1), (2, 0), (2, 1), (2, 2)]


# RFC 0037 (WS2): a `with` block whose `__exit__` *suppresses* an exception,
# nested inside a `for` loop, must leave the loop's iterator on the operand
# stack so the loop continues. A miscomputed handler depth used to truncate
# the stack to empty, so the next `FOR_ITER` aborted with "no iter".
class _Suppress:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return True  # swallow whatever was raised


def for_with_suppress():
    seen = []
    for i in range(4):
        with _Suppress():
            seen.append(i)
            if i % 2 == 1:
                raise ValueError(i)
    return seen


assert for_with_suppress() == [0, 1, 2, 3]


def for_unpack_with_suppress():
    seen = []
    for a, b in [(1, 2), (3, 4), (5, 6)]:
        with _Suppress():
            seen.append(a)
            raise RuntimeError(b)
    return seen


assert for_unpack_with_suppress() == [1, 3, 5]


def nested_for_with_suppress():
    seen = []
    for i in range(3):
        for j in range(3):
            with _Suppress():
                if j == 1:
                    raise KeyError((i, j))
                seen.append((i, j))
    return seen


assert nested_for_with_suppress() == [
    (0, 0), (0, 2), (1, 0), (1, 2), (2, 0), (2, 2)
]


# break / continue / return out of a `with` inside a `for` must still run
# `__exit__` and keep the iterator coherent.
class _Track:
    def __init__(self, log):
        self.log = log

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.log.append("exit")
        return False


def for_with_break():
    log = []
    for i in range(5):
        with _Track(log):
            if i == 2:
                break
            log.append(i)
    return log


assert for_with_break() == [0, "exit", 1, "exit", "exit"]


def for_with_continue():
    log = []
    for i in range(3):
        with _Track(log):
            if i == 1:
                continue
            log.append(i)
    return log


assert for_with_continue() == [0, "exit", "exit", 2, "exit"]


# RFC 0037 (WS2): an inline suite after `:` is a full simple-statement list
# (`small_stmt (';' small_stmt)* [';'] NEWLINE`), not a single statement.
# WeavePy used to keep only the first statement and re-parse the rest in the
# enclosing scope, so `def f(): a = 1; return a` raised "return outside
# function".
def inline_return(): a = 1; return a + 1


assert inline_return() == 2


def inline_multi(): x = 1; y = 2; return x + y


assert inline_multi() == 3


def inline_trailing_semi(): return 7;


assert inline_trailing_semi() == 7


def inline_gen(): yield 1; yield 2; yield 3


assert list(inline_gen()) == [1, 2, 3]


class _Inline: a = 1; b = 2


assert (_Inline.a, _Inline.b) == (1, 2)

if True: _u = 1; _v = 2
assert (_u, _v) == (1, 2)

print("control flow ok")
