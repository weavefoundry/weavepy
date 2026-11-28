"""Functions nested many levels deep.

Each nested function reads names from the scopes around it; compiling
the nest works out, once per function, which of its names are free and
which enclosing locals become cells. The cost of compiling grows with
the nest's size and depth, not with a power of its depth, and every
level still sees the right variable.
"""

import time
import unittest


def nested_source(depth):
    lines = ["def f0():", "    v0 = 0"]
    for i in range(depth):
        pad = "    " * (i + 1)
        lines.append(f"{pad}def f{i + 1}():")
        lines.append(f"{pad}    v{i + 1} = v{i} + 1")
    lines.append("    " * (depth + 1) + f"return v{depth}")
    for i in range(depth, 0, -1):
        lines.append("    " * i + f"return f{i}()")
    return "\n".join(lines) + "\n"


class NestedClosureTests(unittest.TestCase):
    def test_every_level_reads_the_level_above(self):
        for depth in (1, 2, 5, 20, 60):
            with self.subTest(depth=depth):
                ns = {}
                exec(compile(nested_source(depth), "<nest>", "exec"), ns)
                self.assertEqual(ns["f0"](), depth)

    def test_cells_and_free_variables(self):
        ns = {}
        exec(compile(nested_source(3), "<nest>", "exec"), ns)
        code = ns["f0"].__code__
        self.assertEqual(code.co_cellvars, ("v0",))
        inner = [c for c in code.co_consts if hasattr(c, "co_freevars")][0]
        self.assertEqual(inner.co_freevars, ("v0",))
        self.assertEqual(inner.co_cellvars, ("v1",))

    def test_names_bound_on_the_way_are_not_reached_through(self):
        def outer():
            x = "outer"

            def middle():
                x = "middle"

                def inner():
                    def innermost():
                        return x

                    return innermost()

                return inner()

            return middle(), x

        self.assertEqual(outer(), ("middle", "outer"))

        def with_params(x):
            def f(x):
                def g():
                    return x

                return g()

            return f("param"), x

        self.assertEqual(with_params("arg"), ("param", "arg"))

    def test_nonlocal_and_global_deep_down(self):
        src = """
counter = 100
def a():
    n = 0
    def b():
        def c():
            nonlocal n
            n += 1
            def d():
                global counter
                counter += 1
                return n
            return d()
        return c()
    return b(), b(), n
"""
        ns = {}
        exec(src, ns)
        self.assertEqual(ns["a"](), (1, 2, 2))
        self.assertEqual(ns["counter"], 102)

    def test_lambdas_and_comprehensions_in_the_nest(self):
        def make(k):
            base = k

            def level1():
                return [lambda i=i: base + i for i in range(3)]

            def level2():
                return {j: (lambda: base * j)() for j in range(3)}

            return [f() for f in level1()], level2()

        self.assertEqual(make(10), ([10, 11, 12], {0: 0, 1: 10, 2: 20}))

    def test_compile_cost_grows_slowly_with_depth(self):
        def best(src):
            t = float("inf")
            for _ in range(7):
                start = time.perf_counter()
                compile(src, "<nest>", "exec")
                t = min(t, time.perf_counter() - start)
            return t

        shallow = best(nested_source(20))
        deep = best(nested_source(80))
        # (Four times as deep: about sixteen times the work at worst.
        # Recomputing each nested function's free names at every level
        # above it made it about 40 times, and the cubic cost 64; the
        # slack absorbs a loaded runner's stalls on millisecond compiles.)
        self.assertLess(deep, shallow * 30 + 0.05)


if __name__ == "__main__":
    unittest.main()
