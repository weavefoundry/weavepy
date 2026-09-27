"""First-call compilation deferral preserves results and later call behavior."""
import sys


def total(n):
    value = 0
    for i in range(n):
        value += i
    return value


assert total(2000) == 1999000
assert total(2000) == 1999000
assert total(20000) == 199990000
assert total(0) == 0
assert total(-1) == 0

# Rebinding range after compilation must preserve the custom iterator's calls.
builtin_range = range
events = []


class CustomRange:
    def __init__(self, n):
        self.n = n
        self.i = 0
        events.append(("init", n))

    def __iter__(self):
        events.append(("iter", self.n))
        return self

    def __next__(self):
        events.append(("next", self.i))
        if self.i == self.n:
            raise StopIteration
        self.i += 1
        return self.i * 2


range = CustomRange
assert total(4) == 20
assert events == [("init", 4), ("iter", 4)] + [("next", i) for i in builtin_range(5)]
range = builtin_range
assert total(20000) == 199990000


def stepped(start, stop, step):
    value = 0
    for i in range(start, stop, step):
        value += i
    return value


for start, stop, step in [(2000, 0, -1), (0, 2000, 2), (-2000, 0, 1),
                           (2 ** 63 - 100, 2 ** 63 + 100, 1),
                           (2 ** 100, 2 ** 100 + 100, 1)]:
    assert stepped(start, stop, step) == sum(range(start, stop, step))


# A later call through the same code can change operand types or recurse.
def adding(n, value):
    for i in range(n):
        value += i
    return value


assert adding(2000, 0) == 1999000
assert adding(20000, 0.5) == 199990000.5


class RecursiveAdd:
    def __init__(self):
        self.value = 0

    def __iadd__(self, other):
        self.value += adding(3, other)
        return self


assert adding(100, RecursiveAdd()).value == 5250

# Fresh code under a line observer stays observable during a short loop.
lines = []


def observed(n):
    value = 0
    for i in range(n):
        value += i
    return value


def trace(frame, event, arg):
    if frame.f_code is observed.__code__ and event == "line":
        lines.append(frame.f_lineno)
    return trace


sys.settrace(trace)
try:
    assert observed(100) == 4950
finally:
    sys.settrace(None)
assert len(lines) == 203, len(lines)
assert observed(20000) == 199990000
print("short range compilation budget: ok")
