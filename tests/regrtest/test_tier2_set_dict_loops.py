"""Loops over sets and dict keys in tier-2 compiled code.

A set's or a dict's elements step in place, whatever their kind (ints,
strings, tuples, instances, `None`); a set or dict that changes size
mid-loop raises as in CPython, and one that changes and changes back is
the general step's to judge. Run it past the compile thresholds.
"""


class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, K) and self.v == other.v


def count(n, s):
    t = 0
    for _ in range(n):
        for x in s:
            t += 1
    return t


def collect(s):
    out = []
    for x in s:
        out.append(x)
    return out


def grow(s):
    for x in s:
        s.add(len(s) + 100)


def grow_dict(d):
    for k in d:
        d[len(d) + 100] = 0


N = 3000
ints = set(range(10))
strs = {str(i) for i in range(10)}
mixed = {1, "a", (2, 3), None, K(4), 2.5, frozenset({1})}
d = {str(i): i for i in range(10)}
di = {i: i for i in range(10)}
assert count(N, ints) == 10 * N
assert count(N, strs) == 10 * N
assert count(N, mixed) == 7 * N
assert count(N, frozenset(strs)) == 10 * N
assert count(N, d) == 10 * N
assert count(N, di) == 10 * N
for s in (ints, strs, mixed, d, di):
    assert collect(s) == list(s)
for _ in range(N // 100):
    try:
        grow(set(range(5)))
    except RuntimeError:
        pass
    else:
        raise AssertionError("set grew during iteration")
    try:
        grow_dict(dict.fromkeys(range(5)))
    except RuntimeError:
        pass
    else:
        raise AssertionError("dict grew during iteration")
print("ok")
