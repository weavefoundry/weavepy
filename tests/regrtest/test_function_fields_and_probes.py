"""Reads of a function's fields and probes for names it lacks.

`f.__defaults__`, `f.__kwdefaults__`, `f.__code__`, `f.__globals__` and
`f.__builtins__` answer in place as the full read does, an entry a function
stores winning. `getattr(f, name, default)` and `hasattr(f, name)` for a
name the function type doesn't define either (`__signature__`,
`__wrapped__`, as `inspect` asks) answer from a miss remembered on the
type, while each function's own entries still win. A generator's public
names it lacks (`hasattr(gen, "keys")`) are answered likewise.
"""

import builtins
import functools
import inspect
import unittest

N = 3000


def f(a, b=1, *args, c, d=2, **kw):
    pass


def plain(x):
    pass


class FunctionAttrReadTests(unittest.TestCase):
    def test_defaults_and_code(self):
        for _ in range(N):
            r = (f.__defaults__, f.__kwdefaults__, f.__code__, plain.__defaults__,
                 plain.__kwdefaults__, f.__globals__, f.__builtins__)
        self.assertEqual(r[0], (1,))
        self.assertEqual(r[1], {"d": 2})
        self.assertIs(r[2], f.__code__)
        self.assertEqual(r[2].co_name, "f")
        self.assertEqual((r[3], r[4]), (None, None))
        self.assertIs(r[5], globals())
        self.assertIs(r[6], builtins.__dict__)

    def test_assigned_defaults(self):
        def g(a=1):
            return a

        out = []
        for i in range(N):
            if i == N // 2:
                g.__defaults__ = (5,)
            out.append((g.__defaults__, g()))
        self.assertEqual(out[0], ((1,), 1))
        self.assertEqual(out[-1], ((5,), 5))

    def test_missing_names_with_defaults(self):
        def g():
            pass

        seen = []
        for i in range(N):
            if i == N // 3:
                g.__signature__ = "sig"
            if i == 2 * N // 3:
                del g.__signature__
            seen.append((getattr(g, "__signature__", None), hasattr(g, "__wrapped__")))
        self.assertEqual(seen[0], (None, False))
        self.assertEqual(seen[N // 2], ("sig", False))
        self.assertEqual(seen[-1], (None, False))
        with self.assertRaises(AttributeError):
            g.__signature__

    def test_other_functions_keep_their_entries(self):
        def base():
            pass

        @functools.wraps(base)
        def wrapper():
            pass

        for _ in range(N):
            a = (hasattr(plain, "__wrapped__"), getattr(plain, "__wrapped__", 0))
            b = (hasattr(wrapper, "__wrapped__"), getattr(wrapper, "__wrapped__", 0))
        self.assertEqual(a, (False, 0))
        self.assertEqual(b, (True, base))

    def test_signatures(self):
        for _ in range(N // 10):
            s = inspect.signature(f)
            t = inspect.signature(plain)
        self.assertEqual(str(s), "(a, b=1, *args, c, d=2, **kw)")
        self.assertEqual(str(t), "(x)")

    def test_generator_probes(self):
        def gen():
            yield 1

        async def coro():
            pass

        g = gen()
        c = coro()
        for _ in range(N):
            r = (hasattr(g, "keys"), getattr(g, "items", "d"), hasattr(g, "send"),
                 hasattr(g, "gi_code"), hasattr(g, "gi_nope"), hasattr(c, "keys"),
                 hasattr(c, "cr_await"))
        c.close()
        self.assertEqual(r, (False, "d", True, True, False, False, True))
        with self.assertRaises(AttributeError):
            g.keys


if __name__ == "__main__":
    unittest.main()
