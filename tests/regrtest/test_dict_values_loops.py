"""`for v in d.values()` in compiled loops.

Values of every kind come back in insertion order, ints and heap objects
alike; a dict that changes size during the loop, or loses and regains its
only key, raises `RuntimeError` as CPython does; a value replaced in place mid-loop is
seen; `OrderedDict`, dict subclasses and reversed views keep their own
behavior. Loops run past the compile threshold.
"""

import collections
import unittest


class ValuesLoopTests(unittest.TestCase):
    def test_values(self):
        d = {("k%d" % i): i for i in range(50)}
        objs = {i: [i] for i in range(20)}
        for _ in range(300):
            total = 0
            for v in d.values():
                total += v
            self.assertEqual(total, sum(range(50)))
            got = []
            for v in objs.values():
                got.append(v[0])
            self.assertEqual(got, list(range(20)))

    def test_mixed_values(self):
        d = {"a": 1, "b": "two", "c": None, "d": 2**70, "e": 1.5}
        for _ in range(300):
            out = []
            for v in d.values():
                out.append(v)
            self.assertEqual(out, [1, "two", None, 2**70, 1.5])

    def test_size_change(self):
        for _ in range(100):
            d = {i: i for i in range(10)}
            with self.assertRaises(RuntimeError):
                for v in d.values():
                    if v == 3:
                        d[100] = 0
            d = {i: i for i in range(10)}
            with self.assertRaises(RuntimeError):
                for v in d.values():
                    if v == 3:
                        del d[0]

    def test_keys_changed_same_size(self):
        # (CPython's own case, test_dict's test_mutating_iteration_delete.)
        for _ in range(100):
            d = {0: 0}
            with self.assertRaises(RuntimeError):
                for v in d.values():
                    del d[0]
                    d[0] = 0

    def test_value_replaced(self):
        for _ in range(100):
            d = {i: i for i in range(5)}
            seen = []
            for v in d.values():
                seen.append(v)
                if v == 1:
                    d[3] = "new"
            self.assertEqual(seen, [0, 1, 2, "new", 4])

    def test_other_mappings(self):
        od = collections.OrderedDict((i, -i) for i in range(10))
        od.move_to_end(0)

        class Sub(dict):
            pass

        s = Sub(a=1, b=2)
        for _ in range(200):
            self.assertEqual([v for v in od.values()], [-i for i in range(1, 10)] + [0])
            out = []
            for v in s.values():
                out.append(v)
            self.assertEqual(out, [1, 2])
            out = []
            for v in reversed({1: "x", 2: "y"}.values()):
                out.append(v)
            self.assertEqual(out, ["y", "x"])

    def test_empty_and_exhausted(self):
        it = iter({}.values())
        self.assertEqual(list(it), [])
        d = {1: 2}
        it = iter(d.values())
        for _ in it:
            pass
        d[5] = 6
        self.assertEqual(list(it), [])


if __name__ == "__main__":
    unittest.main()
