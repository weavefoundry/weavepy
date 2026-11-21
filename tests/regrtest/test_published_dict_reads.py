"""Attribute reads of instances whose `__dict__` was published.

`copy.copy`, `pickle` and `vars()` publish an instance's dictionary.
Compiled code then reads an attribute through the position the site last
found the name at. That position has to follow a key deleted and added
back, a class that gains a data descriptor for the name, and a dict
replaced through `__dict__`. Run it past the compile threshold.
"""

import copy


class P:
    def __init__(self):
        self.a = 1
        self.b = 2


def read(o):
    return o.a + o.b


q = copy.copy(P())
r = P()
vars(r)
for _ in range(3000):
    assert read(q) == 3
    assert read(r) == 3

del r.a
r.c = 5
r.a = 10
for _ in range(100):
    assert read(r) == 12, read(r)

r.__dict__ = {"b": 7, "a": 1}
for _ in range(100):
    assert read(r) == 8, read(r)

P.a = property(lambda self: 100)
for _ in range(100):
    assert read(r) == 107, read(r)
    assert read(q) == 102, read(q)
del P.a
for _ in range(100):
    assert read(q) == 3, read(q)

del q.b
try:
    read(q)
except AttributeError:
    pass
else:
    raise AssertionError("no AttributeError")
