"""Reading, writing and appending an array's items.

`a[i]`, `a[i] = v`, `a.append(v)`, `len(a)`, iteration and plain slices
work on the array's packed bytes directly. Each costs the same whether
the array holds a few items or a million, gives the item its typecode
describes, and refuses what CPython's array refuses: a value of the
wrong type, an int out of the item's range, an index out of range, a
resize while a view of the array is exported.
"""

import array
import copy
import pickle
import time
import unittest
import weakref

INT_CODES = "bBhHiIlLqQ"


def limits(code):
    bits = 8 * array.array(code).itemsize
    if code.islower():
        return -(1 << (bits - 1)), (1 << (bits - 1)) - 1
    return 0, (1 << bits) - 1


class ItemTests(unittest.TestCase):
    def test_every_int_typecode_round_trips_its_range(self):
        for code in INT_CODES:
            lo, hi = limits(code)
            values = [lo, lo + 1, -1 if lo else 0, 0, 1, hi - 1, hi]
            with self.subTest(code=code):
                a = array.array(code, values)
                self.assertEqual(len(a), len(values))
                self.assertEqual([a[i] for i in range(len(a))], values)
                self.assertEqual([a[i - len(a)] for i in range(len(a))], values)
                self.assertEqual(list(a), values)
                self.assertEqual(a.tolist(), values)
                for i, v in enumerate(reversed(values)):
                    a[i] = v
                self.assertEqual(a.tolist(), values[::-1])
                b = array.array(code)
                for v in values:
                    b.append(v)
                self.assertEqual(b.tolist(), values)
                self.assertTrue(all(type(x) is int for x in b))

    def test_out_of_range_ints_are_refused(self):
        for code in INT_CODES:
            lo, hi = limits(code)
            with self.subTest(code=code):
                a = array.array(code, [0, 0])
                for bad in (lo - 1, hi + 1, lo - 2**70, hi + 2**70):
                    with self.assertRaises(OverflowError):
                        a[0] = bad
                    with self.assertRaises(OverflowError):
                        a.append(bad)
                    with self.assertRaises(OverflowError):
                        array.array(code, [0, bad])
                self.assertEqual(a.tolist(), [0, 0])

    def test_wrong_types_are_refused(self):
        a = array.array("i", [1, 2, 3])
        for bad in (1.5, "1", None, [1], 2 + 0j):
            with self.subTest(bad=bad):
                with self.assertRaises(TypeError):
                    a[0] = bad
                with self.assertRaises(TypeError):
                    a.append(bad)
        self.assertEqual(a.tolist(), [1, 2, 3])
        d = array.array("d", [1.0])
        for bad in ("1", None, [1.0], 1j):
            with self.subTest(bad=bad):
                with self.assertRaises(TypeError):
                    d[0] = bad
                with self.assertRaises(TypeError):
                    d.append(bad)
        self.assertEqual(d.tolist(), [1.0])

    def test_values_that_convert(self):
        class Index:
            def __index__(self):
                return 7

        class Real:
            def __float__(self):
                return 2.5

        a = array.array("i", [0, 0])
        a[0] = True
        a[1] = Index()
        a.append(Index())
        self.assertEqual(a.tolist(), [1, 7, 7])
        self.assertIs(type(a[0]), int)
        d = array.array("d", [0.0, 0.0])
        d[0] = 3
        d[1] = Real()
        d.append(True)
        d.append(Real())
        self.assertEqual(d.tolist(), [3.0, 2.5, 1.0, 2.5])
        self.assertTrue(all(type(x) is float for x in d))

    def test_floats(self):
        d = array.array("d", [0.5, -1.25])
        d[0] = 1e300
        d.append(float("inf"))
        d.append(float("-inf"))
        self.assertEqual(d.tolist()[:2], [1e300, -1.25])
        self.assertEqual(d[2], float("inf"))
        self.assertEqual(d[-1], float("-inf"))
        nan = array.array("d", [float("nan")])
        self.assertNotEqual(nan[0], nan[0])
        f = array.array("f", [1.5, 0.1])
        self.assertEqual(f[0], 1.5)
        self.assertAlmostEqual(f[1], 0.1, places=6)
        self.assertNotEqual(f[1], 0.1)
        f[0] = 2.25
        f.append(-0.0)
        self.assertEqual(f[0], 2.25)
        self.assertEqual(str(f[-1]), "-0.0")

    def test_large_unsigned_values(self):
        q = array.array("Q", [2**64 - 1, 2**63, 2**63 - 1, 0])
        self.assertEqual([q[i] for i in range(4)], [2**64 - 1, 2**63, 2**63 - 1, 0])
        self.assertEqual(list(q), [2**64 - 1, 2**63, 2**63 - 1, 0])
        q[3] = 2**64 - 2
        q.append(2**63 + 5)
        self.assertEqual(q[3], 2**64 - 2)
        self.assertEqual(q[-1], 2**63 + 5)
        self.assertEqual(sum(q), (2**64 - 1) + 2**63 + (2**63 - 1) + (2**64 - 2) + 2**63 + 5)

    def test_unicode_arrays(self):
        u = array.array("w", "héllo\U0001f600")
        self.assertEqual(u[1], "é")
        self.assertEqual(u[-1], "\U0001f600")
        self.assertEqual(list(u), list("héllo\U0001f600"))
        u[0] = "J"
        u.append("!")
        self.assertEqual(u.tounicode(), "Jéllo\U0001f600!")
        self.assertEqual(u[1:3].tounicode(), "él")
        with self.assertRaises(TypeError):
            u[0] = 65
        with self.assertRaises(TypeError):
            u.append("ab")

    def test_index_errors(self):
        a = array.array("i", [1, 2, 3])
        for bad in (3, -4, 10**9, -(10**9)):
            with self.subTest(bad=bad):
                with self.assertRaises(IndexError):
                    a[bad]
                with self.assertRaises(IndexError):
                    a[bad] = 0
        with self.assertRaises(TypeError):
            a["0"]
        with self.assertRaises(TypeError):
            a[1.0]
        self.assertEqual(a[True], 2)
        empty = array.array("i")
        with self.assertRaises(IndexError):
            empty[0]
        with self.assertRaises(IndexError):
            empty[-1]
        self.assertEqual(len(empty), 0)


class SliceTests(unittest.TestCase):
    def test_slices_match_a_list(self):
        values = list(range(40))
        a = array.array("h", values)
        for lo in (None, 0, 3, 17, 39, 40, 100, -1, -5, -40, -100):
            for hi in (None, 0, 3, 17, 39, 40, 100, -1, -5, -40, -100):
                for step in (None, 1, 2, -1, -3):
                    with self.subTest(lo=lo, hi=hi, step=step):
                        got = a[lo:hi:step]
                        self.assertIs(type(got), array.array)
                        self.assertEqual(got.typecode, "h")
                        self.assertEqual(got.tolist(), values[lo:hi:step])

    def test_a_slice_is_a_copy(self):
        a = array.array("i", range(10))
        b = a[2:5]
        b[0] = -1
        b.append(99)
        self.assertEqual(a.tolist(), list(range(10)))
        self.assertEqual(b.tolist(), [-1, 3, 4, 99])
        self.assertEqual(b.itemsize, a.itemsize)
        self.assertEqual(len(b), 4)
        self.assertEqual(copy.copy(b), b)
        self.assertEqual(pickle.loads(pickle.dumps(b)), b)
        ref = weakref.ref(b)
        self.assertIs(ref(), b)
        self.assertEqual(sum(b), 105)
        self.assertEqual(b + b, array.array("i", [-1, 3, 4, 99] * 2))

    def test_slice_stores_from_another_array(self):
        base = list(range(10))
        for lo, hi in [(2, 5), (0, 0), (10, 10), (5, 2), (-3, None), (None, 4), (0, 10)]:
            for rep in ([], [70], [70, 80, 90], list(range(100, 120))):
                with self.subTest(lo=lo, hi=hi, rep=rep):
                    a = array.array("i", base)
                    want = list(base)
                    a[lo:hi] = array.array("i", rep)
                    want[lo:hi] = rep
                    self.assertEqual(a.tolist(), want)
        a = array.array("i", base)
        a[1:3] = a
        want = list(base)
        want[1:3] = base
        self.assertEqual(a.tolist(), want)
        with self.assertRaises(TypeError):
            a[0:1] = array.array("d", [1.0])

    def test_subclass_instances(self):
        class Samples(array.array):
            def total(self):
                return sum(self)

        s = Samples("d", [1.5, 2.5, 3.5])
        self.assertEqual(s[0], 1.5)
        self.assertEqual(s[-1], 3.5)
        s[1] = 10.0
        s.append(4.0)
        self.assertEqual(len(s), 4)
        self.assertEqual(s.total(), 19.0)
        self.assertEqual(list(s), [1.5, 10.0, 3.5, 4.0])
        self.assertIs(type(s[1:]), array.array)
        self.assertEqual(s[1:].tolist(), [10.0, 3.5, 4.0])

        class Doubling(array.array):
            def __getitem__(self, i):
                return 2 * array.array.__getitem__(self, i)

            def __len__(self):
                return 1

        d = Doubling("i", [1, 2, 3])
        self.assertEqual(d[0], 2)
        self.assertEqual(d[2], 6)
        self.assertEqual(len(d), 1)
        self.assertEqual(array.array.__len__(d), 3)
        self.assertEqual(list(d), [1, 2, 3])


class IteratorTests(unittest.TestCase):
    def test_iteration_sees_later_appends(self):
        a = array.array("i", [1, 2, 3])
        it = iter(a)
        self.assertEqual(next(it), 1)
        a.append(4)
        self.assertEqual(list(it), [2, 3, 4])
        # Once finished, an iterator stays finished.
        a.append(5)
        self.assertEqual(list(it), [])
        with self.assertRaises(StopIteration):
            next(it)

    def test_iteration_over_a_shrinking_array(self):
        a = array.array("i", range(10))
        seen = []
        for x in a:
            seen.append(x)
            if x == 3:
                del a[5:]
        self.assertEqual(seen, [0, 1, 2, 3, 4])

    def test_loops(self):
        a = array.array("l", range(5000))
        total = 0
        for x in a:
            total += x
        self.assertEqual(total, 12497500)
        self.assertEqual(sum(a), 12497500)
        self.assertEqual(max(a), 4999)
        self.assertEqual([x for x in a if x % 1000 == 0], [0, 1000, 2000, 3000, 4000])
        self.assertEqual(sum(a[i] for i in range(0, 5000, 7)), sum(range(0, 5000, 7)))
        self.assertIn(4999, a)
        self.assertNotIn(5000, a)
        self.assertEqual(a.index(4321), 4321)
        self.assertEqual(a.count(17), 1)
        self.assertEqual(list(reversed(a))[:3], [4999, 4998, 4997])


class ExportTests(unittest.TestCase):
    def test_a_view_blocks_resizes_and_sees_writes(self):
        a = array.array("i", [1, 2, 3])
        view = memoryview(a)
        with self.assertRaises(BufferError):
            a.append(4)
        with self.assertRaises(BufferError):
            a[0:1] = array.array("i", [7, 8])
        with self.assertRaises(BufferError):
            del a[0]
        a[0] = 42
        a[1:3] = array.array("i", [5, 6])
        self.assertEqual(view.tolist(), [42, 5, 6])
        view[2] = -1
        self.assertEqual(a[2], -1)
        view.release()
        a.append(4)
        self.assertEqual(a.tolist(), [42, 5, -1, 4])

    def test_bytes_round_trip(self):
        a = array.array("H", [1, 2, 0xFFFF])
        raw = a.tobytes()
        self.assertEqual(len(raw), 6)
        b = array.array("H")
        b.frombytes(raw)
        self.assertEqual(a, b)
        self.assertEqual(bytes(a), raw)
        self.assertEqual(bytes(memoryview(a)), raw)


class CostTests(unittest.TestCase):
    """An item costs the same in a large array as in a small one."""

    @staticmethod
    def best(fn, repeat=5):
        best = float("inf")
        for _ in range(repeat):
            start = time.perf_counter()
            fn()
            best = min(best, time.perf_counter() - start)
        return best

    def check(self, work):
        small = array.array("i", range(1_000))
        large = array.array("i", range(1_000_000))
        t_small = self.best(lambda: work(small))
        t_large = self.best(lambda: work(large))
        # (A copy of the buffer per item made the large array a thousand
        # times the small one's cost.)
        self.assertLess(t_large, t_small * 25 + 0.01)

    def test_reads(self):
        def work(a):
            total = 0
            for i in range(600):
                total += a[i] + a[-1 - i]
            return total

        self.check(work)

    def test_writes(self):
        def work(a):
            for i in range(600):
                a[i] = i
                a[-1 - i] = -i

        self.check(work)

    def test_slices(self):
        def work(a):
            total = 0
            for i in range(300):
                total += len(a[i:i + 8])
                a[i:i + 2] = a[i + 2:i + 4]
            return total

        self.check(work)

    def test_appends_and_iteration_prefix(self):
        def work(a):
            for i in range(300):
                a.append(i)
            it = iter(a)
            total = 0
            for _ in range(300):
                total += next(it)
            del a[len(a) - 300:]
            return total

        self.check(work)


if __name__ == "__main__":
    unittest.main()
