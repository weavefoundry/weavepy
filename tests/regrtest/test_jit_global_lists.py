"""Compiled code reads a module-global list on its list lane.

A global holding a uniform list (all ints, all floats, all instances)
compiles like a list local: loops, comprehensions and reads index it
natively. Each loop here runs long enough to compile; then the list's
contents or the global itself change, and every result must still match
what the interpreter computes.
"""

import unittest

WARM = 3000

INTS = list(range(50))
FLOATS = [i * 0.5 for i in range(20)]


class Box:
    def __init__(self, v):
        self.v = v


BOXES = [Box(i) for i in range(10)]


def doubled():
    return [x * 2 for x in INTS]


def total():
    t = 0
    for x in INTS:
        t += x
    return t


def ftotal():
    t = 0.0
    for x in FLOATS:
        t += x
    return t


def btotal():
    t = 0
    for b in BOXES:
        t += b.v
    return t


def nth(i):
    return INTS[i] + len(INTS)


def run(fn, n, *args):
    r = None
    for _ in range(n):
        r = fn(*args)
    return r


class GlobalListTests(unittest.TestCase):
    def test_comprehension_then_mixed_elements(self):
        global INTS
        saved = INTS
        try:
            self.assertEqual(run(doubled, WARM), [x * 2 for x in range(50)])
            INTS.append("a")
            self.assertEqual(doubled()[-1], "aa")
            INTS = [1.5, 2]
            self.assertEqual(doubled(), [3.0, 4])
            INTS = []
            self.assertEqual(doubled(), [])
        finally:
            INTS = saved
            if INTS and INTS[-1] == "a":
                INTS.pop()

    def test_loop_sees_mutations(self):
        global INTS
        saved = list(INTS)
        try:
            self.assertEqual(run(total, WARM), sum(range(50)))
            INTS[0] = 1000
            self.assertEqual(total(), sum(range(50)) + 1000)
            INTS[1] = 2**70
            self.assertEqual(total(), sum(range(2, 50)) + 1000 + 2**70)
            del INTS[10:]
            self.assertEqual(total(), 1000 + 2**70 + sum(range(2, 10)))
        finally:
            INTS[:] = saved

    def test_float_and_instance_lists(self):
        global FLOATS, BOXES
        self.assertEqual(run(ftotal, WARM), sum(i * 0.5 for i in range(20)))
        FLOATS[3] = 7
        self.assertEqual(ftotal(), sum(i * 0.5 for i in range(20)) - 1.5 + 7)
        self.assertEqual(run(btotal, WARM), sum(range(10)))
        saved = BOXES
        BOXES = BOXES + [None]
        try:
            with self.assertRaises(AttributeError):
                btotal()
        finally:
            BOXES = saved
        self.assertEqual(btotal(), sum(range(10)))

    def test_index_and_len(self):
        global INTS
        self.assertEqual(run(nth, WARM, 3), 3 + 50)
        INTS.append(99)
        try:
            self.assertEqual(nth(-1), 99 + 51)
            with self.assertRaises(IndexError):
                nth(100)
        finally:
            INTS.pop()
        saved = INTS
        INTS = list(range(5))
        try:
            self.assertEqual(nth(4), 4 + 5)
        finally:
            INTS = saved


if __name__ == "__main__":
    unittest.main()
