"""Builtin methods with a native fast half, and list.pop(i), called on a
local list, dict, set or str from compiled loops: the call runs in place
when the receiver is the exact type, raises the method's own error at the
call, and leaves a subclass's override, a receiver that changes type, and
anything the fast half declines to the full call."""

import unittest


class L(list):
    def pop(self, i=-1):
        return "mine"

    def sort(self, **kw):
        self.append("sorted")


class K:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v > other.v


def pops(n):
    got = []
    for i in range(n):
        xs = [10, 20, 30, 40]
        a = xs.pop(0)
        b = xs.pop(-1)
        c = xs.pop(1)
        got.append((a, b, c, xs))
    return got[-1]


def pop_errors(n):
    caught = 0
    for i in range(n):
        xs = [1]
        try:
            xs.pop(5)
        except IndexError as e:
            caught += str(e) == "pop index out of range"
        try:
            xs.pop("x")
        except TypeError:
            caught += 1
        e = []
        try:
            e.pop(0)
        except IndexError as e2:
            caught += str(e2) == "pop from empty list"
    return caught


def sorts(n):
    out = None
    for i in range(n):
        xs = [5, 3, 8, 1]
        xs.sort()
        ys = ["b", "a", "c"]
        ys.sort()
        zs = [3.5, -1.0, 2.0]
        zs.sort()
        ks = [K(1), K(3), K(2)]
        ks.sort()
        out = (xs, ys, zs, [k.v for k in ks])
    return out


def sort_errors(n):
    caught = 0
    for i in range(n):
        xs = [1, "a", 2]
        try:
            xs.sort()
        except TypeError:
            caught += 1
    return caught


def mixed(seq, n):
    out = []
    for i in range(n):
        out.append(seq.pop(0) if i % 2 == 0 else None)
    return out


class BuiltinFastMethodSiteTests(unittest.TestCase):
    def test_pop_index(self):
        for n in (1, 400):
            self.assertEqual(pops(n), (10, 40, 30, [20]))

    def test_pop_errors(self):
        self.assertEqual(pop_errors(300), 900)

    def test_sort(self):
        want = ([1, 3, 5, 8], ["a", "b", "c"], [-1.0, 2.0, 3.5], [3, 2, 1])
        for n in (1, 400):
            self.assertEqual(sorts(n), want)
        self.assertEqual(sort_errors(300), 300)

    def test_subclass_and_type_change(self):
        def run(xs, n):
            out = []
            for i in range(n):
                out.append(xs.pop(0))
                xs.append(i)
            return out

        self.assertEqual(run([0, 1, 2], 300)[:4], [0, 1, 2, 0])
        self.assertEqual(run(L([0, 1]), 300)[:3], ["mine", "mine", "mine"])

        def sort_it(xs, n):
            for i in range(n):
                xs.sort()
            return xs

        sort_it([2, 1], 300)
        sub = sort_it(L([2, 1]), 3)
        self.assertEqual(sub, [2, 1, "sorted", "sorted", "sorted"])
        self.assertEqual(mixed(list(range(400)), 400)[:4], [0, None, 1, None])

    def test_str_and_set_methods(self):
        def strs(n):
            out = None
            for i in range(n):
                s = "a,b,,c"
                out = (s.count(","), s.rfind(","), s.title(), s.zfill(8))
            return out

        def sets(n):
            out = None
            for i in range(n):
                a = {1, 2, 3}
                b = {2, 3}
                out = (b.issubset(a), a.isdisjoint(b), sorted(a.difference(b)))
            return out

        for n in (1, 400):
            self.assertEqual(strs(n), (3, 4, "A,B,,C", "00a,b,,c"))
            self.assertEqual(sets(n), (True, False, [1]))


if __name__ == "__main__":
    unittest.main()
