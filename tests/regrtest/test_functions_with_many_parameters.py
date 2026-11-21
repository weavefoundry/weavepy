"""Functions with very many parameters and locals.

Generated code (a `namedtuple`'s constructor, a dataclass's `__init__`, a
template compiled to one function) can have thousands of parameters and
locals. Parsing and compiling such a function takes time in proportion to
its size, a duplicated parameter is still reported, and the function
works.
"""

import collections
import time
import unittest


def source(n):
    names = ["p%d" % i for i in range(n)]
    args = ", ".join(names)
    return "def f(%s):\n    return (%s)\n" % (args, args), names


class ManyParameterTests(unittest.TestCase):
    def test_a_function_of_two_thousand_parameters(self):
        n = 2000
        src, names = source(n)
        ns = {}
        exec(src, ns)
        f = ns["f"]
        self.assertEqual(f(*range(n)), tuple(range(n)))
        self.assertEqual(f.__code__.co_argcount, n)
        self.assertEqual(f.__code__.co_varnames, tuple(names))
        self.assertEqual(f(**{name: i for i, name in enumerate(names)}), tuple(range(n)))
        with self.assertRaises(TypeError):
            f(*range(n - 1))

    def test_a_lambda_and_keyword_only_parameters(self):
        n = 1500
        names = ["q%d" % i for i in range(n)]
        g = eval("lambda %s, *, %s: (%s, %s)" % (", ".join(names), ", ".join("k" + x for x in names), names[0], "k" + names[-1]))
        self.assertEqual(g(*range(n), **{"k" + x: -i for i, x in enumerate(names)}), (0, -(n - 1)))

    def test_duplicates_are_reported(self):
        n = 3000
        names = ["p%d" % i for i in range(n)]
        for dup in (0, n // 2, n - 1):
            args = ", ".join(names + [names[dup]])
            with self.assertRaises(SyntaxError) as cm:
                compile("def f(%s): pass" % args, "<many>", "exec")
            self.assertIn("duplicate argument 'p%d'" % dup, str(cm.exception))
        with self.assertRaises(SyntaxError):
            compile("lambda %s, *%s: 0" % (", ".join(names), names[7]), "<many>", "eval")
        with self.assertRaises(SyntaxError):
            compile("def f(%s, *, %s): pass" % (", ".join(names), names[-1]), "<many>", "exec")

    def test_many_locals(self):
        n = 3000
        body = "\n".join("    v%d = %d" % (i, i) for i in range(n))
        total = " + ".join("v%d" % i for i in range(0, n, 97))
        ns = {}
        exec("def g():\n%s\n    return %s\n" % (body, total), ns)
        self.assertEqual(ns["g"](), sum(range(0, n, 97)))
        self.assertEqual(len(ns["g"].__code__.co_varnames), n)

    def test_a_wide_namedtuple(self):
        n = 3000
        names = ["f%d" % i for i in range(n)]
        Wide = collections.namedtuple("Wide", names)
        w = Wide(*range(n))
        self.assertEqual(w, tuple(range(n)))
        self.assertEqual((w.f0, w.f1499, getattr(w, names[-1])), (0, 1499, n - 1))
        self.assertEqual(w._replace(f7=-7)[7], -7)
        self.assertEqual(Wide._make(range(n)), w)
        self.assertEqual(len(w._asdict()), n)

    def test_time_follows_size(self):
        def cost(n):
            src, _ = source(n)
            best = None
            for _ in range(3):
                t0 = time.perf_counter()
                compile(src, "<many>", "exec")
                dt = time.perf_counter() - t0
                best = dt if best is None else min(best, dt)
            return best

        cost(500)
        small = cost(2000)
        large = cost(16000)
        # (Eight times the parameters took sixty times as long when each
        # name was looked up by scanning the others. Allow 24.)
        self.assertLess(large, 24 * small + 0.01, (small, large))


if __name__ == "__main__":
    unittest.main()
