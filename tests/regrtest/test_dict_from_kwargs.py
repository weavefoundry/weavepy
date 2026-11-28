"""`dict(**mapping)` returns the mapping the call built.

The keyword mapping a `CALL_FUNCTION_EX` builds for `dict(**d)` is held by
nothing else, so it becomes the new dict in place of a copy. The result
never aliases the caller's mapping, keeps its order, and non-string keys
still raise.
"""

import unittest

N = 3000


class S(str):
    pass


class DictFromKwargsTests(unittest.TestCase):
    def test_copy_is_independent(self):
        D = {"a": 1, "b": 2}
        for _ in range(N):
            r = dict(**D)
        self.assertEqual(r, D)
        self.assertIsNot(r, D)
        r["c"] = 3
        self.assertNotIn("c", D)

    def test_merges_and_order(self):
        D = {"b": 2, "a": 1}
        E = {"c": 3}
        for _ in range(N):
            r = dict(z=0, **D)
            q = dict(**D, **E)
        self.assertEqual(list(r.items()), [("z", 0), ("b", 2), ("a", 1)])
        self.assertEqual(list(q), ["b", "a", "c"])

    def test_non_string_keys_raise(self):
        for _ in range(100):
            with self.assertRaises(TypeError):
                dict(**{1: 2})

    def test_string_subclass_keys(self):
        k = S("k")
        for _ in range(N):
            r = dict(**{k: 1})
        self.assertEqual(r, {"k": 1})

    def test_subclass_and_positional(self):
        class D2(dict):
            pass

        for _ in range(N):
            a = D2(**{"x": 1})
            b = dict(*([("y", 2)],), **{"x": 1})
        self.assertIs(type(a), D2)
        self.assertEqual(b, {"y": 2, "x": 1})

    def test_empty(self):
        for _ in range(N):
            r = dict(**{})
        self.assertEqual(r, {})


if __name__ == "__main__":
    unittest.main()
