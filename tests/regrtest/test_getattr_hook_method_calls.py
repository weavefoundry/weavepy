"""Method calls through a class's __getattr__ from compiled loops: a
wrapper delegating to its target's bound methods, a hook returning a
plain function or a lambda (no self bound), a hook that raises
AttributeError for some names, an instance attribute or class method
that shadows the hook, and a hook replaced partway, each matching
CPython."""

import unittest


class Wrap:
    def __init__(self, o):
        self._o = o

    def __getattr__(self, name):
        return getattr(self._o, name)


class Picky:
    def __getattr__(self, name):
        if name.startswith("ok"):
            return len
        raise AttributeError(name)


class Shadow(Wrap):
    def count(self, x):
        return -1


def calls(n, w, x):
    out = None
    for i in range(n):
        out = w.count(x)
    return out


class HookTests(unittest.TestCase):
    def test_delegate(self):
        for n in (1, 400):
            self.assertEqual(calls(n, Wrap([1, 2, 1]), 1), 2)
            self.assertEqual(calls(n, Wrap("abca"), "a"), 2)
            self.assertEqual(calls(n, Shadow([1]), 1), -1)

    def test_plain_function(self):
        def run(n):
            p = Picky()
            out = None
            for i in range(n):
                out = p.ok_len("abc")
            return out

        self.assertEqual(run(400), 3)
        with self.assertRaises(AttributeError):
            for i in range(400):
                Picky().nope()

    def test_instance_shadow(self):
        w = Wrap([1, 2])
        w.count = lambda x: "own"
        self.assertEqual(calls(400, w, 1), "own")

    def test_replaced_hook(self):
        class W(Wrap):
            pass

        w = W([1])
        self.assertEqual(calls(400, w, 1), 1)
        W.__getattr__ = lambda self, name: (lambda x: "new")
        self.assertEqual(calls(400, w, 1), "new")


if __name__ == "__main__":
    unittest.main()
