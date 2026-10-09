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

class CallShapesTest(unittest.TestCase):
    def test_lru_cache_and_partial_in_loops(self):
        import functools

        @functools.lru_cache(maxsize=4)
        def sq(n):
            return n * n

        double = functools.partial(lambda a, b: a * b, 2)
        kw = functools.partial(lambda a, b=0: a - b, b=1)
        total = 0
        for i in range(50):
            for f in (sq, double, kw):
                total += f(i % 6)
        self.assertEqual(total, sum((i % 6) ** 2 + 2 * (i % 6) + (i % 6) - 1 for i in range(50)))
        info = sq.cache_info()
        self.assertEqual((info.hits + info.misses, info.currsize), (50, 4))

    def test_lru_cache_reentrant_and_raising(self):
        import functools

        @functools.lru_cache(maxsize=None)
        def fib(n):
            return n if n < 2 else fib(n - 1) + fib(n - 2)

        @functools.lru_cache(maxsize=8)
        def boom(n):
            raise ValueError(n)

        for _ in range(3):
            self.assertEqual(fib(60), 1548008755920)
            with self.assertRaises(ValueError):
                boom(1)

    def test_function_attributes(self):
        def f():
            pass

        f.calls = 0
        for _ in range(100):
            f.calls += 1
        self.assertEqual(f.calls, 100)
        self.assertEqual(f.__dict__, {"calls": 100})
        d = {}
        f.__dict__ = d
        for i in range(3):
            f.x = i
        self.assertEqual(d, {"x": 2})
        with self.assertRaises(AttributeError):
            for _ in range(3):
                f.missing
        for _ in range(3):
            self.assertEqual(f.__name__, "f")

    def test_builtin_keyword_calls(self):
        for _ in range(3):
            self.assertEqual(sorted((3, 1, 2), key=lambda v: -v), [3, 2, 1])
            self.assertEqual(sorted([1, 2, 3], reverse=True), [3, 2, 1])
            self.assertEqual(max([1, 5, 2], key=lambda v: -v), 1)
            self.assertEqual(min([], default=7), 7)
            self.assertEqual(round(2.567, ndigits=2), 2.57)
            self.assertEqual(sum([1, 2], start=10), 13)
            with self.assertRaises(TypeError):
                sorted([1], bogus=1)
            with self.assertRaises(ZeroDivisionError):
                sorted([1, 2], key=lambda v: 1 // 0)

class DisplayAndComprehensionTest(unittest.TestCase):
    def test_star_displays_and_calls(self):
        a, b = [1, 2], (3,)
        for _ in range(3):
            self.assertEqual([*a, *b], [1, 2, 3])
            self.assertEqual((*a, *b), (1, 2, 3))
            self.assertEqual(max(*a, *b), 3)
            self.assertEqual([*a, *a], [1, 2, 1, 2])
            self.assertEqual([*"ab", *range(2)], ["a", "b", 0, 1])
        with self.assertRaises(TypeError):
            [*a, *5]

    def test_set_and_dict_comprehensions(self):
        for _ in range(3):
            self.assertEqual({x % 3 for x in range(9)}, {0, 1, 2})
            self.assertEqual({str(x): x for x in range(3)}, {"0": 0, "1": 1, "2": 2})
            d = {k: v for k, v in [(1, "a"), (1.0, "b"), (True, "c")]}
            self.assertEqual(list(d.items()), [(1, "c")])
            self.assertIs(type(next(iter(d))), int)
            s = {x for x in [1, 1.0, True]}
            self.assertIs(type(next(iter(s))), int)

    def test_comprehension_key_with_python_eq(self):
        class K:
            def __init__(self, v):
                self.v = v

            def __eq__(self, other):
                return isinstance(other, K) and other.v == self.v

            def __hash__(self):
                return hash(self.v)

        for _ in range(3):
            self.assertEqual(len({K(1) for _ in range(3)}), 1)
            self.assertEqual(len({K(1): 0 for _ in range(3)}), 1)

    def test_starred_unpack(self):
        for src in ([1, 2, 3, 4], (1, 2, 3, 4)):
            for _ in range(3):
                a, *b, c = src
                self.assertEqual((a, b, c), (1, [2, 3], 4))
                *d, = src
                self.assertEqual(d, [1, 2, 3, 4])
        with self.assertRaises(ValueError):
            a, b, *c, d, e = [1, 2, 3]

class ContextManagerShapesTest(unittest.TestCase):
    def test_contextmanager_and_suppress(self):
        import contextlib

        @contextlib.contextmanager
        def cm(v, *, extra=0):
            yield v + extra

        total = 0
        for i in range(50):
            with cm(i, extra=1) as v:
                total += v
            with contextlib.suppress(KeyError):
                raise KeyError(i)
        self.assertEqual(total, sum(range(1, 51)))

    def test_delete_attributes(self):
        class C:
            pass

        class D:
            def __delattr__(self, name):
                self.log.append(name)

        class Q:
            @property
            def x(self):
                return 1

            @x.deleter
            def x(self):
                Q.deleted += 1

        Q.deleted = 0
        for _ in range(5):
            c = C()
            c.a, c.b = 1, 2
            del c.a
            self.assertEqual(vars(c), {"b": 2})
            with self.assertRaises(AttributeError):
                del c.a
            d = D()
            d.__dict__["log"] = []
            del d.x
            self.assertEqual(d.log, ["x"])
            del Q().x
        self.assertEqual(Q.deleted, 5)

    def test_abc_instantiation(self):
        import abc

        class Base(abc.ABC):
            @abc.abstractmethod
            def f(self):
                pass

        class Impl(Base):
            def __init__(self, v):
                self.v = v

            def f(self):
                return self.v

        for i in range(5):
            self.assertEqual(Impl(i).f(), i)
            with self.assertRaises(TypeError):
                Base()

        class Meta(type):
            def __call__(cls, *args):
                return ("meta", args)

        class M(metaclass=Meta):
            pass

        for _ in range(3):
            self.assertEqual(M(1), ("meta", (1,)))

    def test_generator_forwarding_calls(self):
        def gen(a, b=0):
            yield a + b

        def forward(f, args, kwds):
            return f(*args, **kwds)

        for _ in range(5):
            self.assertEqual(list(forward(gen, (1,), {})), [1])
            self.assertEqual(list(forward(gen, (1,), {"b": 2})), [3])
            self.assertEqual(list(gen(*[4])), [4])

class SliceStoreAndGcTest(unittest.TestCase):
    def test_slice_assignment(self):
        import random

        rng = random.Random(3)
        for _ in range(500):
            xs = list(range(rng.randint(0, 6)))
            ref = list(xs)
            a = rng.choice([None, -9, -2, -1, 0, 1, 2, 5, 9])
            b = rng.choice([None, -9, -2, -1, 0, 1, 2, 5, 9])
            v = rng.choice([[], [7], (8, 9), [10, 11, 12]])
            xs[a:b] = v
            ref[slice(a, b)] = list(v)
            self.assertEqual(xs, ref)
        ys = [1, 2, 3]
        ys[1:] = ys
        self.assertEqual(ys, [1, 1, 2, 3])
        with self.assertRaises(TypeError):
            xs = [1, 2]
            xs[0:1] = 5

    def test_dead_registered_instances(self):
        import gc
        import weakref

        class Node:
            pass

        keep = []
        for _ in range(3):
            batch = [Node() for _ in range(2000)]
            gc.collect()
            keep.append(weakref.ref(batch[0]))
            del batch
        self.assertEqual([r() for r in keep], [None, None, None])
        gc.collect()

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
