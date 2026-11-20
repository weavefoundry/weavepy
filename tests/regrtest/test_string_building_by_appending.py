"""Building a string by appending to a local in a loop.

`s += piece` (or `s = s + piece`) on a local whose string nothing else
holds grows the string where it is, so a loop that builds a long string
costs its length once, not once per append. Nothing else may see the
string change: a second name for it, a container that holds it, a dict
key, or a hash taken earlier all keep the value they had, and the loop
gives the same string whether it runs a few times or long enough to be
compiled.
"""

import time
import unittest

SHORT = 60
LONG = 40_000


def build(n, piece):
    s = ""
    for _ in range(n):
        s += piece
    return s


def build_plus(n, piece):
    s = ""
    for _ in range(n):
        s = s + piece
    return s


def build_parts(n):
    s = ""
    for i in range(n):
        s = s + str(i & 7) + ","
    return s


def build_with_aliases(n):
    s = "start"
    kept = []
    for i in range(n):
        if i % 1000 == 0:
            kept.append(s)
        s += "x"
    return s, kept


def build_with_second_name(n):
    s = "ab"
    seen = []
    for i in range(n):
        t = s
        s += "c"
        if i % 997 == 0:
            seen.append((len(t), len(s), t is s, t[-1], s[-1]))
        del t
    return seen, len(s)


def build_as_dict_keys(n):
    d = {}
    s = "k"
    for i in range(n):
        d[s] = i
        s += "x"
    return d


def build_mixed_widths(n):
    s = ""
    for i in range(n):
        s += "é" if i & 1 else "z"
        if i % 5000 == 0:
            hash(s)
            len(s)
    return s


def double(n):
    s = "a"
    for _ in range(n):
        s += s
    return s


class AppendTests(unittest.TestCase):
    def test_the_string_built(self):
        for n in (SHORT, LONG):
            for piece in ("ab", "", "é", "long piece of text "):
                want = piece * n
                self.assertEqual(build(n, piece), want)
                self.assertEqual(build_plus(n, piece), want)
                self.assertEqual(hash(build(n, piece)), hash(want))
                self.assertEqual(len(build(n, piece)), len(want))
            self.assertEqual(build_parts(n), "".join("%d," % (i & 7) for i in range(n)))

    def test_other_holders_keep_their_value(self):
        for n in (SHORT, LONG):
            s, kept = build_with_aliases(n)
            self.assertEqual(s, "start" + "x" * n)
            for i, k in enumerate(kept):
                self.assertEqual(k, "start" + "x" * (1000 * i))
            seen, length = build_with_second_name(n)
            self.assertEqual(length, n + 2)
            for k, (lt, ls, same, tl, sl) in enumerate(seen):
                self.assertEqual((lt, ls, same, sl), (997 * k + 2, 997 * k + 3, False, "c"))

    def test_keys_and_hashes(self):
        for n in (SHORT, 5000):
            d = build_as_dict_keys(n)
            self.assertEqual(len(d), n)
            self.assertTrue(all(len(k) == v + 1 for k, v in d.items()))
            self.assertEqual(d["k" + "x" * (n - 1)], n - 1)

    def test_mixed_character_widths(self):
        for n in (SHORT, LONG):
            s = build_mixed_widths(n)
            want = "".join("é" if i & 1 else "z" for i in range(n))
            self.assertEqual(s, want)
            self.assertEqual(len(s), n)
            self.assertEqual(s[n // 2], want[n // 2])
            self.assertEqual(s.encode(), want.encode())

    def test_a_string_added_to_itself(self):
        self.assertEqual(double(16), "a" * (1 << 16))

    def test_other_types_through_the_same_statement(self):
        def run(start, piece, n):
            x = start
            for _ in range(n):
                x += piece
            return x

        for n in (SHORT, LONG):
            self.assertEqual(run(0, 2, n), 2 * n)
            self.assertEqual(run(0.5, 1.5, n), 0.5 + 1.5 * n)
            self.assertEqual(run([], [1], n), [1] * n)
            self.assertEqual(run((), (1,), min(n, 3000)), (1,) * min(n, 3000))
            self.assertEqual(run(b"", b"ab", min(n, 5000)), b"ab" * min(n, 5000))
        with self.assertRaises(TypeError):
            run("a", 1, LONG)


class CostTests(unittest.TestCase):
    def per_append(self, n):
        best = None
        for _ in range(3):
            t0 = time.perf_counter()
            build(n, "ab")
            dt = (time.perf_counter() - t0) / n
            best = dt if best is None else min(best, dt)
        return best

    def test_cost_of_an_append_does_not_grow(self):
        # (A string 16 times as long cost 16 times as much an append when
        # each one copied the whole string. Allow 4, far past noise.)
        build(LONG, "ab")
        small = self.per_append(20_000)
        large = self.per_append(320_000)
        self.assertLess(large, 4 * small + 1e-7, (small, large))


if __name__ == "__main__":
    unittest.main()
