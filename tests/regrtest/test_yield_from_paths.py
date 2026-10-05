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


if __name__ == "__main__":
    unittest.main()
