"""Native container methods called on an expression's result.

`o.items.append(x)`, `d[k].append(v)` and `make().append(x)` call the
method on a receiver the stack holds, not a local; compiled code checks the
receiver's type in line and runs the method's kernel there. The receiver
must then be released as the full call releases it (a temporary dies at
once, its finalizers before the next statement), and every other receiver
must take the general path.
"""

import unittest


class Holder:
    def __init__(self):
        self.items = []
        self.table = {}
        self.seen = set()


class Finalized:
    count = 0

    def __del__(self):
        Finalized.count += 1


class ListSub(list):
    def append(self, value):
        super().append(("sub", value))


class StackReceiverTests(unittest.TestCase):
    ROUNDS = 5000

    def test_attribute_receivers(self):
        h = Holder()
        for i in range(self.ROUNDS):
            h.items.append(i)
            h.seen.add(i % 7)
            h.table.get(i)
        self.assertEqual(len(h.items), self.ROUNDS)
        self.assertEqual(h.items[-1], self.ROUNDS - 1)
        self.assertEqual(h.seen, set(range(7)))

    def test_subscript_receivers(self):
        groups = {k: [] for k in range(5)}
        rows = [[] for _ in range(3)]
        for i in range(self.ROUNDS):
            groups[i % 5].append(i)
            rows[i % 3].append(i)
        self.assertEqual([len(v) for v in groups.values()], [1000] * 5)
        self.assertEqual(sum(len(r) for r in rows), self.ROUNDS)

    def test_temporary_receiver_dies_promptly(self):
        def make():
            return [Finalized()]

        Finalized.count = 0
        for i in range(self.ROUNDS):
            make().append(i)
            self.assertEqual(Finalized.count, i + 1)

    def test_receiver_type_changes(self):
        h = Holder()
        values = [[], ListSub(), (), [], "str"]
        out = []
        for i in range(self.ROUNDS):
            h.items = values[i % len(values)]
            try:
                h.items.append(i)
            except AttributeError:
                out.append("no-append")
        self.assertEqual(values[1][:2], [("sub", 1), ("sub", 6)])
        self.assertEqual(len(values[0]), self.ROUNDS // 5)
        self.assertEqual(out.count("no-append"), 2 * self.ROUNDS // 5)

    def test_results_kept(self):
        d = {"a": 1}
        h = Holder()
        h.table = d
        got = []
        for _ in range(self.ROUNDS):
            got.append(h.table.get("a"))
            got.append(h.table.get("b", 2))
        self.assertEqual(got[:4], [1, 2, 1, 2])
        s = "abc"
        h.text = s
        for _ in range(self.ROUNDS):
            self.assertTrue(h.text.startswith("a"))
            self.assertEqual(h.text.upper(), "ABC")

    def test_small_int_and_const_arguments(self):
        h = Holder()
        for i in range(self.ROUNDS):
            h.items.append(3)
            h.items.append("x")
        self.assertEqual(h.items[:4], [3, "x", 3, "x"])
        h.items.clear()
        for i in range(100):
            h.items.insert(0, i)
        self.assertEqual(h.items[:3], [99, 98, 97])


if __name__ == "__main__":
    unittest.main()
