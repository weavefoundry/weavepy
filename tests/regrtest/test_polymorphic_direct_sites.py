"""A polymorphic call site's direct-call cache.

A compiled `CALL` site keeps several callees (compiled bodies and
frameless leaves alike) and replaces them round robin. Whatever the mix,
and however often it changes, each call must run its own callee with
its own arguments and defaults, and a callee whose code is swapped or
whose body compiles later must still run as itself.
"""

import sys
import unittest

N = 3000
FORCED = "--forced-frame-jit"


class Num:
    def __init__(self, v):
        self.v = v

    def eval(self, env):
        return self.v


class Var:
    def __init__(self, name):
        self.name = name

    def eval(self, env):
        return env[self.name]


class Neg:
    def __init__(self, inner):
        self.inner = inner

    def eval(self, env):
        t = [self.inner.eval(env)]
        return -t[0]


class Add:
    def __init__(self, a, b):
        self.a = a
        self.b = b

    def eval(self, env):
        t = [self.a.eval(env), self.b.eval(env)]
        return t[0] + t[1]


class Scaled:
    def __init__(self, inner, k=3):
        self.inner = inner
        self.k = k

    def eval(self, env, k=None):
        t = [self.inner.eval(env)]
        return t[0] * (self.k if k is None else k)


class Const:
    def eval(self, env, bonus=7):
        return bonus


def tree():
    return Add(
        Add(Num(1), Var("x")),
        Add(Neg(Var("y")), Add(Scaled(Num(2)), Add(Const(), Scaled(Var("x"), 5)))),
    )


def run(nodes, env, n):
    s = 0
    for i in range(n):
        node = nodes[i % len(nodes)]
        s += node.eval(env)
    return s


def expected(nodes, env, n):
    vals = [node.eval(env) for node in nodes]
    return sum(vals[i % len(vals)] for i in range(n))


class PolymorphicSiteTest(unittest.TestCase):
    def test_forced_frame_jit(self):
        if FORCED in sys.argv:
            self.skipTest("already forced")
        import os
        import subprocess

        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_megamorphic_mix(self):
        env = {"x": 4, "y": 9}
        t = tree()
        nodes = [t, Num(5), Var("y"), Neg(Num(3)), Scaled(Var("x")), Const()]
        self.assertEqual(run(nodes, env, N), expected(nodes, env, N))

    def test_changing_mix(self):
        env = {"x": 2, "y": 3}
        groups = [
            [Num(1), Var("x")],
            [Neg(Num(2)), Const(), Scaled(Num(1), 4)],
            [Add(Num(1), Num(2)), Var("y"), Num(9), Neg(Var("x")), Const()],
        ]
        for _ in range(3):
            for nodes in groups:
                self.assertEqual(run(nodes, env, N // 3), expected(nodes, env, N // 3))

    def test_swapped_code(self):
        env = {"x": 1, "y": 1}
        nodes = [Num(1), Var("x"), Neg(Num(1)), Const()]
        self.assertEqual(run(nodes, env, N), expected(nodes, env, N))
        saved = Num.eval.__code__
        try:
            Num.eval.__code__ = (lambda self, env: self.v * 100).__code__
            self.assertEqual(run(nodes, env, N), expected(nodes, env, N))
        finally:
            Num.eval.__code__ = saved
        self.assertEqual(run(nodes, env, N), expected(nodes, env, N))

    def test_deep_recursion_through_the_site(self):
        node = Num(1)
        for _ in range(150):
            node = Neg(node)
        for _ in range(20):
            self.assertEqual(node.eval({}), 1)
        for _ in range(sys.getrecursionlimit()):
            node = Neg(node)
        with self.assertRaises(RecursionError):
            node.eval({})


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
