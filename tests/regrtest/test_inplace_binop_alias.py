"""Augmented assignments on mutable values change them in place, in every
tier: an alias taken before `a += m` sees the change and stays `a`.

The frame JIT's binary-operator helper dropped the instruction's in-place
flag, so a compiled `a += m` on two lists built a new list: `b = a;
a += m` left `b` behind once the loop compiled. The cases cover the
in-place shapes the helpers and the core loop serve (a list's `+=` with
another list, itself and a tuple; a set's `|=`, `&=`, `-=` and `^=`; a
dict's `|=`; a bytearray's `+=`), a class with `__iadd__` and one with
only `__add__` (whose `+=` makes a new object), and the immutable kinds
whose `+=` rebinds.

Each case runs in a fresh interpreter with every tier on, with a low
compile threshold, with a low frame-JIT threshold, with the method tier
off and with the compilers off (as test_override_fast_paths does)."""

import os
import subprocess
import sys
import unittest

CASES = {}

CASES["mutable_inplace"] = r'''
class Acc:
    def __init__(self):
        self.items = []

    def __iadd__(self, other):
        self.items.append(other)
        return self


class Plus:
    def __init__(self, n):
        self.n = n

    def __add__(self, other):
        return Plus(self.n + other)


def run(n):
    same = [0] * 12
    sizes = None
    m = [2, 3]
    for i in range(n):
        a = [1]
        b = a
        a += m
        same[0] += b is a
        a += a
        same[1] += b is a
        a += (i,)
        same[2] += b is a

        s = {1, 2, 3}
        t = s
        s |= {4}
        same[3] += t is s
        s &= {1, 2, 4}
        same[4] += t is s
        s -= {1}
        same[5] += t is s
        s ^= {9}
        same[6] += t is s

        d = {"a": 1}
        e = d
        d |= {"b": i}
        same[7] += e is d

        ba = bytearray(b"x")
        bb = ba
        ba += b"yz"
        same[8] += bb is ba

        acc = Acc()
        alias = acc
        acc += i
        same[9] += alias is acc

        p = Plus(1)
        q = p
        p += 2
        same[10] += q is not p and q.n == 1 and p.n == 3

        st = "s"
        sa = st
        st += "t"
        tu = (1,)
        ta = tu
        tu += (2,)
        same[11] += sa == "s" and st == "st" and ta == (1,) and tu == (1, 2)
        sizes = (b, sorted(t), e, bytes(bb), alias.items)
    return same, sizes


for _ in range(3):
    r = run(1000)
print(r)
# (The same at module level, where the loop itself compiles.)
m = [5]
agree = 0
for i in range(3000):
    a = [i]
    b = a
    a += m
    agree += b is a and b == [i, 5]
print(agree)
'''

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    "mutable_inplace": "([1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000], "
    "([1, 2, 3, 1, 2, 3, 999], [2, 4, 9], {'a': 1, 'b': 999}, b'xyz', [999]))\n3000\n",
}


def run_case(name, extra):
    env = dict(os.environ, **extra)
    result = subprocess.run(
        [sys.executable, "-c", CASES[name]],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class InplaceBinopAliasTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in CASES:
    setattr(InplaceBinopAliasTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
