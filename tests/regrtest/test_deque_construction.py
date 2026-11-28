"""`deque()` and `deque(xs)` of a list or tuple build in place.

The exact class's plain constructions lay the instance out as its
`__init__` does; a `maxlen`, any other iterable, and subclasses keep the
Python constructor, and the result behaves as a deque throughout.
"""

import copy
import pickle
import unittest
from collections import deque

N = 3000


class D(deque):
    pass


class DequeConstructionTests(unittest.TestCase):
    def test_shapes(self):
        L = [1, 2, 3]
        for _ in range(N):
            a = deque()
            b = deque(L)
            c = deque((4, 5))
            d = deque("xy")
            e = deque(L, 2)
        a.append(1)
        b.appendleft(0)
        self.assertEqual((list(a), list(b), list(c), list(d), list(e)), ([1], [0, 1, 2, 3], [4, 5], ["x", "y"], [2, 3]))
        self.assertIsNone(b.maxlen)
        self.assertEqual(e.maxlen, 2)
        L.append(4)
        self.assertEqual(list(b), [0, 1, 2, 3])
        self.assertEqual(list(reversed(b)), [3, 2, 1, 0])
        self.assertIs(type(D([1])), D)

    def test_round_trips(self):
        b = deque([1, 2])
        self.assertEqual(copy.copy(b), b)
        self.assertEqual(pickle.loads(pickle.dumps(b)), b)
        self.assertEqual(repr(deque()), "deque([])")


if __name__ == "__main__":
    unittest.main()
