"""`exec` and `eval` of the same source string keep CPython's results when
the compiled code is reused: each run binds into its own namespace, the
functions it defines are distinct objects, a source that warns warns on
every compile, a syntax error is raised every time, the caller's
`__future__` flags still decide how the source compiles, and classes that
`dataclasses` and `namedtuple` build from identical generated source work
independently."""
import warnings
from collections import namedtuple
from dataclasses import dataclass

src = "def f(x):\n    return x + k\nk = 1\n"
spaces = []
for k in range(3):
    ns = {"k": 0}
    exec(src, ns)
    ns["k"] = k
    spaces.append(ns)
assert [ns["f"](10) for ns in spaces] == [10, 11, 12]
assert spaces[0]["f"] is not spaces[1]["f"]
spaces[0]["f"].attr = 1
assert not hasattr(spaces[1]["f"], "attr")

for g in ({"a": 2}, {"a": 5}):
    assert eval("a * 3", g) == g["a"] * 3

with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    for _ in range(3):
        exec("s = '\\d'", {})
assert sum(issubclass(w.category, SyntaxWarning) for w in caught) == 3, caught

for _ in range(3):
    try:
        exec("def broken(:\n    pass\n", {})
    except SyntaxError:
        pass
    else:
        raise AssertionError("no SyntaxError")

# The same source under a caller whose `__future__` flags differ.
inner = "def g(x: undefined_name): pass\n"
plain = {}
exec(inner, plain)
future = {}
exec("from __future__ import annotations\nexec(inner, ns)", {"ns": future, "inner": inner})
assert future["g"].__annotations__ == {"x": "undefined_name"}
try:
    plain["g"].__annotations__
except NameError:
    pass
else:
    raise AssertionError("annotations evaluated as strings")

made = []
for i in range(50):

    @dataclass(frozen=(i % 2 == 0))
    class D:
        a: int
        b: str = "b"

    NT = namedtuple("NT", ["p", "q"])
    made.append((D, NT))
    assert D(i).a == i and repr(D(1, "z")).endswith("D(a=1, b='z')")
    assert NT(1, 2).q == 2
assert made[0][0] is not made[1][0]
assert made[0][0](1) != made[1][0](1)
assert made[0][0](1) == made[0][0](1)
try:
    made[0][0](1).a = 2
except Exception as e:
    assert type(e).__name__ == "FrozenInstanceError", e
else:
    raise AssertionError("not frozen")
d = made[1][0](1)
d.a = 2
assert d.a == 2
print("ok")
