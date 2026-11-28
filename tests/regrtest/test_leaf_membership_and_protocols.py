"""Membership, truth and attribute fallbacks evaluated in place.

Frameless evaluation now runs `x in container` (and `not in`) over native
containers, `bool(obj)` takes the truth test's path, and a pure
`__getattr__` answers a missing attribute without an activation. These
loops check results against containers whose members compare in Python,
recursive `__contains__`, error cases and the `__bool__ = None` block.
"""

import unittest

N = 3000


class Loose:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return getattr(other, "v", other) == self.v

    def __hash__(self):
        return hash(self.v)


class Box:
    def __init__(self, items):
        self.items = items

    def __contains__(self, x):
        return x in self.items


class NotBox:
    def __init__(self, items):
        self.items = items

    def has_not(self, x):
        return x not in self.items


class Nested:
    def __init__(self, inner):
        self.inner = inner

    def __contains__(self, x):
        return x in self.inner


def key_in(x, allowed=frozenset({1, 3})):
    return x in allowed


class MembershipTests(unittest.TestCase):
    def test_native_containers(self):
        boxes = [Box([1, 2, 3]), Box((1, 2)), Box({1: 0}), Box({2, 3}),
                 Box(frozenset({1}))]
        text = Box("abc")
        for i in range(N):
            r = [(2 in b, "b" in b, 1 in b) for b in boxes]
            r.append(("b" in text, "z" in text))
        self.assertEqual(r, [(True, False, True), (True, False, True),
                             (False, False, True), (True, False, False),
                             (False, False, True), (True, False)])
        nb = NotBox([1, 2])
        for i in range(N):
            r2 = (nb.has_not(1), nb.has_not(5))
        self.assertEqual(r2, (False, True))

    def test_python_equality_and_nesting(self):
        b = Box([Loose(1), Loose(2)])
        nested = Nested(Box([7]))
        for i in range(N):
            r = (1 in b, 5 in b, Loose(2) in b, 7 in nested, 8 in nested)
        self.assertEqual(r, (True, False, True, True, False))

    def test_errors(self):
        b = Box(5)
        for i in range(N):
            try:
                1 in b
            except TypeError as e:
                msg = str(e)
        self.assertEqual(msg, "argument of type 'int' is not a container or iterable")
        s = Box("abc")
        with self.assertRaises(TypeError):
            for i in range(N):
                1 in s

    def test_sorted_with_membership_key(self):
        for i in range(N):
            r = sorted([3, 2, 1, 4], key=key_in)
        self.assertEqual(r, [2, 4, 3, 1])


class TruthTests(unittest.TestCase):
    def test_bool_call(self):
        class Empty:
            def __len__(self):
                return 0

        class Both:
            def __bool__(self):
                return False

            def __len__(self):
                return 5

        class Plain:
            pass

        for i in range(N):
            r = (bool(Empty()), bool(Both()), bool(Plain()), bool([0]))
        self.assertEqual(r, (False, False, True, True))

    def test_bool_errors(self):
        class Neg:
            def __len__(self):
                return -1

        class Int:
            def __bool__(self):
                return 1

        class Blocked:
            __bool__ = None

        for cls, exc, msg in ((Neg, ValueError, "__len__() should return >= 0"),
                              (Int, TypeError, "__bool__ should return bool, returned int"),
                              (Blocked, TypeError, "'Blocked' cannot be interpreted as a boolean")):
            for i in range(N // 10):
                with self.assertRaises(exc) as cm:
                    bool(cls())
            self.assertEqual(str(cm.exception), msg)
        with self.assertRaises(TypeError):
            if Blocked():
                pass


class GetattrFallbackTests(unittest.TestCase):
    def test_fallback(self):
        class GA:
            def __init__(self):
                self.real = 1

            def __getattr__(self, name):
                return name

        class GE:
            def __getattr__(self, name):
                if name.startswith("x"):
                    return 1
                raise AttributeError(name)

        g, ge = GA(), GE()
        for i in range(N):
            r = (g.real, g.missing, ge.xa, getattr(ge, "nope", 5), hasattr(ge, "nope"))
        self.assertEqual(r, (1, "missing", 1, 5, False))
        with self.assertRaises(AttributeError):
            ge.nope


if __name__ == "__main__":
    unittest.main()
