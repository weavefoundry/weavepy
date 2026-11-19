"""Field reads, deletions and comparisons of struct sequences.

`st.st_mode`, `time.localtime().tm_year` and `sys.version_info.major`
read the instance's field as a plain attribute read does; deleting or
assigning a field fails with CPython's "readonly attribute"; a struct
sequence compares with a tuple or one of its own type as its tuple of
visible fields does. Hidden fields, unknown names, a struct sequence
built from a sequence, and receivers of other classes at the same site
behave as before.
"""

import os
import sys
import time
import unittest

N = 3000


class Plain:
    def __init__(self):
        self.st_mode = "plain"
        self.tm_year = "plain"


class StructSequenceFieldTests(unittest.TestCase):
    def test_reads(self):
        st = os.stat(".")
        lt = time.localtime(0)
        ts = os.terminal_size((80, 24))
        vi = sys.version_info
        for _ in range(N):
            r = (st.st_mode, st.st_size, st.st_mtime, lt.tm_year, lt.tm_zone,
                 ts.columns, ts.lines, vi.major, vi.minor)
        self.assertEqual(r[0], st[0])
        self.assertEqual(r[1], st[6])
        self.assertIsInstance(r[2], float)
        self.assertEqual(r[3], lt[0])
        self.assertEqual(r[4], time.localtime(0).tm_zone)
        self.assertEqual(r[5:7], (80, 24))
        self.assertEqual(r[7:], (vi[0], vi[1]))

    def test_mixed_receivers(self):
        items = [os.stat("."), Plain(), os.stat(os.__file__)]
        out = []
        for i in range(N):
            out.append(items[i % 3].st_mode)
        self.assertEqual(out[:3], [items[0][0], "plain", items[2][0]])
        self.assertEqual(out[-3:], out[:3])

    def test_unset_hidden_field(self):
        t = time.struct_time((2020, 1, 2, 3, 4, 5, 6, 7, 0))
        for _ in range(N):
            r = (t.tm_year, t.tm_zone, t.tm_gmtoff)
        self.assertEqual(r, (2020, None, None))

    def test_unknown_name(self):
        st = os.stat(".")
        for _ in range(500):
            with self.assertRaises(AttributeError):
                st.no_such_field
        self.assertFalse(hasattr(st, "no_such_field"))

    def test_delete_and_assign(self):
        st = os.stat(".")
        mode = st.st_mode
        for _ in range(500):
            with self.assertRaisesRegex(AttributeError, "readonly attribute"):
                del st.st_mode
            with self.assertRaisesRegex(AttributeError, "readonly attribute"):
                st.st_mode = 1
            with self.assertRaises(AttributeError):
                del st.no_such_field
        self.assertEqual(st.st_mode, mode)
        lt = time.localtime(0)
        with self.assertRaisesRegex(AttributeError, "readonly attribute"):
            del lt.tm_year
        self.assertEqual(lt.tm_year, time.localtime(0).tm_year)

    def test_compare(self):
        vi = sys.version_info
        t = tuple(vi)
        for _ in range(N):
            r = (vi >= (3, 8), vi < (3, 8), (3, 8) <= vi, vi == t, t == vi,
                 vi != t, vi > (vi[0], vi[1]), vi == sys.version_info)
        self.assertEqual(r, (True, False, True, True, True, False, True, True))
        a = os.terminal_size((1, 2))
        b = os.terminal_size((1, 3))
        self.assertTrue(a < b)
        self.assertTrue(a == (1, 2))
        self.assertFalse(a == b)
        self.assertEqual(hash(a), hash((1, 2)))
        self.assertFalse(a == "x")
        with self.assertRaises(TypeError):
            a < "x"
        # Different struct sequence types compare by their own methods.
        self.assertNotEqual(os.terminal_size((1, 2)), time.localtime(0))

    def test_from_sequence(self):
        ts = os.terminal_size([5, 6])
        for _ in range(N):
            r = (ts.columns, ts.lines, ts == (5, 6))
        self.assertEqual(r, (5, 6, True))


if __name__ == "__main__":
    unittest.main()
