"""Explicit raise exits preserve operands, locals, and exception semantics."""
FAILURE = ValueError("failure")
CAUSE = LookupError("cause")
EVENTS = []


def raise_guarded_sum(n, fail):
    total = 0
    for i in range(n):
        total += i
    if fail:
        raise FAILURE
    return total


def raise_guarded_cause(n):
    if n < 0:
        raise FAILURE from CAUSE
    return n + 1


def raise_guarded_none(n):
    if n < 0:
        raise FAILURE from None
    return n + 1


def raise_guarded_invalid(n):
    if n < 0:
        raise 7
    return n + 1


def raise_guarded_bare(n):
    if n < 0:
        raise
    return n + 1


def raise_guarded_handler(n, fail):
    try:
        value = n + 3
        if fail:
            raise FAILURE
        return value
    except ValueError:
        return value + 7
    finally:
        EVENTS.append(n)


def raise_guarded_loop(n, stop):
    total = 0
    for i in range(n):
        total += i
        if i == stop:
            raise FAILURE
    return total


for _ in range(400):
    assert raise_guarded_sum(50, False) == 1225
    assert raise_guarded_loop(50, -1) == 1225
    assert raise_guarded_handler(7, False) == 10
    for function in (raise_guarded_cause, raise_guarded_none,
                     raise_guarded_invalid, raise_guarded_bare):
        assert function(10) == 11


def caught(function, args, expected):
    FAILURE.__traceback__ = None
    try:
        function(*args)
    except expected as error:
        return error
    raise AssertionError("expected exception")


error = caught(raise_guarded_sum, (50, True), ValueError)
assert error is FAILURE
trace = error.__traceback__
while trace.tb_next is not None:
    trace = trace.tb_next
assert trace.tb_frame.f_code.co_name == "raise_guarded_sum"
assert trace.tb_frame.f_locals["total"] == 1225

assert caught(raise_guarded_cause, (-1,), ValueError) is FAILURE
assert FAILURE.__cause__ is CAUSE
assert FAILURE.__suppress_context__
assert caught(raise_guarded_none, (-1,), ValueError) is FAILURE
assert FAILURE.__cause__ is None
assert FAILURE.__suppress_context__
assert str(caught(raise_guarded_invalid, (-1,), TypeError)).startswith("exceptions must derive from BaseException")
assert "No active exception" in str(caught(raise_guarded_bare, (-1,), RuntimeError))

try:
    raise CAUSE
except LookupError:
    assert caught(raise_guarded_bare, (-1,), LookupError) is CAUSE

EVENTS.clear()
assert raise_guarded_handler(7, True) == 17
assert EVENTS == [7]
error = caught(raise_guarded_loop, (50, 23), ValueError)
assert error is FAILURE
trace = error.__traceback__
while trace.tb_next is not None:
    trace = trace.tb_next
assert trace.tb_frame.f_locals["i"] == 23
assert trace.tb_frame.f_locals["total"] == 276

# The compiler duplicates the constructor CALL across conditional arms.
# On an exit, the other arm's call marker must not cover the exception.
class ClosedCheck:
    def __init__(self):
        self.closed = False

    def check(self, message=None):
        if self.closed:
            raise ValueError("closed" if message is None else message)


node = ClosedCheck()
for _ in range(400):
    assert node.check() is None
    assert node.check("custom") is None
node.closed = True
for message, expected in ((None, "closed"), ("custom", "custom")):
    error = caught(node.check, (message,), ValueError)
    assert type(error) is ValueError
    assert str(error) == expected


def make_failure(message):
    EVENTS.append(message)
    return ValueError(message)


def raise_constructed(n, message):
    if n < 0:
        raise make_failure("default" if message is None else message)
    return n


for _ in range(400):
    assert raise_constructed(7, None) == 7
    assert raise_constructed(7, "custom") == 7
EVENTS.clear()
for message, expected in ((None, "default"), ("custom", "custom")):
    assert str(caught(raise_constructed, (-1, message), ValueError)) == expected
assert EVENTS == ["default", "custom"]

import _pyio
stream = _pyio.IOBase()
for _ in range(400):
    stream.flush()
stream.close()
assert str(caught(stream.flush, (), ValueError)) == "I/O operation on closed file."
print("ok")
