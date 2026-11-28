"""`zip` and `enumerate` over dict iterators, stepped in place.

After a dict iterator's first (checked) step, adapters step it without
the general path while its dict is unchanged. Items, order, the end, and
the size-change and key-change errors behave as before.
"""

import collections
import unittest

N = 3000


class ZipDictIteratorTests(unittest.TestCase):
    def test_items_keys_values(self):
        d = {w: i for i, w in enumerate("the quick brown fox jumps".split())}
        for _ in range(N):
            it = iter(d.items())
            head = list(zip(range(3), it))
            rest = list(it)
            z = list(zip(d, d.values()))
            e = list(enumerate(d.keys()))
        self.assertEqual(head, [(0, ("the", 0)), (1, ("quick", 1)), (2, ("brown", 2))])
        self.assertEqual(rest, [("fox", 3), ("jumps", 4)])
        self.assertEqual(z, [("the", 0), ("quick", 1), ("brown", 2), ("fox", 3), ("jumps", 4)])
        self.assertEqual(e[-1], (4, "jumps"))

    def test_size_change(self):
        for _ in range(N // 10):
            d = {1: 1, 2: 2, 3: 3}
            it = iter(d)
            with self.assertRaisesRegex(RuntimeError, "changed size"):
                for i, k in zip(range(10), it):
                    if i == 1:
                        d[9] = 9

    def test_key_change(self):
        for _ in range(N // 10):
            d = {1: 1, 2: 2, 3: 3}
            it = iter(d.items())
            with self.assertRaisesRegex(RuntimeError, "keys changed"):
                for i, kv in zip(range(10), it):
                    if i == 0:
                        del d[1]
                        d[5] = 5

    def test_value_change_and_ordered(self):
        d = {1: "a", 2: "b"}
        it = iter(d.values())
        out = []
        for i, v in zip(range(5), it):
            out.append(v)
            d[2] = "z"
        self.assertEqual(out, ["a", "z"])
        od = collections.OrderedDict(a=1, b=2, c=3)
        od.move_to_end("a")
        for _ in range(N // 10):
            r = list(zip(range(5), od.items()))
        self.assertEqual(r, [(0, ("b", 2)), (1, ("c", 3)), (2, ("a", 1))])


if __name__ == "__main__":
    unittest.main()
