"""`for a, b in rows` over lists and tuples of pairs in tier-2 code.

A pair whose halves fit the loop's lanes is read where it sits; a row of
another length, a row that isn't a tuple, halves outside the lanes, and a
list that grows mid-loop behave as in CPython. Run it past the compile
thresholds.
"""


def total(n, rows):
    t = 0
    for _ in range(n):
        for a, b in rows:
            t += a * b
    return t


def keys(rows):
    out = []
    for a, b in rows:
        out.append(a)
    return out


def grow(rows):
    seen = 0
    for a, b in rows:
        seen += 1
        if len(rows) < 6:
            rows.append((a + 1, b))
    return seen


N = 3000
ints = [(i, i + 1) for i in range(10)]
assert total(N, ints) == sum(i * (i + 1) for i in range(10)) * N
assert total(N, tuple(ints)) == sum(i * (i + 1) for i in range(10)) * N
floats = [(i * 0.5, 2.0) for i in range(4)]
assert total(N, floats) == 6.0 * N
assert total(1, [(1, 2), (2.5, 2)]) == 7.0
assert total(1, [(1, 2), [3, 4]]) == 14
assert keys([(2, "ab"), ("c", 3)]) == [2, "c"]
for bad in ([(1, 2, 3)], [(1,)], [5]):
    try:
        total(1, bad)
    except (ValueError, TypeError):
        pass
    else:
        raise AssertionError(bad)
assert keys([("x", 1), ("y", None), (None, 2)]) == ["x", "y", None]
for _ in range(N // 10):
    assert grow([(1, 1), (2, 2)]) == 6
print("ok")
