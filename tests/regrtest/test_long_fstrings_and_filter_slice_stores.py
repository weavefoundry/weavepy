"""Compiled f-strings of up to sixteen parts, and slice stores from `filter`.

`f"{a}-{b}-{c}-{d}-{e}"` (nine parts, `UUID.__str__`'s shape) joins in
compiled code; past sixteen parts, or with a non-`str` part, the full
path joins. `xs[a:b] = filter(None, ys)` (`urljoin`'s) drains the filter
in place when the items' truth runs no code, and takes the full path
otherwise (an instance's `__bool__`, a `filter` with a Python function),
including when the filter reads the list being assigned.
"""

import unittest


def nine(a, b, c, d, e):
    return f"{a}-{b}-{c}-{d}-{e}"


def seventeen(*p):
    return f"{p[0]}{p[1]}{p[2]}{p[3]}{p[4]}{p[5]}{p[6]}{p[7]}{p[8]}|{p[9]}|{p[10]}|{p[11]}|{p[12]}|{p[13]}|{p[14]}|{p[15]}|{p[16]}"


class Truthy:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        return self.v > 0


def slice_filter(xs, ys):
    xs[1:-1] = filter(None, ys)
    return xs


class LongFStringTests(unittest.TestCase):
    def test_nine_parts(self):
        for i in range(300):
            self.assertEqual(nine("ab", i, 1.5, None, "€"), f"ab-{i}-1.5-None-€")
        self.assertEqual(nine("x\udcac", "", "", "", ""), "x\udcac----")

    def test_past_the_limit(self):
        parts = [str(i) for i in range(17)]
        want = "".join(parts[:9]) + "|" + "|".join(parts[9:])
        for _ in range(300):
            self.assertEqual(seventeen(*parts), want)


class FilterSliceStoreTests(unittest.TestCase):
    def test_strings(self):
        for _ in range(300):
            self.assertEqual(slice_filter(["a", "b", "c", "d"], ["", "x", "", "y"]), ["a", "x", "y", "d"])
            self.assertEqual(slice_filter([1, 2], [0, 0]), [1, 2])
            self.assertEqual(slice_filter([1, 2, 3], []), [1, 3])

    def test_full_path_cases(self):
        objs = [Truthy(1), Truthy(0), Truthy(2)]
        for _ in range(100):
            got = slice_filter(["a", "b", "c"], objs)
            self.assertEqual([getattr(o, "v", o) for o in got], ["a", 1, 2, "c"])
        xs = ["", "p", "", "q"]
        for _ in range(100):
            ys = list(xs)
            ys[1:-1] = filter(lambda s: s, ys)
            self.assertEqual(ys, ["", "p", "q", "q"])

    def test_filter_over_target(self):
        for _ in range(100):
            xs = ["a", "", "b", "", "c"]
            xs[:] = filter(None, xs)
            self.assertEqual(xs, ["a", "b", "c"])
            ys = ["", "z"]
            it = filter(None, ys)
            ys[0:0] = it
            self.assertEqual(ys, ["z", "", "z"])
            self.assertEqual(list(it), [])


if __name__ == "__main__":
    unittest.main()
