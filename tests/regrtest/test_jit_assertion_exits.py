"""Cold assertion exits preserve messages, locals, cleanup, and identities."""
import builtins
import sys

REAL_ASSERTION_ERROR = AssertionError
EVENTS = []


def assert_plain(n):
    assert n >= 0
    return n + 1


def assert_message(n, message):
    assert n >= 0, message
    return n + 1


def message_effect(n):
    EVENTS.append(n)
    return "bad value %d" % n


def assert_formatted(n):
    assert n >= 0, f"value {message_effect(n)}"
    return n + 1


def assert_conditional(n):
    assert n >= 0, message_effect(-10 if n < -1 else -20)
    return n + 1


def assert_loop(n, fail):
    total = 0
    for i in range(n):
        total += i
        assert i != fail, (i, total)
    return total


def assert_cleanup(n):
    try:
        assert n >= 0, message_effect(n)
        return n + 1
    finally:
        EVENTS.append("cleanup")


def catch(function, *args):
    try:
        function(*args)
    except REAL_ASSERTION_ERROR as error:
        return error
    raise RuntimeError("assertion did not fail")


marker = object()
for i in range(300):
    assert assert_plain(i) == i + 1
    assert assert_message(i, marker) == i + 1
    assert assert_formatted(i) == i + 1
    assert assert_conditional(i) == i + 1
    assert assert_loop(20, -1) == 190
    assert assert_cleanup(i) == i + 1
assert EVENTS == ["cleanup"] * 300
EVENTS.clear()

assert catch(assert_plain, -1).args == ()
error = catch(assert_message, -1, marker)
assert error.args == (marker,) and error.args[0] is marker
assert catch(assert_formatted, -2).args == ("value bad value -2",)
assert catch(assert_conditional, -2).args == ("bad value -10",)
assert catch(assert_conditional, -1).args == ("bad value -20",)
assert EVENTS == [-2, -10, -20]
EVENTS.clear()

error = catch(assert_loop, 20, 13)
assert error.args == ((13, 91),)
tb = error.__traceback__
while tb.tb_next is not None:
    tb = tb.tb_next
assert tb.tb_frame.f_code.co_name == "assert_loop"
assert tb.tb_frame.f_locals["i"] == 13
assert tb.tb_frame.f_locals["total"] == 91
assert tb.tb_frame.f_locals["n"] == 20
assert tb.tb_lineno == assert_loop.__code__.co_firstlineno + 4

assert catch(assert_cleanup, -3).args == ("bad value -3",)
assert EVENTS == [-3, "cleanup"]
EVENTS.clear()

# LOAD_COMMON_CONSTANT must keep its canonical exception under shadowing.
saved_builtin = builtins.AssertionError
try:
    builtins.AssertionError = RuntimeError
    AssertionError = KeyError
    error = catch(assert_plain, -1)
    assert type(error) is REAL_ASSERTION_ERROR
finally:
    builtins.AssertionError = saved_builtin
    del AssertionError

cause = LookupError("outer")
try:
    raise cause
except LookupError:
    error = catch(assert_plain, -1)
    assert error.__context__ is cause and error.__cause__ is None

# An error burst must not change later values or keep evaluating messages.
for _ in range(100):
    assert catch(assert_formatted, -1).args == ("value bad value -1",)
EVENTS.clear()
for i in range(300):
    assert assert_formatted(i) == i + 1
assert EVENTS == []

observed = []


def trace(frame, event, arg):
    if frame.f_code.co_name == "assert_plain":
        observed.append(event)
    return trace


sys.settrace(trace)
try:
    assert assert_plain(8) == 9
    assert type(catch(assert_plain, -1)) is REAL_ASSERTION_ERROR
finally:
    sys.settrace(None)
assert "call" in observed and "exception" in observed and "return" in observed
# Exercise the second canonical exception constant through real bytecode.
import dis
import types

raw = bytearray(assert_plain.__code__.co_code)
constant = next(ins for ins in dis.get_instructions(assert_plain)
                if ins.opname == "LOAD_COMMON_CONSTANT")
assert constant.arg == 0
raw[constant.offset + 1] = 1
changed = assert_plain.__code__.replace(co_code=bytes(raw), co_name="assert_not_implemented")
assert_not_implemented = types.FunctionType(changed, globals())
for i in range(300):
    assert assert_not_implemented(i) == i + 1
try:
    assert_not_implemented(-1)
except NotImplementedError as error:
    assert type(error) is NotImplementedError and error.args == ()
else:
    raise RuntimeError("canonical NotImplementedError constant was lost")

print("JIT assertion exits: all checks passed")
