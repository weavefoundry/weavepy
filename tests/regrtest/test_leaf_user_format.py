"""A class's pure `__format__` runs frameless for f-strings and format();
results, errors, and every case the frameless call declines match the full
call."""

import unittest


class P:
    def __init__(self, v):
        self.v = v

    def __format__(self, spec):
        return format(self.v, spec)


class Bad:
    def __format__(self, spec):
        return 5


class Raises:
    def __format__(self, spec):
        if spec == "boom":
            raise ValueError(spec)
        return "ok:" + spec


class Sub(P):
    pass


class LeafUserFormat(unittest.TestCase):
    def test_values(self):
        p, q = P(5), P(2.5)
        for _ in range(300):
            self.assertEqual(f"{p:>4}", "   5")
            self.assertEqual(f"{p}", "5")
            self.assertEqual(format(p, "03d"), "005")
            self.assertEqual(f"{q:.2f}", "2.50")
            self.assertEqual("{:x}".format(P(255)), "ff")
            self.assertEqual(f"{Sub('ab'):>3}", " ab")

    def test_errors(self):
        for _ in range(200):
            with self.assertRaises(TypeError):
                f"{Bad()}"
            with self.assertRaises(ValueError):
                format(P(5), "q")
            with self.assertRaises(ValueError):
                f"{Raises():boom}"
            self.assertEqual(f"{Raises():x}", "ok:x")

    def test_patched(self):
        class T:
            def __format__(self, spec):
                return "a"
        t = T()
        for _ in range(300):
            self.assertEqual(f"{t}", "a")
        T.__format__ = lambda self, spec: "b"
        self.assertEqual(f"{t}", "b")
        del T.__format__
        self.assertEqual(format(t, ""), str(t))


if __name__ == "__main__":
    unittest.main()
