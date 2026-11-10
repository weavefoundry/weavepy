"""`for i, x in enumerate(...)` loops from compiled code end as the
interpreter ends them: over a list, a tuple and a range, a loop's `else`,
a shared enumerate object resumed after its loop, a list grown after the
loop, items whose finalizers run when a temporary list dies with the
loop, and `dict.setdefault` with plain and other keys."""

import gc


class Mortal:
    log = []

    def __init__(self, v):
        self.v = v

    def __del__(self):
        Mortal.log.append(self.v)


def sums(n):
    xs = [3, 1, 2]
    t = (4, 5)
    total = 0
    for k in range(n):
        for i, x in enumerate(xs):
            total += i * x
        for i, x in enumerate(t):
            total += i + x
        for i, x in enumerate(range(3)):
            total += x
        for i, x in enumerate(xs):
            if x == 99:
                break
        else:
            total += 1
    return total


for _ in range(4):
    assert sums(200) == 200 * ((0 * 3 + 1 * 1 + 2 * 2) + (0 + 4 + 1 + 5) + 3 + 1)


def shared(n):
    out = []
    for k in range(n):
        xs = [1, 2]
        e = enumerate(xs)
        for i, x in e:
            out.append(x)
        xs.append(3)
        out.append(next(e, None))
    return out


for _ in range(3):
    assert shared(100) == [1, 2, None] * 100


def temporaries(n):
    seen = 0
    for k in range(n):
        Mortal.log.clear()
        for i, m in enumerate([Mortal(k), Mortal(k + 1)]):
            pass
        del m
        seen += len(Mortal.log)
    return seen


gc.collect()
for _ in range(3):
    assert temporaries(100) == 200


def defaults(n):
    d = {}
    for k in range(n):
        d.setdefault("a", []).append(k)
        d.setdefault(k % 3, set()).add(k)
        d.setdefault((k % 2, "t"), 0)
        d.setdefault(1.5, "f")
    return d


for _ in range(3):
    d = defaults(30)
    assert d["a"] == list(range(30))
    assert d[0] == set(range(0, 30, 3)) and d[2] == set(range(2, 30, 3))
    assert d[(0, "t")] == 0 and d[1.5] == "f"
    assert list(d)[:3] == ["a", 0, (0, "t")]
print("ok")
