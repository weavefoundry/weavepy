"""Global and builtin loads from compiled loops see every change: a global
rebound, deleted (then the builtin of that name), shadowing a builtin, the
builtins dict changed, a function run under other globals (`exec`,
`types.FunctionType`), and a missing name's `NameError`."""

import builtins
import types

K = 1


def read(n):
    t = 0
    for _ in range(n):
        t += K + len("ab")
    return t


for _ in range(5):
    assert read(300) == 900

K = 5
assert read(10) == 70
del K
try:
    read(1)
except NameError as e:
    assert "K" in str(e)
else:
    raise AssertionError("expected NameError")
builtins.K = 7
try:
    assert read(10) == 90
finally:
    del builtins.K
K = 1
assert read(10) == 30

# A global shadowing a builtin, and back.
len = lambda s: 10
assert read(10) == 110
del len
assert read(10) == 30

# The builtin itself replaced.
orig_len = builtins.len
builtins.len = lambda s: 100
try:
    assert read(10) == 1010
finally:
    builtins.len = orig_len
assert read(10) == 30

# The same code under other globals.
g = {"K": 3, "len": orig_len}
f = types.FunctionType(read.__code__, g)
assert f(10) == 50
ns = {}
exec("K = 4\n" + "def r(n):\n    t = 0\n    for _ in range(n):\n        t += K\n    return t\n", ns)
for _ in range(4):
    assert ns["r"](300) == 1200
ns["K"] = 0
assert ns["r"](10) == 0
assert read(10) == 30
print("ok")
