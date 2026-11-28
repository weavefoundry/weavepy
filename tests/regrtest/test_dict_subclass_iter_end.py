"""The end of a loop over a `dict` subclass's keys, values or items.

`heapq.nlargest` loops over `iter(counter.items())` kept in a local.
Compiled code ends such a loop in place when the instance lives on; an
iterator that stays exhausted after its dict grows, and a temporary
instance whose `__del__` runs when the loop lets it go, keep CPython's
behavior. Run it past the compile threshold.
"""

import collections

log = []


class D(dict):
    def __del__(self):
        log.append("del")


def drain(d):
    it = iter(d.items())
    n = 0
    for kv in it:
        n += 1
    d["late"] = 1
    assert next(it, None) is None
    return n


def temporary():
    n = 0
    for k in D(a=1, b=2):
        n += 1
    log.append("after")
    return n


c = collections.Counter("aabbc")
for _ in range(3000):
    assert drain(collections.Counter("aab")) == 2
    before = len(c)
    assert drain(c) == before
    del log[:]
    assert temporary() == 2
    assert log == ["del", "after"], log
