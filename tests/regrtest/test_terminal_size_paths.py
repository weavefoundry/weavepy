"""The paths behind `shutil.get_terminal_size()`.

A struct sequence built from a tuple or list of numbers, strings and
`None` is made directly; other values, lengths the type rejects, and types
that refuse instances behave as before. A missing key of a dict keyed by
`bytes` (`os.environ`'s data) raises its `KeyError` in compiled code, and
`os.get_terminal_size()` raising `OSError` in a loop is caught there.
"""

import os
import shutil
import sys
import time
import unittest

N = 3000


class StructSequenceConstructionTests(unittest.TestCase):
    def test_from_tuple_and_list(self):
        for i in range(N):
            a = os.terminal_size((80, i))
            b = os.terminal_size([i, 24])
        self.assertEqual((a.columns, a.lines, tuple(a)), (80, N - 1, (80, N - 1)))
        self.assertEqual((b.columns, b.lines), (N - 1, 24))
        self.assertIs(type(a), os.terminal_size)
        self.assertTrue(isinstance(a, tuple))

    def test_hidden_fields(self):
        for _ in range(N):
            t9 = time.struct_time((2020, 1, 2, 3, 4, 5, 6, 7, 0))
            t11 = time.struct_time((2020, 1, 2, 3, 4, 5, 6, 7, 0, "UTC", 0))
        self.assertEqual(len(t9), 9)
        self.assertEqual((t9.tm_zone, t9.tm_gmtoff), (None, None))
        self.assertEqual(len(t11), 9)
        self.assertEqual((t11.tm_zone, t11.tm_gmtoff), ("UTC", 0))
        self.assertEqual(t11[:3], (2020, 1, 2))

    def test_other_values(self):
        items = []
        for _ in range(500):
            ts = os.terminal_size((items, {"k": 1}))
        self.assertIs(ts.columns, items)
        self.assertEqual(ts.lines, {"k": 1})
        ts = os.terminal_size(iter((1, 2)))
        self.assertEqual(tuple(ts), (1, 2))

    def test_rejected(self):
        for _ in range(500):
            with self.assertRaises(TypeError):
                os.terminal_size((1, 2, 3))
            with self.assertRaises(TypeError):
                os.terminal_size([1])
            with self.assertRaises(TypeError):
                time.struct_time((1,) * 12)
            with self.assertRaises(TypeError):
                type(sys.flags)((0,) * len(sys.flags))
            with self.assertRaises(TypeError):
                type(sys.version_info)((3, 14, 0, "final", 0))


class EnvironAndTerminalTests(unittest.TestCase):
    def test_missing_environ_keys(self):
        e = os.environ
        hits = misses = 0
        for i in range(N):
            try:
                e["WEAVEPY_NO_SUCH_VARIABLE_%d" % (i % 3)]
                hits += 1
            except KeyError as exc:
                misses += 1
                last = exc
        self.assertEqual((hits, misses), (0, N))
        self.assertEqual(last.args, ("WEAVEPY_NO_SUCH_VARIABLE_%d" % ((N - 1) % 3),))
        self.assertIsNone(last.__cause__)
        self.assertTrue(last.__suppress_context__)

    def test_bytes_keyed_dict(self):
        d = {b"a": 1, b"b": 2}
        out = []
        for i in range(N):
            k = b"a" if i % 2 else b"zz"
            try:
                out.append(d[k])
            except KeyError as exc:
                out.append(exc.args[0])
        self.assertEqual(out[:2], [b"zz", 1])

    def test_get_terminal_size(self):
        r, w = os.pipe()
        try:
            errors = 0
            for _ in range(N):
                try:
                    os.get_terminal_size(w)
                except OSError as exc:
                    errors += 1
                    last = exc
            self.assertEqual(errors, N)
            self.assertIsNotNone(last.errno)
        finally:
            os.close(r)
            os.close(w)
        for _ in range(500):
            size = shutil.get_terminal_size((81, 25))
        self.assertIsInstance(size, os.terminal_size)
        self.assertEqual(len(size), 2)


if __name__ == "__main__":
    unittest.main()
