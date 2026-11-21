"""Dict literals in compiled code.

Distinct `str` and `int` keys go in without probing for an equal one; a
repeated key, `True` against `1`, and `None` keys take the probe, the last
value winning and the first key kept; a literal that closes a cycle is
still collected. Run it past the compile threshold.
"""

import gc
def f(n):
    for i in range(n):
        d = {"a": i, "b": [i], 1: "x", "a": 5}
        assert d == {"a": 5, "b": [i], 1: "x"} and list(d) == ["a", "b", 1]
        e = {1: "a", True: "b", 2: "c", 2: "d"}
        assert e == {1: "b", 2: "d"} and type(next(iter(e))) is int
        g = {None: 1, "k": 2, None: 3}
        assert g == {None: 3, "k": 2}
f(5000)
l = []
d = {"x": l}
l.append(d)
del d, l
gc.collect()
print("ok")
