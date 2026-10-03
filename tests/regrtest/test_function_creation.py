"""Functions made in a hot loop keep their attributes and call shapes.

A `def` or `lambda` that runs many times builds each function from a
template of its code's name slots, attaches its defaults and closure as it
is built, and is called through the call site's shape for its code. Every
function made here must still behave as CPython's do.
"""

import types
import unittest


def make_adder(n, scale=2):
    def add(x, y=10):
        return (x + y + n) * scale

    return add


def make_lambda(k):
    return lambda x, z=k: x * z


class FunctionCreationTests(unittest.TestCase):
    def test_closures_and_defaults(self):
        total = 0
        for i in range(5000):
            total += make_adder(i)(1)
        self.assertEqual(total, sum((1 + 10 + i) * 2 for i in range(5000)))
        f = make_adder(3, scale=5)
        self.assertEqual(f(1, 2), 30)
        self.assertEqual(f.__defaults__, (10,))
        self.assertEqual(f.__closure__[0].cell_contents, 3)
        self.assertEqual([make_lambda(k)(3) for k in range(5)], [0, 3, 6, 9, 12])
        g = make_lambda(4)
        self.assertEqual(g(2, 5), 10)
        self.assertEqual(g.__defaults__, (4,))

    def test_names_and_identity(self):
        fs = [make_adder(i) for i in range(100)]
        self.assertEqual({f.__name__ for f in fs}, {"add"})
        self.assertEqual(fs[0].__qualname__, "make_adder.<locals>.add")
        self.assertEqual(fs[0].__module__, __name__)
        self.assertIs(fs[0].__name__, fs[1].__name__)
        fs[0].__name__ = "renamed"
        fs[0].__qualname__ = "q"
        self.assertEqual(fs[0].__name__, "renamed")
        self.assertEqual(fs[1].__name__, "add")
        self.assertEqual(fs[1].__qualname__, "make_adder.<locals>.add")
        self.assertEqual(fs[2].__dict__, {})
        fs[2].tag = 1
        self.assertEqual(fs[2].__dict__, {"tag": 1})
        self.assertEqual(fs[3].__dict__, {})

    def test_module_name_follows_globals(self):
        src = "def f():\n    return __name__\n"
        code = compile(src, "<m>", "exec")
        first, second = {"__name__": "first"}, {"__name__": "second"}
        for _ in range(100):
            exec(code, first)
            exec(code, second)
        self.assertEqual(first["f"].__module__, "first")
        self.assertEqual(second["f"].__module__, "second")
        self.assertEqual(second["f"](), "second")
        anon = {}
        exec(code, anon)
        self.assertIsNone(anon["f"].__module__)
        clone = types.FunctionType(first["f"].__code__, {"__name__": "third"})
        self.assertEqual(clone.__module__, "third")

    def test_keyword_defaults_and_annotations(self):
        def build(i):
            def h(a: int, *, b=i) -> int:
                return a + b

            return h

        hs = [build(i) for i in range(2000)]
        self.assertEqual(hs[7](1), 8)
        self.assertEqual(hs[7].__kwdefaults__, {"b": 7})
        self.assertEqual(hs[7].__annotations__, {"a": int, "return": int})

    def test_name_slots_read_and_written_later(self):
        import functools
        import pickle

        fs = [make_adder(i) for i in range(50)]

        @functools.wraps(fs[0])
        def wrapper(*args):
            return fs[0](*args)

        self.assertEqual(wrapper.__name__, "add")
        self.assertEqual(wrapper.__qualname__, "make_adder.<locals>.add")
        self.assertEqual(wrapper.__module__, __name__)
        self.assertIs(wrapper.__wrapped__, fs[0])
        fs[1].__module__ = "elsewhere"
        self.assertEqual(fs[1].__module__, "elsewhere")
        self.assertEqual(fs[2].__module__, __name__)
        fs[3].__doc__ = "doc"
        self.assertEqual((fs[3].__doc__, fs[4].__doc__), ("doc", None))
        self.assertIs(pickle.loads(pickle.dumps(make_adder)), make_adder)
        g = [make_lambda(k) for k in range(3)]
        self.assertEqual({h.__name__ for h in g}, {"<lambda>"})
        self.assertEqual(g[0].__qualname__, "make_lambda.<locals>.<lambda>")

    def test_free_variable_reads(self):
        def counter():
            n = 0

            def get():
                return n

            def bump():
                nonlocal n
                n += 1

            return get, bump

        get, bump = counter()
        for _ in range(5000):
            self.assertEqual(get(), 0)
        bump()
        self.assertEqual(get(), 1)

        def late():
            def read():
                return value

            out = [read]
            try:
                read()
            except NameError as e:
                out.append(type(e))
            value = 7
            out.append(read())
            return out

        for _ in range(3000):
            r = late()
        self.assertEqual(r[1:], [NameError, 7])

        def scaled(k):
            return lambda x: x * k + len(box)

        box = [1, 2]
        fs = [scaled(i) for i in range(4000)]
        self.assertEqual(sum(f(1) for f in fs), sum(range(4000)) + 2 * 4000)
        box.append(3)
        self.assertEqual(fs[5](2), 13)

    def test_fresh_functions_share_call_shapes(self):
        def caller(n):
            t = 0
            for i in range(n):
                t += (lambda x, y=2: x * y)(i)
            return t

        self.assertEqual(caller(5000), 2 * sum(range(5000)))

        def defaults_change(n):
            out = []
            for i in range(n):
                f = lambda x, y=i: x + y
                out.append(f(1))
            return out

        self.assertEqual(defaults_change(300)[-3:], [298, 299, 300])


if __name__ == "__main__":
    unittest.main()
