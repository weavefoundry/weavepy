"""Operations compiled loops now finish in place: set algebra whose result
is discarded or stored, iterating an io.StringIO (and a closed one's
ValueError), Counter.most_common(n), and copy.deepcopy of plain trees,
a method whose first call changes its class (a lazily set class
attribute), and string.Template.substitute, each giving CPython's
results and errors, with the general path still taking what the native
one declines (user objects, a subclass, a memo)."""

import collections
import copy
import io
import unittest


class K:
    def __init__(self, k):
        self.k = k

    def __eq__(self, o):
        return isinstance(o, K) and self.k == o.k

    def __hash__(self):
        return hash(self.k)


class SubCounter(collections.Counter):
    pass


def set_ops(n, a, b):
    out = None
    for i in range(n):
        a | b
        x = a | b
        y = a & b
        z = a - b
        w = a ^ b
        out = (x, y, z, w)
    return out


def lines(n, text):
    out = None
    for i in range(n):
        got = []
        for line in io.StringIO(text):
            got.append(line)
        out = got
    return out


def ranks(n, c, k):
    out = None
    for i in range(n):
        out = c.most_common(k)
    return out


def deep(n, x):
    out = None
    for i in range(n):
        out = copy.deepcopy(x)
    return out


class Lazy:
    ready = False

    def __init__(self, v):
        self.v = v

    def get(self):
        if not Lazy.ready:
            Lazy.ready = True
        return self.v


def lazy_calls(n):
    total = 0
    o = Lazy(2)
    for i in range(n):
        total += o.get()
    return total


class CompiledLoopTests(unittest.TestCase):
    def test_method_after_class_change(self):
        self.assertEqual(lazy_calls(500), 1000)
        Lazy.get = lambda self: -1
        self.assertEqual(lazy_calls(300), -300)

    def test_template(self):
        import string

        t = string.Template("$a and ${b}")
        out = None
        for i in range(400):
            out = t.substitute(a=i, b="x")
        self.assertEqual(out, "399 and x")
        with self.assertRaises(KeyError):
            for i in range(300):
                t.substitute(a=1)

    def test_set_ops(self):
        for n in (1, 400):
            self.assertEqual(set_ops(n, {1, 2}, {2, 3}), ({1, 2, 3}, {2}, {1}, {1, 3}))
            x, y, z, w = set_ops(n, frozenset({1, 2}), {2, 3})
            self.assertIs(type(x), frozenset)
            self.assertEqual((x, y, z, w), ({1, 2, 3}, {2}, {1}, {1, 3}))
            ks = set_ops(n, {K(1), K(2)}, {K(2)})
            self.assertEqual(len(ks[0]), 2)
            self.assertEqual(ks[1], {K(2)})
        with self.assertRaises(TypeError):
            set_ops(300, {1}, [1])

    def test_stringio_lines(self):
        for n in (1, 400):
            self.assertEqual(lines(n, "a,1\nb,2\nc"), ["a,1\n", "b,2\n", "c"])
            self.assertEqual(lines(n, ""), [])

        def closed(n):
            caught = 0
            for i in range(n):
                f = io.StringIO("x\n")
                f.close()
                try:
                    for line in f:
                        pass
                except ValueError:
                    caught += 1
            return caught

        self.assertEqual(closed(300), 300)

    def test_most_common(self):
        c = collections.Counter("abracadabra")
        for n in (1, 400):
            self.assertEqual(ranks(n, c, 2), [("a", 5), ("b", 2)])
            self.assertEqual(ranks(n, c, 0), [])
            self.assertEqual(ranks(n, SubCounter("aab"), 1), [("a", 2)])
            self.assertEqual(ranks(n, collections.Counter({"x": 1.5, "y": 2}), 1), [("y", 2)])

    def test_deepcopy(self):
        tree = {"a": [1, 2, (3, "x")], "b": {"c": None}}
        for n in (1, 400):
            got = deep(n, tree)
            self.assertEqual(got, tree)
            self.assertIsNot(got, tree)
            self.assertIsNot(got["a"], tree["a"])
            k = K(1)
            mixed = deep(n, [k, [k]])
            self.assertIs(mixed[0], mixed[1][0])
            self.assertIsNot(mixed[0], k)
        shared = [1]
        both = deep(300, [shared, shared])
        self.assertIs(both[0], both[1])


if __name__ == "__main__":
    unittest.main()
