"""Building containers from instances and iterators in place.

`list(obj)` of an instance whose `__iter__` and `__len__` evaluate as pure
leaves skips the full protocol, `iter()` runs inside frameless
evaluations, a list or tuple iterator drains at once, and an `__iter__`
returning a non-iterator raises CPython's `TypeError` on every path.
"""

import unittest

N = 3000


class Seq:
    def __init__(self):
        self.d = [1, 2, 3]

    def __len__(self):
        return len(self.d)

    def __iter__(self):
        return iter(self.d)


class BadLen:
    def __len__(self):
        raise RuntimeError("len")

    def __iter__(self):
        return iter([1])


class TypeErrorLen:
    def __len__(self):
        raise TypeError("t")

    def __iter__(self):
        return iter([2])


class Hint:
    def __init__(self):
        self.calls = 0

    def __length_hint__(self):
        self.calls += 1
        return 1

    def __iter__(self):
        return iter([3])


class NotIter:
    def __iter__(self):
        return [1]


class GenIter:
    def __iter__(self):
        yield 5


class DictIter:
    def __init__(self):
        self.m = {"a": 1}

    def __iter__(self):
        return iter(self.m)


class IterProtocolTests(unittest.TestCase):
    def test_collect(self):
        for i in range(N):
            r = (list(Seq()), tuple(Seq()), list(TypeErrorLen()), list(GenIter()),
                 list(DictIter()), set(Seq()))
        self.assertEqual(r, ([1, 2, 3], (1, 2, 3), [2], [5], ["a"], {1, 2, 3}))
        h = Hint()
        self.assertEqual(list(h), [3])
        self.assertEqual(h.calls, 1)

    def test_errors(self):
        for i in range(N // 10):
            with self.assertRaises(RuntimeError):
                list(BadLen())
            with self.assertRaises(TypeError) as cm:
                list(NotIter())
        self.assertEqual(str(cm.exception), "iter() returned non-iterator of type 'list'")

        def loop():
            for i in range(N):
                try:
                    for x in NotIter():
                        pass
                except TypeError as e:
                    msg = str(e)
            return msg

        self.assertEqual(loop(), "iter() returned non-iterator of type 'list'")

    def test_partial_iterators(self):
        for i in range(N):
            xs = [1, 2, 3]
            it = iter(xs)
            next(it)
            first = list(it)
            xs.append(4)
            r = (first, list(it), next(it, "end"))
            t = iter((1, 2))
            r2 = (tuple(t), list(t))
        self.assertEqual(r, ([2, 3], [], "end"))
        self.assertEqual(r2, ((1, 2), []))


if __name__ == "__main__":
    unittest.main()
