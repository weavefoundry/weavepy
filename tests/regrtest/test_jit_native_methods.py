"""Native method calls preserve binding, fallbacks, and completed results."""

from collections import deque
import sys
import weakref
import gc


def exercise(values, n):
    total = 0
    for i in range(n):
        values.append(i)
        total += values.popleft()
    return total


assert exercise(deque(), 4000) == 7998000

# NATIVE METHOD FALLBACKS:

class Custom(deque):
    pass


values = Custom()
assert exercise(values, 4000) == 7998000
original = values.append
# An already bound method retains its original receiver and function.
Custom.append = lambda self, value: deque.append(self, value + 3)
original(10)
assert values.popleft() == 10
assert exercise(values, 3) == 12
del Custom.append
assert exercise(values, 3) == 3

# An instance attribute shadows the native non-data descriptor.
values.append = lambda value: deque.append(values, value + 5)
assert exercise(values, 3) == 18
del values.append
assert exercise(values, 3) == 3

calls = []
class AppendDescriptor:
    def __get__(self, instance, owner):
        frame = sys._getframe(1)
        calls.append((frame.f_code.co_name, frame.f_locals.get("i")))
        return lambda value: deque.append(instance, value + 7)


Custom.append = AppendDescriptor()
assert exercise(values, 3) == 24
assert calls == [("exercise", 0), ("exercise", 1), ("exercise", 2)], calls
del Custom.append


def rotate(values, index, n):
    marker = 73
    for i in range(n):
        values.rotate(index)
    assert marker == 73


values = deque([1, 2, 3])
rotate(values, 1, 4000)
calls.clear()
class Index:
    def __index__(self):
        frame = sys._getframe(1)
        calls.append((frame.f_code.co_name, frame.f_locals.get("marker")))
        return 1


rotate(values, Index(), 3)
assert calls == [("rotate", 73)] * 3, calls

# An index produced inside the native activation reaches the fast method's
# argument guard, rather than failing the function's entry type guard.
def generated_index():
    return Index()


def rotate_generated(values, n):
    marker = 73
    for i in range(n):
        values.rotate(generated_index())
    assert marker == 73


calls.clear()
rotate_generated(values, 4000)
assert calls == [("rotate_generated", 73)] * 4000, calls[:5]

# Bound results own their receivers until the result itself is released.
def bound(values, n):
    result = None
    for i in range(n):
        result = values.append
    return result


values = deque()
ref = weakref.ref(values)
method = bound(values, 4000)
del values
gc.collect()
assert ref() is not None
method(11)
assert list(ref()) == [11]
del method
gc.collect()
assert ref() is None

# A hook installed by a call inside a compiled loop must observe subsequent
# builtin calls, before the next periodic native-loop poll.
events = []


def profile(frame, event, function):
    if event == "c_call" and getattr(function, "__name__", "") in ("append", "popleft"):
        if frame.f_code.co_name == "profiled":
            events.append((function.__name__, frame.f_locals.get("i")))


def install():
    sys.setprofile(profile)


def profiled(values, n, switch):
    total = 0
    for i in range(n):
        if i == switch:
            install()
        values.append(i)
        total += values.popleft()
    return total


assert profiled(deque(), 4000, -1) == 7998000
try:
    assert profiled(deque(), 5, 2) == 10
finally:
    sys.setprofile(None)
assert events == [(name, i) for i in range(2, 5) for name in ("append", "popleft")], events
print("Native method semantics: ok")
