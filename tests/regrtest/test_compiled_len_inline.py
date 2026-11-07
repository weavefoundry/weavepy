"""Compiled `len(x)` keeps CPython's results where it reads the length in
line: lists (grown and cleared), tuples, ASCII and non-ASCII strings, a
fresh value only the call holds, dicts and sets, objects with `__len__`
(including one that raises), a `len` the module rebinds, and a list
subclass."""
import builtins


class Sized:
    def __len__(self):
        return 7


class Bad:
    def __len__(self):
        raise ValueError("no")


class L(list):
    def __len__(self):
        return 99


def lens(xs):
    t = []
    for x in xs:
        t.append(len(x))
    return t


def fresh(n):
    t = 0
    for i in range(n):
        t += len([i, i]) + len("ab" * (i % 3))
    return t


items = [[1, 2], (1, 2, 3), "abcd", "héllo", "", (), {1: 2}, {1, 2, 3}, Sized(), L([1])]
want = [2, 3, 4, 5, 0, 0, 1, 3, 7, 99]
for _ in range(3000):
    assert lens(items) == want
grow = []
for i in range(3000):
    grow.append(i)
    assert lens([grow])[0] == i + 1
grow.clear()
assert lens([grow]) == [0]
assert fresh(3000) == sum(2 + 2 * (i % 3) for i in range(3000))
try:
    lens([Bad()])
except ValueError:
    pass
else:
    raise AssertionError("no ValueError")
len = lambda x: -1
assert lens(["abc"]) == [-1]
del len
assert lens(["abc"]) == [3]
builtins.len, real = (lambda x: -2), builtins.len
try:
    assert lens(["abc"]) == [-2]
finally:
    builtins.len = real
assert lens(["abc"]) == [3]
print("ok")
