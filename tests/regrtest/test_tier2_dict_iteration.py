"""Loops over a dict and its views from code tier 2 compiles step as the
interpreter's do: keys, values and items in order, a dict changed during
the loop raising where CPython raises, a loop left early and its iterator
resumed, an `OrderedDict` and a dict subclass, and short inner loops that
tier 2 enters a few iterations at a time."""

import collections


def keys(d, n):
    acc = 0
    for _ in range(n):
        for k in d:
            acc += k
    return acc


def views(d, n):
    acc = 0
    for _ in range(n):
        for v in d.values():
            acc += v
        for k, v in d.items():
            acc += k * v
        for k in d.keys():
            acc -= k
    return acc


def strings(d, n):
    out = None
    for _ in range(n):
        out = []
        for k in d:
            out.append(k)
    return out


def grows(d, n):
    hits = 0
    for i in range(n):
        try:
            for k in d:
                if k == 3:
                    d[100 + i] = 0
        except RuntimeError as e:
            assert "changed size during iteration" in str(e), e
            hits += 1
            del d[100 + i]
    return hits


def rewrites(d, n):
    # Storing to an existing key keeps the size: the loop goes on.
    acc = 0
    for _ in range(n):
        for k in d:
            d[k] = d[k] + 1
            acc += d[k]
    return acc


def early_exit(d, n):
    acc = 0
    for _ in range(n):
        it = iter(d)
        for k in it:
            if k == 4:
                break
        acc += next(it) + sum(it)
    return acc


def sorted_inner(xs, n):
    acc = 0
    for _ in range(n):
        for x in sorted(xs):
            acc += x
        for x in reversed(xs):
            acc -= x
    return acc


class Sub(dict):
    pass


N = 3000
for _ in range(3):
    d = {i: i * 2 for i in range(10)}
    assert keys(d, N) == 45 * N
    assert views(d, N) == (90 + 2 * 285 - 45) * N
    assert strings({"a": 1, "b": 2, "c": 3}, N) == ["a", "b", "c"]
    assert grows(dict(d), N) == N
    assert rewrites(dict(d), 50) == sum(90 + 10 * j for j in range(1, 51))
    assert early_exit(d, N) == (5 + 6 + 7 + 8 + 9) * N
    assert sorted_inner([3, 1, 2, 5, 4, 9, 8, 7, 6, 0], N) == 0
    od = collections.OrderedDict((i, i) for i in range(8))
    od.move_to_end(0)
    assert keys(od, N) == 28 * N
    assert strings(od, 10) == [1, 2, 3, 4, 5, 6, 7, 0]
    assert keys(Sub(d), N) == 45 * N
    assert keys({}, N) == 0
