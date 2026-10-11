"""A `LOAD_GLOBAL` site's cached value follows every namespace change.

The core loop keeps the value a `LOAD_GLOBAL` site last read, in place,
while the module and builtins dicts are exactly as they were. Rebinding,
deleting, shadowing or unshadowing a name, changing a builtin, and running
the same code against another globals dict must each be seen at once.
"""

import builtins
import unittest

G = 1


def read_g(n):
    total = 0
    for _ in range(n):
        total += G
    return total


def read_len(xs, n):
    total = 0
    for _ in range(n):
        total += len(xs)
    return total


def read_abs(n):
    total = 0
    for i in range(n):
        total += abs(-i)
    return total


def read_missing():
    return MISSING_NAME  # noqa: F821


class GlobalCacheTest(unittest.TestCase):
    def test_rebind(self):
        global G
        try:
            for v in (1, 2, "x", None, 3.5, 7):
                G = v
                if isinstance(v, str):
                    self.assertEqual(read_g(0), 0)
                    continue
                if v is None:
                    with self.assertRaises(TypeError):
                        read_g(3)
                    continue
                self.assertEqual(read_g(200), v * 200)
        finally:
            G = 1

    def test_delete_and_restore(self):
        global G
        self.assertEqual(read_g(100), 100)
        del G
        try:
            with self.assertRaises(NameError):
                read_g(1)
        finally:
            G = 4
        self.assertEqual(read_g(100), 400)
        G = 1

    def test_builtin_shadowing(self):
        xs = [1, 2, 3]
        self.assertEqual(read_len(xs, 300), 900)
        globals()["len"] = lambda s: 10
        try:
            self.assertEqual(read_len(xs, 300), 3000)
        finally:
            del globals()["len"]
        self.assertEqual(read_len(xs, 300), 900)

    def test_builtin_replaced(self):
        self.assertEqual(read_abs(300), sum(range(300)))
        saved = builtins.abs
        builtins.abs = lambda v: 1
        try:
            self.assertEqual(read_abs(300), 300)
        finally:
            builtins.abs = saved
        self.assertEqual(read_abs(300), sum(range(300)))

    def test_other_globals(self):
        src = "def f(n):\n    t = 0\n    for _ in range(n):\n        t += K\n    return t\n"
        code = compile(src, "<g>", "exec")
        fs = []
        for k in range(4):
            ns = {"K": k + 1}
            exec(code, ns)
            fs.append((ns, ns["f"]))
        for _ in range(3):
            for k, (ns, f) in enumerate(fs):
                self.assertEqual(f(150), (k + 1) * 150)
        fs[2][0]["K"] = 100
        self.assertEqual(fs[2][1](10), 1000)
        self.assertEqual(fs[1][1](10), 20)

    def test_missing_then_defined(self):
        for _ in range(50):
            with self.assertRaises(NameError):
                read_missing()
        globals()["MISSING_NAME"] = 5
        try:
            self.assertEqual(read_missing(), 5)
        finally:
            del globals()["MISSING_NAME"]
        with self.assertRaises(NameError):
            read_missing()


if __name__ == "__main__":
    unittest.main()
