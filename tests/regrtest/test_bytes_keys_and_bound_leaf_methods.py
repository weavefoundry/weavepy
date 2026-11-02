"""`bytes` cache keys and built-in methods bound to subclass instances.

`functools.lru_cache` keys on `bytes` arguments (`urllib.parse`'s
`_byte_quoter_factory(safe)`) and dicts keyed by `bytes` settle natively,
with `bytes` and `str` keys kept apart. A built-in method bound to an
instance of a built-in subclass (`_Quoter(dict).__getitem__`, mapped over
bytes by `urllib.parse.quote`) returns stored values directly and still
reaches the subclass's `__missing__` for a missing key.
"""

import functools
import unittest
import urllib.parse

N = 300


class SubBytes(bytes):
    pass


class Missing(dict):
    def __missing__(self, key):
        value = "<%d>" % key
        self[key] = value
        return value


class LruBytesTests(unittest.TestCase):
    def test_hits_and_misses(self):
        calls = []

        @functools.lru_cache(maxsize=4)
        def f(x):
            calls.append(x)
            return len(x)

        for _ in range(N):
            self.assertEqual(f(b"ab"), 2)
            self.assertEqual(f(b"xyz"), 3)
        self.assertEqual(calls, [b"ab", b"xyz"])
        self.assertEqual(f.cache_info().hits, 2 * N - 2)
        # Evictions keep the order exact.
        for key in (b"1", b"2", b"3", b"4", b"ab"):
            f(key)
        self.assertEqual(calls[-5:], [b"1", b"2", b"3", b"4", b"ab"])

    def test_bytes_and_str_distinct(self):
        @functools.lru_cache(maxsize=None)
        def g(x):
            return type(x).__name__

        @functools.lru_cache(maxsize=8)
        def h(x, y=0):
            return (type(x).__name__, y)

        for _ in range(N // 10):
            self.assertEqual(g(b"k"), "bytes")
            self.assertEqual(g("k"), "str")
            self.assertEqual(h(b"k", 1), ("bytes", 1))
            self.assertEqual(h("k", 1), ("str", 1))
            self.assertEqual(h(b"k"), ("bytes", 0))

    def test_typed(self):
        @functools.lru_cache(typed=True)
        def t(x):
            return type(x).__name__

        for _ in range(N // 10):
            self.assertEqual(t(b"k"), "bytes")
            self.assertEqual(t(SubBytes(b"k")), "SubBytes")
        self.assertEqual(t.cache_info().currsize, 2)

    def test_quote(self):
        for i in range(N):
            self.assertEqual(urllib.parse.quote("a b/c?%d" % i), "a%%20b/c%%3F%d" % i)
            self.assertEqual(urllib.parse.quote(b"\xff/x", safe=b""), "%FF%2Fx")


class DictKeyTests(unittest.TestCase):
    def test_bytes_keys(self):
        d = {b"a": 1, "a": 2, (b"a", 1): 3, ("a", 1): 4}
        for _ in range(N // 10):
            self.assertEqual(d[b"a"], 1)
            self.assertEqual(d["a"], 2)
            self.assertEqual(d[(b"a", 1)], 3)
            self.assertEqual(d[("a", 1)], 4)
            self.assertNotIn(b"b", d)


class BoundLeafMethodTests(unittest.TestCase):
    def test_bound_getitem_on_subclass(self):
        m = Missing({97: "a"})
        get = m.__getitem__
        for _ in range(N):
            self.assertEqual(get(97), "a")
        self.assertEqual(get(98), "<98>")
        self.assertEqual(m[98], "<98>")
        self.assertEqual("".join(map(get, b"abc")), "a<98><99>")
        self.assertEqual(list(map(get, [97, 100])), ["a", "<100>"])

    def test_bound_getitem_raises(self):
        class Plain(dict):
            pass

        get = Plain({1: "x"}).__getitem__
        for _ in range(N // 10):
            self.assertEqual(get(1), "x")
            with self.assertRaises(KeyError):
                get(2)
        with self.assertRaises(KeyError):
            list(map(get, [1, 2]))

    def test_override_wins(self):
        class Over(dict):
            def __getitem__(self, key):
                return "over"

        get = Over({1: "x"}).__getitem__
        for _ in range(N // 10):
            self.assertEqual(get(1), "over")
        self.assertEqual(list(map(get, [1])), ["over"])
        self.assertEqual(list(map(dict.__getitem__.__get__(Over({1: "x"})), [1])), ["x"])


if __name__ == "__main__":
    unittest.main()
