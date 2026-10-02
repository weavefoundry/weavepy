"""Compiled code calls keyword-taking pure functions without a frame.

A keyword call of a small pure function binds its values onto the
function's parameters (a fresh `**kwargs` dictionary included) and
evaluates it frameless. Each loop runs long enough for the tier-2 compiler
to take it; then the callee changes, and every call must see the change.
"""

import unittest


def with_kwargs(a, **kw):
    return a + kw.get("delta", 0)


def mixed(a, b=1, **kw):
    return a * b + kw.get("c", 0)


def give_back(a, **kw):
    return kw


class Holder:
    def scaled(self, a, **kw):
        return a * kw.get("k", 1)


def kw_loop(n):
    t = 0
    for i in range(n):
        t += with_kwargs(i, delta=2)
    return t


def mixed_loop(n):
    t = 0
    for i in range(n):
        t += mixed(i, c=3, b=2)
    return t


def fresh_dicts(n):
    seen = []
    for i in range(n):
        seen.append(give_back(i, x=i))
    return seen


def bound_loop(n):
    h = Holder()
    scaled = h.scaled
    t = 0
    for i in range(n):
        t += scaled(i, k=3)
    return t


def triangle(n):
    # The sum of 0, 1, ..., n - 1.
    return n * (n - 1) // 2


class KwLeafCallTests(unittest.TestCase):
    N = 3000

    def setUp(self):
        self.saved = (with_kwargs.__code__, mixed.__defaults__)

    def tearDown(self):
        with_kwargs.__code__, mixed.__defaults__ = self.saved

    def test_calls(self):
        n = self.N
        self.assertEqual(kw_loop(n), triangle(n) + 2 * n)
        self.assertEqual(mixed_loop(n), 2 * triangle(n) + 3 * n)
        self.assertEqual(bound_loop(n), 3 * triangle(n))

    def test_each_call_gets_a_fresh_dict(self):
        dicts = fresh_dicts(self.N)
        self.assertEqual(len({id(d) for d in dicts}), self.N)
        self.assertEqual(dicts[7], {"x": 7})
        dicts[7]["y"] = 1
        self.assertEqual(dicts[8], {"x": 8})

    def test_code_reassigned(self):
        def other(a, **kw):
            return len(kw) * 100

        n = self.N
        self.assertEqual(kw_loop(n), triangle(n) + 2 * n)
        with_kwargs.__code__ = other.__code__
        self.assertEqual(kw_loop(5), 500)

    def test_defaults_reassigned(self):
        n = self.N
        self.assertEqual(mixed_loop(n), 2 * triangle(n) + 3 * n)
        mixed.__defaults__ = (10,)
        self.assertEqual(mixed_loop(5), 2 * triangle(5) + 15)

        def call_without_b(n):
            t = 0
            for i in range(n):
                t += mixed(i, c=1)
            return t

        self.assertEqual(call_without_b(n), 10 * triangle(n) + n)
        mixed.__defaults__ = None
        with self.assertRaises(TypeError):
            call_without_b(1)

    def test_bad_keyword(self):
        def call_bad(n):
            t = 0
            for i in range(n):
                t += mixed(i, b=2, c=1)
            return t

        self.assertEqual(call_bad(self.N), 2 * triangle(self.N) + self.N)
        with self.assertRaises(TypeError):
            mixed(1, a=2)


if __name__ == "__main__":
    unittest.main()
