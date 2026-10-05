"""A long compiled loop bounds how many dead objects its pins keep alive.

Compiled code pins the objects it handles. A loop over objects something
else keeps alive (a list's elements) may hold many pins; a loop whose pins
are the last references must leave and release them every few thousand,
so finalizers run with a bounded delay.
"""

import unittest


class Probe:
    alive = 0

    def __init__(self):
        Probe.alive += 1

    def __del__(self):
        Probe.alive -= 1

    def ping(self):
        return 1


def make():
    return Probe()


def churn(n, peaks):
    t = 0
    for i in range(n):
        p = make()
        t += p.ping()
        if i % 1000 == 0:
            peaks.append(Probe.alive)
    return t


def walk(items):
    t = 0
    for p in items:
        t += p.ping()
    return t


class PinLimitTests(unittest.TestCase):
    def test_dead_objects_are_released_as_the_loop_runs(self):
        peaks = []
        self.assertEqual(churn(40000, peaks), 40000)
        # The soft limit is a few thousand pins; never most of the loop.
        self.assertLess(max(peaks), 12000)
        self.assertEqual(Probe.alive, 0)

    def test_shared_objects_stay_pinned_correctly(self):
        items = [Probe() for _ in range(40000)]
        self.assertEqual(walk(items), 40000)
        self.assertEqual(walk(items[::-1]), 40000)
        self.assertEqual(Probe.alive, 40000)
        del items
        self.assertEqual(Probe.alive, 0)


if __name__ == "__main__":
    unittest.main()
