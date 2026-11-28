"""`sum()` of a dict's values or keys, read from the dict in place.

Integers (machine or big), floats, bools and a `start` sum as CPython
sums them; an item `sum` can't add raises the same `TypeError`; an
items view and subclasses' views keep working. Run it past the compile
threshold.
"""

import collections


def run(n):
    for i in range(n):
        d = {"a": i, "b": 2, "c": True}
        assert sum(d.values()) == i + 3 and sum(d.values(), 10) == i + 13
        assert sum({1: 0, 2: 0}.keys()) == 3 and sum({}.values()) == 0
        f = {"x": 1.5, "y": 2}
        assert sum(f.values()) == 3.5 and type(sum(f.values())) is float
        big = {"p": 2**70, "q": 1}
        assert sum(big.values()) == 2**70 + 1
        assert sum({"m": 2**62, "n": 2**62}.values()) == 2**63
        assert sum(d.values(), 0.5) == i + 3.5
        try:
            sum({"s": "x"}.values())
        except TypeError:
            pass
        else:
            raise AssertionError("summed a str")
        assert sum({"k": (1,)}.items(), ()) == ("k", (1,))
        assert sum(collections.Counter("abca").values()) == 4
        assert sum(collections.OrderedDict(a=1, b=2).values()) == 3


run(3000)
print("ok")
