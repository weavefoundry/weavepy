"""Native math guards observe changed values, key positions, and namespaces."""

import math
import sys
import types

original_math = math
original_sin = math.sin
original_cos = math.cos


def wave(x):
    return math.sin(x) + math.cos(x)


def repeat(n):
    total = 0.0
    for i in range(n):
        total += wave(i)
    return total


expected = sum(original_sin(i) + original_cos(i) for i in range(2000))
for _ in range(200):
    assert wave(0.0) == 1.0
for _ in range(3):
    assert abs(repeat(2000) - expected) < 1e-10

# Moving an index without changing the target must retain the right function.
namespace = vars(math)
acos = namespace.pop("acos")
assert wave(0.0) == 1.0
namespace["acos"] = acos
sin = namespace.pop("sin")
namespace["sin"] = sin
assert wave(0.0) == 1.0

# Replacing an existing value doesn't change its index. Validate the actual
# value on each entry and after a nested call that can execute Python.
events = []


def replacement(x):
    events.append(x)
    return 11.0


math.sin = replacement
assert wave(0.0) == 12.0
assert events == [0.0]
namespace["sin"] = original_sin
assert wave(0.0) == 1.0
namespace["cos"] = replacement
assert wave(0.0) == 11.0
assert events == [0.0, 0.0]
math.cos = original_cos

# The module itself is guarded, including functions sharing a code object but
# using different global dictionaries.
math = types.SimpleNamespace(sin=lambda x: 20.0, cos=lambda x: 30.0)
assert wave(0.0) == 50.0
math = original_math
assert wave(0.0) == 1.0
other = types.FunctionType(wave.__code__, {"math": math})
assert other(0.0) == 1.0
other.__globals__["math"] = types.SimpleNamespace(sin=lambda x: 40.0, cos=lambda x: 50.0)
assert other(0.0) == 90.0
assert wave(0.0) == 1.0

# A nested callback changes the module between two compiled accesses.
# The ordinary deoptimization path must run that callback exactly once.
mutated = []


def change(i):
    if i == 70:
        math.sin = replacement
        mutated.append(i)


def changing(n):
    total = 0.0
    for i in range(n):
        total += math.sin(0.0)
        change(i)
        total += math.sin(0.0)
    return total


for _ in range(100):
    assert changing(20) == 0.0
assert changing(100) == 59 * 11.0
assert mutated == [70]
assert events[-59:] == [0.0] * 59
math.sin = original_sin
assert wave(0.0) == 1.0

# A missing attribute raises at the original Python operation.
del math.sin
try:
    wave(0.0)
except AttributeError:
    pass
else:
    raise AssertionError("deleted math function was still called")
math.sin = original_sin
assert wave(0.0) == 1.0


def root(x):
    return math.sqrt(x)


def roots(x, n):
    total = 0.0
    for _ in range(n):
        total += root(x)
    return total


for _ in range(200):
    assert root(4.0) == 2.0
for _ in range(50):
    assert roots(4.0, 50) == 100.0
assert math.copysign(1.0, root(-0.0)) == -1.0
assert math.isnan(root(float("nan")))
try:
    roots(-1.0, 50)
except ValueError as error:
    frames = []
    tb = error.__traceback__
    while tb is not None:
        frames.append(tb.tb_frame.f_code.co_name)
        tb = tb.tb_next
    assert frames[-2:] == ["roots", "root"], frames
else:
    raise AssertionError("negative square root did not raise")

# Pure scalar globals still require entry guards, including value rebinding.
SCALE = 2.0


def scaled(x):
    return SCALE * x


def scaling(n):
    total = 0.0
    for i in range(n):
        total += scaled(i)
    return total


for _ in range(200):
    assert scaled(1.0) == 2.0
for _ in range(50):
    assert scaling(100) == 9900.0
SCALE = 3.0
assert scaling(100) == 14850.0

calls = []


def profile(frame, event, arg):
    if event == "call" and frame.f_code.co_name == "root":
        calls.append(frame.f_locals["x"])


sys.setprofile(profile)
try:
    assert roots(9.0, 3) == 9.0
finally:
    sys.setprofile(None)
assert calls == [9.0, 9.0, 9.0]
print("JIT math guards: ok")
