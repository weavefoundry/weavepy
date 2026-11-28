"""sorted(), min() and max() with a native key function from compiled
loops: a builtin (abs, len, str.lower through the type) and a bound
method (d.get, a string's index), keys of one plain kind, ties (the first
extreme wins, the sort is stable), mixed or missing keys, reverse sorts,
empty inputs with and without a default, and a key that raises."""

import unittest


def run(n):
    out = None
    d = {"a": 3, "b": 1, "c": 3, "d": 0}
    for i in range(n):
        xs = [3, -5, 2, -2, 5]
        words = ["Bb", "aa", "Cc", "AA"]
        out = (
            max(d, key=d.get),
            min(d, key=d.get),
            sorted(d, key=d.get),
            sorted(d, key=d.get, reverse=True),
            max(xs, key=abs),
            min(xs, key=abs),
            sorted(xs, key=abs),
            max(words, key=str.lower),
            min(words, key=str.lower),
            sorted(words, key=str.lower),
            max(["x", "yyy", "zz"], key=len),
            min((2.5, -1.5, 0.5), key=abs),
            max([], key=abs, default="empty"),
            max(["b", "a"], key="ab".index),
            max([1.0, float("nan"), 3.0], key=float),
        )
    return out


def mixed(n):
    out = None
    d = {"a": 1, "b": 2.5}
    for i in range(n):
        out = (
            max(d, key=d.get),
            max(["zz", "a"], key={"a": 1, "zz": 1}.get),
            sorted([1, 2.5, -3], key=abs),
        )
    return out


def raising(n):
    caught = 0
    for i in range(n):
        try:
            max(["a", 1], key=str.lower)
        except TypeError:
            caught += 1
        try:
            min([], key=abs)
        except ValueError:
            caught += 1
        try:
            max(["zz", "a"], key={"a": 1, "zz": None}.get)
        except TypeError:
            caught += 1
    return caught


class Tests(unittest.TestCase):
    def test_values(self):
        got = run(3000)
        self.assertEqual(got, (
            "a",
            "d",
            ["d", "b", "a", "c"],
            ["a", "c", "b", "d"],
            -5,
            2,
            [2, -2, 3, -5, 5],
            "Cc",
            "aa",
            ["aa", "AA", "Bb", "Cc"],
            "yyy",
            0.5,
            "empty",
            "b",
            3.0,
        ))

    def test_mixed_keys(self):
        self.assertEqual(mixed(2000), ("b", "zz", [1, 2.5, -3]))

    def test_raising(self):
        self.assertEqual(raising(1000), 3000)


if __name__ == "__main__":
    unittest.main()
