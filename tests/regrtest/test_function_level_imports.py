"""Imports inside functions that compiled code runs give what the import
statement gives every time: the module in `sys.modules` now (replaced,
removed, or a non-module object), a replaced `builtins.__import__`, names
missing from a module, submodules named in a from-list, and dotted
names."""

import builtins
import sys
import types


def plain(n):
    out = None
    for _ in range(n):
        import os

        out = os
    return out


def dotted(n):
    out = None
    for _ in range(n):
        import os.path

        out = os
    return out


def dotted_as(n):
    out = None
    for _ in range(n):
        import os.path as p

        out = p
    return out


def from_names(n):
    out = None
    for _ in range(n):
        from collections import OrderedDict, deque

        out = (OrderedDict, deque)
    return out


def from_submodule(n):
    out = None
    for _ in range(n):
        from os import path

        out = path
    return out


def helper(x):
    import math

    return math.floor(x) + 1


def in_callee(n):
    acc = 0
    for i in range(n):
        acc += helper(i + 0.5)
    return acc


def missing(n):
    hits = 0
    for _ in range(n):
        try:
            from os import no_such_name_here
        except ImportError as e:
            assert "no_such_name_here" in str(e), e
            hits += 1
    return hits


def fake_module(n):
    seen = []
    for i in range(n):
        import wp_fake_mod

        seen.append(wp_fake_mod.value)
        if i == n // 2:
            sys.modules["wp_fake_mod"] = types.SimpleNamespace(value="swapped")
    return seen[0], seen[-1]


import collections
import math
import os

N = 3000
for _ in range(3):
    assert plain(N) is os
    assert dotted(N) is os
    assert dotted_as(N) is os.path
    assert from_names(N) == (collections.OrderedDict, collections.deque)
    assert from_submodule(N) is os.path
    assert in_callee(N) == sum(math.floor(i + 0.5) + 1 for i in range(N))
    assert missing(N) == N
    m = types.ModuleType("wp_fake_mod")
    m.value = "first"
    sys.modules["wp_fake_mod"] = m
    assert fake_module(N) == ("first", "swapped")
    del sys.modules["wp_fake_mod"]

# A module removed from `sys.modules` is imported afresh; `None` there
# blocks the import.
first = __import__("colorsys")
assert plain(10) is os


def reimport():
    import colorsys

    return colorsys


for _ in range(N):
    assert reimport() is first
del sys.modules["colorsys"]
second = reimport()
assert second is not first and second is sys.modules["colorsys"]
for _ in range(N):
    assert reimport() is second
sys.modules["colorsys"] = None
try:
    reimport()
except ImportError:
    pass
else:
    raise AssertionError("None in sys.modules didn't block the import")
sys.modules["colorsys"] = second

# A replaced `__import__` is called for every import statement.
calls = []
real = builtins.__import__


def spy(name, *args, **kwargs):
    calls.append(name)
    return real(name, *args, **kwargs)


builtins.__import__ = spy
try:
    assert plain(5) is os
    assert from_submodule(5) is os.path
finally:
    builtins.__import__ = real
assert calls == ["os"] * 10, calls
