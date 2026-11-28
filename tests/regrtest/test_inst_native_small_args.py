"""Native methods of built-in subclasses' instances called with small ints.

`od.move_to_end(5)`, `xs.pop(0)` and `xs.insert(1, x)` on instances of
built-in subclasses run the native method on the arguments in place from
compiled code: the results, the errors they raise (a missing key, an
index out of range), and a method a subclass shadows or a class changed
mid-loop behave as in CPython. Run it past the compile threshold.
"""

from collections import OrderedDict


class L(list):
    pass


class D(dict):
    pass


def moves(od, n):
    for _ in range(n):
        od.move_to_end(2)
        od.move_to_end(0)
    return list(od)


def pops(xs, n):
    out = 0
    for i in range(n):
        xs.insert(0, i)
        out += xs.pop(0)
        xs.insert(1, i)
        out -= xs.pop(1)
    return out


def gets(d, n):
    t = 0
    for _ in range(n):
        t += d.get(3, 100) + d.get(250, 7)
    return t


def missing(od, n):
    hits = 0
    for _ in range(n):
        try:
            od.move_to_end(99)
        except KeyError as e:
            hits += e.args == (99,)
    return hits


N = 3000
od = OrderedDict((i, i) for i in range(5))
assert moves(od, N) == [1, 3, 4, 2, 0]
xs = L([9, 8])
assert pops(xs, N) == 0 and xs == [9, 8]
d = D({3: 1})
assert gets(d, N) == 8 * N
assert missing(od, N) == N


class Moving(OrderedDict):
    pass


m = Moving((i, i) for i in range(3))
assert moves(m, N) == [1, 2, 0]
Moving.move_to_end = lambda self, k, last=True: None
assert moves(m, 10) == [1, 2, 0]
m2 = Moving((i, i) for i in range(3))
assert moves(m2, 10) == [0, 1, 2]


class Shadow(L):
    def pop(self, i=-1):
        return 0


sh = Shadow([1])
assert pops(sh, N) == 0 and len(sh) == 1 + 2 * N, len(sh)
print("ok")
