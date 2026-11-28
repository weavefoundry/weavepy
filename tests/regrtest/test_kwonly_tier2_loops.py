"""Hot loops in functions with keyword-only parameters.

Tier 2 compiles frames whose signatures carry keyword-only parameters;
the parameters are locals the caller's binding fills before the body
runs. These loops cover defaults, explicit values, rebinding, deletion,
and values of several types.
"""

import unittest


def scaled(n, *, scale=2, off=1):
    t = 0
    for i in range(n):
        t += i * scale + off
    return t


def floats(xs, *, key=3):
    t = 0.0
    for x in xs:
        t += x * 1.5 + key
    return t


def rebinds(n, *, step=1):
    t = 0
    for i in range(n):
        t += step
        if i % 7 == 0:
            step = step + 1
    return t, step


def deletes(n, *, tag="a"):
    out = 0
    for i in range(n):
        out += len(tag)
    del tag
    try:
        tag  # noqa: B018
    except UnboundLocalError:
        return out, "unbound"
    return out, "bound"


def mixed(n, a, /, b, *, c, d=None):
    t = 0
    for i in range(n):
        t += a + b + c + (0 if d is None else d)
    return t


class Acc:
    def run(self, n, *, by=1):
        t = 0
        for i in range(n):
            t += by
        return t


class KwonlyLoopTests(unittest.TestCase):
    N = 30000

    def test_defaults_and_explicit(self):
        n = self.N
        for _ in range(3):
            self.assertEqual(scaled(n), sum(i * 2 + 1 for i in range(n)))
            self.assertEqual(scaled(n, scale=3, off=0), sum(i * 3 for i in range(n)))

    def test_float_and_int_key(self):
        xs = [float(i) for i in range(self.N)]
        expect = sum(x * 1.5 + 2 for x in xs)
        for _ in range(3):
            self.assertEqual(floats(xs, key=2), expect)
            self.assertEqual(floats(xs, key=2.5), sum(x * 1.5 + 2.5 for x in xs))

    def test_type_change_between_calls(self):
        for _ in range(3):
            self.assertEqual(scaled(1000, scale=2.0), float(scaled(1000)))
            self.assertEqual(scaled(1000, scale=1 << 70, off=0),
                             sum(i * (1 << 70) for i in range(1000)))

    def test_rebinding(self):
        n = self.N
        t, step = 0, 1
        for i in range(n):
            t += step
            if i % 7 == 0:
                step += 1
        for _ in range(3):
            self.assertEqual(rebinds(n), (t, step))

    def test_deletion(self):
        for _ in range(3):
            self.assertEqual(deletes(self.N, tag="xyz"), (3 * self.N, "unbound"))

    def test_mixed_signature(self):
        for _ in range(3):
            self.assertEqual(mixed(self.N, 1, 2, c=3), 6 * self.N)
            self.assertEqual(mixed(self.N, 1, b=2, c=3, d=4), 10 * self.N)

    def test_method(self):
        a = Acc()
        for _ in range(3):
            self.assertEqual(a.run(self.N), self.N)
            self.assertEqual(a.run(self.N, by=5), 5 * self.N)

    def test_missing_kwonly_still_raises(self):
        with self.assertRaises(TypeError):
            mixed(10, 1, 2)


if __name__ == "__main__":
    unittest.main()
