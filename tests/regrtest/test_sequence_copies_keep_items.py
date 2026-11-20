"""Copies of a list's or tuple's items share the items, not the container.

`tuple(xs)`, `list(xs)`, `xs[:]`, `xs.copy()`, `a + b`, `t * n`,
`(*xs,)` and slices copy the items in one pass. Each copy holds its own
reference to every item: dropping the copy, the source, or both, in any
order, releases each item exactly once, and an item only the copy holds
dies with the copy.
"""

import gc
import sys
import unittest
import weakref


class Item:
    """A heap item whose death can be observed."""

    __slots__ = ("n", "__weakref__")

    def __init__(self, n):
        self.n = n

    def __repr__(self):
        return f"Item({self.n})"


def items(n):
    return [Item(i) for i in range(n)]


COPIERS = {
    "tuple(list)": lambda xs: tuple(xs),
    "list(list)": lambda xs: list(xs),
    "list(tuple)": lambda xs: list(tuple(xs)),
    "tuple(tuple slice)": lambda xs: tuple(xs)[0:],
    "xs[:]": lambda xs: xs[:],
    "xs.copy()": lambda xs: xs.copy(),
    "xs + []": lambda xs: xs + [],
    "[] + xs": lambda xs: [] + xs,
    "t + ()": lambda xs: tuple(xs) + (),
    "[*xs]": lambda xs: [*xs],
    "(*xs,)": lambda xs: (*xs,),
    "xs * 1": lambda xs: xs * 1,
    "t * 1": lambda xs: tuple(xs) * 1,
    "sorted by n": lambda xs: sorted(xs, key=lambda item: item.n),
    "list(reversed)": lambda xs: list(reversed(xs))[::-1],
    "tuple(reversed)": lambda xs: tuple(reversed(xs))[::-1],
    "extend": lambda xs: (lambda out: (out.extend(xs), out)[1])([]),
    "extend tuple": lambda xs: (lambda out: (out.extend(tuple(xs)), out)[1])([]),
}


class CopiesShareItemsTests(unittest.TestCase):
    def test_every_copy_holds_the_same_items(self):
        for name, copy in COPIERS.items():
            for n in (0, 1, 2, 7, 300):
                with self.subTest(name=name, n=n):
                    xs = items(n)
                    out = copy(xs)
                    self.assertEqual(len(out), n)
                    for a, b in zip(xs, out):
                        self.assertIs(a, b)

    def test_items_outlive_the_source(self):
        for name, copy in COPIERS.items():
            with self.subTest(name=name):
                xs = items(50)
                refs = [weakref.ref(x) for x in xs]
                out = copy(xs)
                del xs
                gc.collect()
                self.assertTrue(all(r() is not None for r in refs))
                self.assertEqual([x.n for x in out], list(range(50)))
                del out
                gc.collect()
                self.assertTrue(all(r() is None for r in refs))

    def test_items_outlive_the_copy(self):
        for name, copy in COPIERS.items():
            with self.subTest(name=name):
                xs = items(50)
                refs = [weakref.ref(x) for x in xs]
                out = copy(xs)
                del out
                gc.collect()
                self.assertTrue(all(r() is not None for r in refs))
                self.assertEqual([x.n for x in xs], list(range(50)))
                del xs
                gc.collect()
                self.assertTrue(all(r() is None for r in refs))

    def test_reference_counts_return_to_where_they_started(self):
        probe = Item(-1)
        xs = [probe] * 40 + ["text", 3, 2.5, None, (probe,)]
        before = sys.getrefcount(probe)
        for name, copy in COPIERS.items():
            if name == "sorted by n":
                continue
            with self.subTest(name=name):
                for _ in range(30):
                    out = copy(xs)
                    self.assertEqual(sys.getrefcount(probe), before + 40)
                    del out
                self.assertEqual(sys.getrefcount(probe), before)

    def test_a_copy_can_be_the_only_owner(self):
        for name, copy in COPIERS.items():
            with self.subTest(name=name):
                out = copy(items(20))
                self.assertEqual([x.n for x in out], list(range(20)))
                refs = [weakref.ref(x) for x in out]
                del out
                gc.collect()
                self.assertTrue(all(r() is None for r in refs))


class ConcatAndRepeatTests(unittest.TestCase):
    def test_tuple_concatenation(self):
        a = tuple(items(5))
        b = tuple(items(3))
        c = a + b
        self.assertEqual(len(c), 8)
        self.assertTrue(all(x is y for x, y in zip(c, a + b)))
        self.assertIs(c[0], a[0])
        self.assertIs(c[5], b[0])
        self.assertIs(a + (), a)
        self.assertIs(() + a, a)
        self.assertIs(() + (), ())

    def test_list_concatenation_is_a_new_list(self):
        a = items(4)
        b = items(4)
        c = a + b
        self.assertIsNot(c, a)
        c.append(None)
        self.assertEqual(len(a), 4)
        self.assertEqual(len(b), 4)
        self.assertEqual([x.n for x in c[:8]], [0, 1, 2, 3, 0, 1, 2, 3])
        d = a + a
        self.assertEqual(len(d), 8)
        self.assertIs(d[0], d[4])

    def test_repeats(self):
        a = items(3)
        for n in (-1, 0, 1, 2, 5, 400):
            with self.subTest(n=n):
                want = max(n, 0) * 3
                out = a * n
                self.assertEqual(len(out), want)
                self.assertTrue(all(out[i] is a[i % 3] for i in range(want)))
                t = tuple(a) * n
                self.assertEqual(len(t), want)
                self.assertTrue(all(t[i] is a[i % 3] for i in range(want)))
        ints = list(range(1000))
        self.assertEqual(ints * 2, ints + ints)
        self.assertEqual((ints * 3)[2000:], ints)
        self.assertEqual(ints, list(range(1000)))
        t = tuple(a)
        self.assertIs(t * 1, t)

    def test_extending_a_list_with_itself_doubles_it(self):
        xs = items(6)
        xs.extend(xs)
        self.assertEqual([x.n for x in xs], [0, 1, 2, 3, 4, 5] * 2)
        xs += xs
        self.assertEqual(len(xs), 24)

    def test_slices(self):
        xs = items(40)
        t = tuple(xs)
        for lo, hi, step in [
            (None, None, None), (3, 17, None), (-5, None, None), (None, -5, None),
            (30, 10, None), (None, None, -1), (None, None, 3), (35, 2, -4), (100, 200, None),
        ]:
            with self.subTest(lo=lo, hi=hi, step=step):
                want = [xs[i] for i in range(*slice(lo, hi, step).indices(40))]
                got_list = xs[lo:hi:step]
                got_tuple = t[lo:hi:step]
                self.assertEqual(len(got_list), len(want))
                self.assertTrue(all(a is b for a, b in zip(got_list, want)))
                self.assertIsInstance(got_tuple, tuple)
                self.assertTrue(all(a is b for a, b in zip(got_tuple, want)))
        self.assertIs(t[:], t)
        self.assertIs(t[10:10], ())


class TupleHashTests(unittest.TestCase):
    def test_int_tuples_hash_as_cpython_does(self):
        # The combination is fixed by the language's implementation, and an
        # int's hash doesn't depend on the hash seed.
        if sys.hash_info.width != 64:
            self.skipTest("64-bit hashes")
        self.assertEqual(hash(()), 5740354900026072187)
        self.assertEqual(hash((1, 2, 3)), 529344067295497451)
        self.assertEqual(hash((0,)), -8753497827991233192)
        self.assertEqual(hash(tuple(range(1000))), -5792488972412445855)
        self.assertEqual(hash((-1, -2, 2**61 - 1, 2**61, -(2**62))), hash((-2, -2, 0, 1, -2)))

    def test_equal_tuples_hash_equal(self):
        a = tuple(range(1, 500))
        b = tuple(list(range(1, 500)))
        self.assertEqual(hash(a), hash(b))
        self.assertEqual(hash(a[1:]), hash(tuple(range(2, 500))))
        self.assertEqual(hash(a + (7,)), hash((*a, 7)))
        self.assertEqual(hash(("a", 1, "b")), hash(("a", 1) + ("b",)))
        self.assertEqual(hash((1, 2.0, True)), hash((1.0, 2, 1)))
        self.assertEqual(hash((1, (2, (3, "x")))), hash((1, (2, (3, "x")))))

    def test_an_unhashable_item_still_raises(self):
        for bad in ((1, 2, []), (1, "a", {}), tuple(range(100)) + ([],), ((1, [2]),)):
            with self.subTest(bad=bad):
                with self.assertRaises(TypeError):
                    hash(bad)
                with self.assertRaises(TypeError):
                    {bad: 1}

    def test_a_python_hash_runs_once_per_hashing(self):
        calls = []

        class Key:
            def __hash__(self):
                calls.append(1)
                return 7

        t = (1, "a", Key(), 2)
        h = hash(t)
        self.assertEqual(hash((1, "a", 7, 2)), h)
        self.assertGreaterEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
