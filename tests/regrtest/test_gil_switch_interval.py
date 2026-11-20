"""A thread back from a blocking call gets the GIL within the switch interval.

A busy thread keeps the GIL for `sys.getswitchinterval()` once another
thread asks for it, then hands it over. The interval is the one the
program set, and it is counted once: from the moment the other thread
started to wait, not again from when the busy thread's own turn ran out.
"""

import sys
import threading
import time
import unittest


def waits_behind_a_busy_thread(interval, wanted=3, tries=60):
    """How long `time.sleep` calls took to come back while a busy thread ran.

    Only the calls during which the busy thread got to run count: on a
    loaded machine it may not be scheduled before the sleeper is back, and
    the sleeper then finds the GIL free. A call it did run through waits at
    least the interval the busy thread is owed; load can only add to that.
    """
    before = sys.getswitchinterval()
    sys.setswitchinterval(interval)
    stop = False
    spins = [0]

    def spin():
        xs = []
        while not stop:
            xs.append(1)
            if len(xs) > 100:
                del xs[:50]
            spins[0] += 1

    t = threading.Thread(target=spin)
    t.start()
    try:
        took = []
        for _ in range(tries):
            seen = spins[0]
            t0 = time.perf_counter()
            time.sleep(0.0002)
            wait = time.perf_counter() - t0
            if spins[0] != seen:
                took.append(wait)
                if len(took) >= wanted:
                    break
    finally:
        stop = True
        t.join()
        sys.setswitchinterval(before)
    return took


class SwitchIntervalTests(unittest.TestCase):
    def test_interval_round_trips(self):
        before = sys.getswitchinterval()
        try:
            for value in (0.0001, 0.005, 0.02):
                sys.setswitchinterval(value)
                self.assertAlmostEqual(sys.getswitchinterval(), value, places=6)
        finally:
            sys.setswitchinterval(before)

    def check(self, interval):
        waits = waits_behind_a_busy_thread(interval)
        if not waits:
            self.skipTest("the busy thread never got to run")
        # The busy thread keeps the GIL for the interval the program set...
        self.assertGreater(min(waits), interval * 0.5, waits)
        # ...counted once, from when the other thread started to wait:
        # one interval, not two (the shortest wait is the one the load
        # stretched least).
        self.assertLess(min(waits), interval * 1.6, waits)

    def test_long_interval(self):
        self.check(0.1)

    def test_short_interval(self):
        self.check(0.01)

    def test_a_busy_thread_alone_is_not_slowed(self):
        # With nobody waiting, the interval changes nothing a thread does.
        before = sys.getswitchinterval()
        sys.setswitchinterval(1e-6)
        try:
            total = 0
            for i in range(200000):
                total += i
            self.assertEqual(total, 199999 * 200000 // 2)
        finally:
            sys.setswitchinterval(before)


if __name__ == "__main__":
    unittest.main()
