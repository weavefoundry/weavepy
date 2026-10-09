"""Subscripts and membership tests on containers held in globals.

A compiled loop reads `container[key]` and tests `key in container` on
module-level dicts, sets, lists and tuples in place when no Python code
decides the answer. A key only a user `__eq__` could match, a missing
key, and an index out of range must still behave as the interpreter's
full path does.
"""

import unittest

TABLE = {"k%d" % i: i for i in range(20)}
NUMS = {i: i * i for i in range(50)}
SEEN = set(range(100))
ITEMS = list(range(10))
PAIR = (7, 8)


class Twin:
    """Hashes and compares equal to the int 3."""

    def __hash__(self):
        return hash(3)

    def __eq__(self, other):
        return other == 3


def table_sum(n):
    total = 0
    for _ in range(n):
        total += TABLE["k5"] + NUMS[7] + ITEMS[-1] + PAIR[1]
    return total


def membership(n):
    hits = 0
    for i in range(n):
        if (i & 127) in SEEN:
            hits += 1
        if "k3" in TABLE:
            hits += 1
    return hits


def lookup_missing(n):
    caught = 0
    for i in range(n):
        try:
            TABLE["nope"]
        except KeyError:
            caught += 1
    return caught


class DynamicContainerReadTest(unittest.TestCase):
    def test_reads(self):
        self.assertEqual(table_sum(3000), 3000 * (5 + 49 + 9 + 8))

    def test_membership(self):
        n = 3000
        expected = sum(1 for i in range(n) if (i & 127) < 100) + n
        self.assertEqual(membership(n), expected)

    def test_missing_key_raises(self):
        self.assertEqual(lookup_missing(2000), 2000)

    def test_user_eq_key(self):
        global SEEN
        saved = SEEN
        try:
            SEEN = {Twin()}
            self.assertEqual(membership(500), sum(1 for i in range(500) if i & 127 == 3) + 500)
        finally:
            SEEN = saved

    def test_out_of_range(self):
        def read(n):
            out = 0
            for i in range(n):
                out += ITEMS[i]
            return out

        self.assertEqual(read(10), 45)
        with self.assertRaises(IndexError):
            read(11)


if __name__ == "__main__":
    unittest.main()
