"""Shapes the interpreter's core loop serves in place: non-ASCII string
indexing and slicing, sequence concatenation, empty constructors, tuple
loops, and generator calls."""

import gc
import random
import sys
import unittest


class StrIndexTest(unittest.TestCase):
    def test_non_ascii_index_and_slice(self):
        rng = random.Random(5)
        alpha = "aé€😀b́"
        for _ in range(100):
            s = "".join(rng.choice(alpha) for _ in range(rng.randint(0, 30)))
            ref = list(s)
            n = len(s)
            for _ in range(40):
                i = rng.randint(-n - 2, n + 2)
                j = rng.randint(-n - 2, n + 2)
                if -n <= i < n:
                    self.assertEqual(s[i], ref[i])
                else:
                    with self.assertRaises(IndexError):
                        s[i]
                self.assertEqual(s[i:j], "".join(ref[i:j]))
                self.assertEqual(s[max(i, 0):], "".join(ref[max(i, 0):]))

    def test_scan_two_strings_alternately(self):
        a = "ünïcode " * 20
        b = "日本語テキスト" * 20
        out = []
        for i in range(len(b)):
            out.append(a[i % len(a)] + b[i])
        self.assertEqual("".join(out), "".join(x + y for x, y in zip((a * 2)[: len(b)], b)))

    def test_string_dies_while_cursor_names_it(self):
        def make(k):
            return "é" * 50 + str(k)

        for k in range(20):
            s = make(k)
            self.assertEqual(s[50], str(k)[0])
            del s


class SequenceTest(unittest.TestCase):
    def test_concat(self):
        t, l = (1, 2), [3]
        for _ in range(3):
            self.assertEqual(t + (3,), (1, 2, 3))
            self.assertIs(t + (), t)
            self.assertIs(() + t, t)
            self.assertEqual(l + [4], [3, 4])
        self.assertEqual(l, [3])

    def test_inplace_add_keeps_identity(self):
        u = [0, 1]
        u2 = u
        for i in range(3):
            u += [i]
        self.assertIs(u, u2)
        self.assertEqual(u, [0, 1, 0, 1, 2])
        t = (1,)
        t2 = t
        for i in range(2):
            t += (i,)
        self.assertIsNot(t, t2)

    def test_concat_subclasses(self):
        class T(tuple):
            def __add__(self, other):
                return "T+"

        class L(list):
            def __add__(self, other):
                return "L+"

        for _ in range(3):
            self.assertEqual(T((1,)) + (2,), "T+")
            self.assertEqual(L([1]) + [2], "L+")

    def test_empty_constructors(self):
        for _ in range(3):
            s, d, l, t = set(), dict(), list(), tuple()
            self.assertEqual((s, d, l, t), (set(), {}, [], ()))
            s.add(1)
            d[1] = 2
            self.assertIsNot(set(), s)
            self.assertIs(tuple(), ())

    def test_tuple_loop_releases_items(self):
        log = []

        class Item:
            def __del__(self):
                log.append("del")

        def run():
            total = 0
            for x in (Item(), Item()):
                total += 1
            log.append("after")
            return total

        self.assertEqual(run(), 2)
        gc.collect()
        # (`x` holds the last item until the function returns.)
        self.assertEqual(log, ["del", "after", "del"])


class GeneratorCallTest(unittest.TestCase):
    def test_shapes(self):
        def plain(a, b):
            yield a
            yield b

        def defaults(a, b=10, c=20):
            yield a + b + c

        k = 5

        def closure(a):
            yield a + k

        def cell(a):
            def inner():
                return a
            yield inner()

        for _ in range(3):
            self.assertEqual(list(plain(1, 2)), [1, 2])
            self.assertEqual(list(defaults(1)), [31])
            self.assertEqual(list(defaults(1, 2)), [23])
            self.assertEqual(list(closure(1)), [6])
            self.assertEqual(list(cell(7)), [7])
            g = plain(1, 2)
            self.assertEqual(g.__name__, "plain")
            self.assertIsNone(g.gi_frame.f_back)

    def test_errors_and_replaced_defaults(self):
        def gen(a, b=1):
            yield a, b

        for _ in range(2):
            with self.assertRaises(TypeError):
                gen()
            with self.assertRaises(TypeError):
                gen(1, 2, 3)
        gen.__defaults__ = (7,)
        self.assertEqual(list(gen(1)), [(1, 7)])

    def test_methods_and_genexp(self):
        class C:
            def items(self, n):
                yield from range(n)

        c = C()
        data = [1, 2, 3]
        for _ in range(3):
            self.assertEqual(list(c.items(3)), [0, 1, 2])
            self.assertEqual(sum(x * 2 for x in data if x != 2), 8)

    def test_coroutine_origin_tracking(self):
        async def coro():
            return 1

        sys.set_coroutine_origin_tracking_depth(2)
        try:
            c = coro()
            self.assertIsNotNone(c.cr_origin)
            c.close()
        finally:
            sys.set_coroutine_origin_tracking_depth(0)
        c = coro()
        self.assertIsNone(c.cr_origin)
        c.close()


class IterationTest(unittest.TestCase):
    def test_set_iteration(self):
        s = {1, 2, 3}
        for _ in range(3):
            self.assertEqual(sorted(x for x in s), [1, 2, 3])
            self.assertEqual(sum(1 for _ in frozenset(s)), 3)
        with self.assertRaises(RuntimeError):
            for x in s:
                s.add(x + 10)

    def test_shared_list_iterator_stays_exhausted(self):
        data = [1, 2]
        it = iter(data)
        out = [x for x in it]
        data.append(3)
        self.assertEqual(out, [1, 2])
        self.assertEqual(list(it), [])

    def test_shared_exhausted_iterators(self):
        for src in ((1, 2), "ab", b"ab", range(2)):
            it = iter(src)
            self.assertEqual(len([x for x in it]), 2)
            self.assertEqual(list(it), [])

    def test_genexpr_call_shape(self):
        def frames():
            return list(sys._getframe(1).f_code.co_name for _ in range(1))

        self.assertEqual(frames(), ["frames"])

        def raising(xs):
            return list(1 // x for x in xs)

        with self.assertRaises(ZeroDivisionError):
            raising([1, 0])

    def test_comprehension_save_restore_with_cells(self):
        def f():
            x = "outer"
            fns = [lambda: x for _ in range(2)]
            vals = [x for x in range(3)]
            return x, vals, [g() for g in fns]

        for _ in range(3):
            self.assertEqual(f(), ("outer", [0, 1, 2], ["outer", "outer"]))

    def test_common_constants(self):
        all = lambda xs: "shadowed"  # noqa: E731
        for _ in range(3):
            self.assertIs(any(x > 1 for x in (1, 2)), True)
            self.assertEqual(all([1]), "shadowed")
            with self.assertRaises(AssertionError) as cm:
                assert False, "msg"
            self.assertEqual(str(cm.exception), "msg")

    def test_decorators(self):
        def deco(f):
            return lambda: ("decorated", f())

        @deco
        def g():
            return 1

        @(lambda c: c)
        class K:
            pass

        self.assertEqual(g(), ("decorated", 1))
        self.assertEqual(K.__name__, "K")

class FormatTest(unittest.TestCase):
    def test_huge_field_index(self):
        for _ in range(3):
            with self.assertRaises(ValueError):
                "{9223372036854775808}".format(1)
            with self.assertRaises(ValueError):
                "{99999999999999999999999}".format(1)
            with self.assertRaises(IndexError):
                "{5}".format(1)


if __name__ == "__main__":
    unittest.main()
