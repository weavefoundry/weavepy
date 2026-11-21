"""`dir()` of classes, instances, functions, modules and aliases.

`inspect.getmembers` starts from `dir(cls)`. The names come back sorted
and without duplicates, an instance's own attributes among its class's,
a function's attributes among its type's, and a PEP 604 union without
its private bookkeeping. Run it past the compile threshold.
"""

import sys


class A:
    x = 1

    def m(self):
        pass


class B(A):
    y = 2


def f():
    pass


f.tag = 1
a = B()
a.z = 3

for _ in range(2000):
    for o in (A, B, a, f, a.m, sys, 1, "s", [1], list[int], int | str):
        d = dir(o)
        assert d == sorted(set(d)), o
    assert {"x", "y", "m", "__init__", "__class__"} <= set(dir(B))
    assert {"x", "y", "z", "m"} <= set(dir(a))
    assert "tag" in dir(f) and "__code__" in dir(f)
    u = dir(int | str)
    assert "__origin__" in u and "__is_pep604_union__" not in u
    assert "__origin__" in dir(list[int]) and "append" in dir(list[int])
