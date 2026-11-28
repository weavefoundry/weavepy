"""Compiled augmented assignments call a class's Python in-place method
(`__iadd__`, ...) directly; the results (`self` or a new object), a
`NotImplemented` that goes on to `__add__` / `__radd__` or to the
augmented operator's TypeError, raises, and methods replaced later match
the general operator."""

import unittest


class Acc:
    def __init__(self):
        self.total = 0
        self.calls = 0

    def __iadd__(self, o):
        self.calls += 1
        self.total += o
        return self


class Fresh:
    def __init__(self, v):
        self.v = v

    def __imul__(self, o):
        return Fresh(self.v * o)


class Picky:
    def __init__(self, v):
        self.v = v

    def __iadd__(self, o):
        if not isinstance(o, int):
            return NotImplemented
        self.v += o
        return self

    def __add__(self, o):
        if isinstance(o, str):
            return Picky(self.v + len(o))
        return NotImplemented


class Right:
    def __radd__(self, o):
        return "radd"


class OnlyI:
    def __iadd__(self, o):
        return NotImplemented


class Raiser:
    def __iadd__(self, o):
        raise KeyError(o)


class InplaceDunderDirect(unittest.TestCase):
    def test_self_result(self):
        a = Acc()
        x = a
        for i in range(3000):
            x += i
        self.assertIs(x, a)
        self.assertEqual((a.total, a.calls), (sum(range(3000)), 3000))

    def test_new_result(self):
        x = Fresh(1)
        for i in range(60):
            x *= 2
        self.assertEqual(x.v, 2**60)
        for _ in range(1000):
            y = Fresh(3)
            y *= 2
            self.assertEqual(y.v, 6)

    def test_not_implemented_fallbacks(self):
        for i in range(1000):
            p = Picky(1)
            q = p
            q += 2
            self.assertIs(q, p)
            self.assertEqual(p.v, 3)
            q += "abcd"
            self.assertIsNot(q, p)
            self.assertEqual(q.v, 7)
            r = Picky(0)
            r += Right()
            self.assertEqual(r, "radd")
            with self.assertRaises(TypeError) as cm:
                s = Picky(0)
                s += 1.5
            self.assertEqual(
                str(cm.exception),
                "unsupported operand type(s) for +=: 'Picky' and 'float'",
            )
            with self.assertRaises(TypeError):
                o = OnlyI()
                o += 1

    def test_raises(self):
        r = Raiser()
        n = 0
        for i in range(1000):
            x = r
            try:
                x += i
            except KeyError:
                n += 1
        self.assertEqual(n, 1000)

        def bump(o):
            o += 1
            return o

        for i in range(1000):
            with self.assertRaises(KeyError):
                bump(r)
            self.assertIsInstance(bump(Acc()), Acc)

    def test_attribute_and_item_targets(self):
        class Box:
            pass

        b = Box()
        b.acc = Acc()
        d = {"k": Acc()}
        for i in range(1000):
            b.acc += 1
            d["k"] += 2
        self.assertEqual(b.acc.total, 1000)
        self.assertEqual(d["k"].total, 2000)

    def test_replaced(self):
        class C:
            def __iadd__(self, o):
                return 1

        n = 0
        for i in range(1000):
            x = C()
            x += 1
            n += x
        C.__iadd__ = lambda self, o: 10
        for i in range(1000):
            x = C()
            x += 1
            n += x
        del C.__iadd__
        C.__add__ = lambda self, o: 100
        for i in range(1000):
            x = C()
            x += 1
            n += x
        self.assertEqual(n, 111000)

    def test_generator_caller(self):
        a = Acc()

        def g():
            x = a
            for i in range(500):
                x += 1
                yield i

        self.assertEqual(sum(g()), sum(range(500)))
        self.assertEqual(a.total, 500)


if __name__ == "__main__":
    unittest.main()
