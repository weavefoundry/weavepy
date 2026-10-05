"""`yield from` over instances whose `__iter__` is a generator function,
and delegation chains whose generators yield from deep recursion."""

import sys
import unittest


class Tree:
    __slots__ = ("left", "value", "right")

    def __init__(self, left, value, right):
        self.left = left
        self.value = value
        self.right = right

    def __iter__(self):
        if self.left:
            yield from self.left
        yield self.value
        if self.right:
            yield from self.right


def build(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(build(lo, mid), mid, build(mid + 1, hi))


class YieldFromPathsTest(unittest.TestCase):
    def test_tree_walk(self):
        tree = build(0, 200)
        self.assertEqual(list(tree), list(range(200)))
        self.assertEqual(sum(tree), sum(range(200)))
        self.assertEqual(max(x for x in tree if x % 7 == 3), 199 - (199 - 3) % 7)

    def test_generator_names_follow_function(self):
        class C:
            def __iter__(self):
                yield 1

        g = iter(C())
        self.assertEqual(g.__name__, "__iter__")
        self.assertEqual(g.__qualname__, C.__iter__.__qualname__)
        C.__iter__.__name__ = "renamed"
        C.__iter__.__qualname__ = "Q.renamed"

        def outer():
            it = yield from C()
            return it

        inner = []

        def spy():
            gen = iter(C())
            inner.append((gen.__name__, gen.__qualname__))
            yield from gen

        self.assertEqual(list(outer()), [1])
        self.assertEqual(list(spy()), [1])
        self.assertEqual(inner, [("renamed", "Q.renamed")])

    def test_iter_with_default_argument(self):
        class C:
            def __iter__(self, n=3):
                yield from range(n)

        def outer():
            yield from C()

        self.assertEqual(list(outer()), [0, 1, 2])

    def test_iter_closure(self):
        k = 10

        class C:
            def __iter__(self):
                yield k
                yield k + 1

        def outer():
            yield from C()

        self.assertEqual(list(outer()), [10, 11])

    def test_iter_replaced_on_class(self):
        class C:
            def __iter__(self):
                yield "a"

        def outer(obj):
            yield from obj

        obj = C()
        self.assertEqual(list(outer(obj)), ["a"])
        C.__iter__ = lambda self: iter(["b", "c"])
        self.assertEqual(list(outer(obj)), ["b", "c"])
        C.__iter__ = None
        with self.assertRaises(TypeError):
            list(outer(obj))

    def test_iter_non_iterator_result(self):
        class C:
            def __iter__(self):
                return 5

        def outer():
            yield from C()

        with self.assertRaises(TypeError):
            list(outer())

    def test_delegate_released_after_end_send(self):
        log = []

        class Res:
            def __iter__(self):
                yield 1

            def __del__(self):
                log.append("del")

        def outer():
            yield from Res()
            log.append("after")
            yield 2

        self.assertEqual(list(outer()), [1, 2])
        self.assertEqual(log, ["del", "after"])

    def test_handled_exception_across_yield(self):
        class C:
            def __iter__(self):
                try:
                    raise KeyError("k")
                except KeyError:
                    yield sys.exception()
                yield sys.exception()

        def outer():
            yield from C()

        out = list(outer())
        self.assertIsInstance(out[0], KeyError)
        self.assertIsNone(out[1])

    def test_frame_of_suspended_delegate(self):
        class C:
            def __iter__(self):
                yield sys._getframe()
                yield sys._getframe(1).f_code.co_name

        def outer():
            yield from C()

        g = outer()
        f = next(g)
        self.assertEqual(f.f_code.co_name, "__iter__")
        self.assertEqual(next(g), "outer")


    def test_chain_passes_values_and_results(self):
        def leaf(n):
            for i in range(n):
                yield i
            return n

        def mid(d, n):
            if d == 0:
                r = yield from leaf(n)
            else:
                r = yield from mid(d - 1, n)
            return r + 1

        for _ in range(50):
            self.assertEqual(list(mid(12, 5)), [0, 1, 2, 3, 4])
            g = mid(12, 3)
            self.assertEqual(sum(g), 3)
            with self.assertRaises(StopIteration):
                next(g)

        def outer():
            got = yield from mid(6, 2)
            yield got

        self.assertEqual(list(outer()), [0, 1, 9])

    def test_chain_running_flags(self):
        seen = []

        def inner():
            seen.append((top.gi_running, middle.gi_running))
            yield 1
            seen.append((top.gi_running, middle.gi_running))

        def mid():
            yield from inner()

        def outer():
            yield from middle

        middle = mid()
        top = outer()
        self.assertEqual(list(top), [1])
        self.assertEqual(seen, [(True, True), (True, True)])
        self.assertFalse(top.gi_running)

    def test_chain_inner_raise_and_throw(self):
        def inner():
            yield 1
            raise ValueError("inner")

        def mid():
            try:
                yield from inner()
            except ValueError as e:
                yield str(e)

        def outer():
            yield from mid()

        self.assertEqual(list(outer()), [1, "inner"])

        def catcher():
            try:
                yield 1
            except KeyError:
                yield "caught"

        def chain():
            yield from catcher()

        g = chain()
        self.assertEqual(next(g), 1)
        self.assertEqual(g.throw(KeyError), "caught")
        g.close()

    def test_chain_frames(self):
        def inner():
            f = sys._getframe()
            yield [f.f_code.co_name, f.f_back.f_code.co_name, f.f_back.f_back.f_code.co_name]

        def mid():
            yield from inner()

        def outer():
            yield from mid()

        self.assertEqual(next(outer()), ["inner", "mid", "outer"])

    def test_deep_chain_hits_recursion_limit(self):
        def chain(d):
            if d == 0:
                yield 1
            else:
                yield from chain(d - 1)

        old = sys.getrecursionlimit()
        sys.setrecursionlimit(200)
        try:
            with self.assertRaises(RecursionError):
                list(chain(400))
            self.assertEqual(list(chain(50)), [1])
        finally:
            sys.setrecursionlimit(old)

    def test_for_loop_over_instance_generator(self):
        total = 0
        for _ in range(300):
            for x in Tree(None, 2, Tree(None, 3, None)):
                total += x
        self.assertEqual(total, 1500)

    def test_exhausted_generator_releases_locals_promptly(self):
        log = []

        class D:
            def __del__(self):
                log.append("del")

        def g():
            x = D()
            yield 1

        for _ in range(500):
            for _v in g():
                pass
            log.append("after")
            self.assertEqual(log[-2:], ["del", "after"])

    def test_renamed_generator_function(self):
        def gen():
            yield 1

        self.assertEqual(gen().__name__, "gen")
        gen.__name__ = "renamed"
        gen.__qualname__ = "Q.renamed"
        g = gen()
        self.assertEqual((g.__name__, g.__qualname__), ("renamed", "Q.renamed"))
        self.assertIs(gen().__name__, gen().__name__)


if __name__ == "__main__":
    unittest.main()
