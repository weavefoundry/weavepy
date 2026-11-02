"""A class with a Python `__new__`, constructed in compiled loops.

A loop hot enough to compile must still run the class's `__new__` and use
what it returns: an instance of a subclass, an instance carrying
attributes `__new__` set, or an object of another type entirely. That
holds for a `__new__` the class inherits, and for one added after the
loop compiled. Each case runs in fresh interpreters with every tier on,
with the method tier off, with a low compile threshold, and with the
compilers off.
"""

import os
import subprocess
import sys
import unittest

CASES = r'''
class R:
    def __new__(cls, x):
        if x:
            return object.__new__(C)
        return object.__new__(cls)

    def __init__(self, x):
        self.ran = x


class C(R):
    pass


class K:
    def __new__(cls, x):
        def inner():
            return x * 2

        inst = object.__new__(cls)
        inst.v = inner()
        return inst

    def __init__(self, x):
        self.w = x


class Inherits(K):
    def __init__(self, x):
        self.w = -x


class Other:
    def __new__(cls, x):
        return ("other", x)

    def __init__(self, x):
        raise AssertionError("__init__ must not run")


class Late:
    def __init__(self, x):
        self.x = x


class Check:
    def eq(self, a, b):
        if a != b:
            raise AssertionError((a, b))

    def run(self, n):
        for i in range(n):
            c = R(1)
            self.eq(type(c), C)
            self.eq(c.ran, 1)
            self.eq(type(R(0)), R)
            k = K(i)
            self.eq((k.v, k.w), (2 * i, i))
            h = Inherits(i)
            self.eq((type(h), h.v, h.w), (Inherits, 2 * i, -i))
            self.eq(Other(i), ("other", i))

    def late(self, n, added):
        for i in range(n):
            p = Late(i)
            if added:
                self.eq((p.x, p.tag), (i, "new"))
            else:
                self.eq(p.x, i)


check = Check()
check.run(400)
check.late(200, False)


def new(cls, x):
    inst = object.__new__(cls)
    inst.tag = "new"
    return inst


Late.__new__ = new
check.late(200, True)
check.run(100)
print("ok")
'''

ENVS = [
    {},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_JIT": "0"},
]


class CompiledPythonNewTests(unittest.TestCase):
    def test_cases(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                env = dict(os.environ, **extra)
                result = subprocess.run(
                    [sys.executable, "-c", CASES],
                    capture_output=True,
                    text=True,
                    env=env,
                    timeout=300,
                )
                self.assertEqual(result.returncode, 0, result.stderr[-3000:])
                self.assertEqual(result.stdout.strip(), "ok")


if __name__ == "__main__":
    unittest.main()
