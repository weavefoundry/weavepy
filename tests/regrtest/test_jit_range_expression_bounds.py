"""A counted range whose bounds are expressions deopts with the right stack.

While `range(len(self.xs), self.n)` evaluates its bounds, the
interpreter's stack holds the erased `range` callee and its null marker
under them. A side exit inside the bounds (an attribute read that misses
its cache, a property, an overflowing bound) must rebuild both below the
values being computed.
"""

import unittest


class Pool:
    def __init__(self, procs, limit):
        self._processes = procs
        self._max_workers = limit

    def spawn(self):
        self._processes[len(self._processes)] = 1

    def launch(self):
        for _ in range(len(self._processes), self._max_workers):
            self.spawn()


class FixedPool(Pool):
    @property
    def _max_workers(self):
        return 2

    @_max_workers.setter
    def _max_workers(self, value):
        pass


class RangeExpressionBoundsTest(unittest.TestCase):
    def test_hot_then_shape_changes(self):
        for _ in range(3000):
            p = Pool({}, 3)
            p.launch()
            self.assertEqual(len(p._processes), 3)
        for procs, want in (({}, 4), ({1: 1}, 1), ({1: 1, 2: 2}, 2)):
            p = Pool(procs, 4)
            p.launch()
            self.assertEqual(len(p._processes), want)
        f = FixedPool({}, 9)
        f.launch()
        self.assertEqual(len(f._processes), 2)
        g = Pool({}, 2**70)
        g._max_workers = 3
        g.launch()
        self.assertEqual(len(g._processes), 3)


if __name__ == "__main__":
    unittest.main()
