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


def waits_behind_a_busy_thread(interval, samples):
    """The shortest of several `time.sleep` calls' returns, a busy thread running.

    (The shortest: a loaded machine can only make a thread wait longer for
    its turn, never less than the interval the busy thread is owed.)
    """
    before = sys.getswitchinterval()
    sys.setswitchinterval(interval)
    stop = False

    def spin():
        xs = []
        while not stop:
            xs.append(1)
            if len(xs) > 100:
                del xs[:50]

    t = threading.Thread(target=spin)
    t.start()
    try:
        took = []
        for _ in range(samples):
            t0 = time.perf_counter()
            time.sleep(0.0002)
            took.append(time.perf_counter() - t0)
    finally:
        stop = True
        t.join()
        sys.setswitchinterval(before)
    return min(took)


class SwitchIntervalTests(unittest.TestCase):
    def test_interval_round_trips(self):
        before = sys.getswitchinterval()
        try:
            for value in (0.0001, 0.005, 0.02):
                sys.setswitchinterval(value)
                self.assertAlmostEqual(sys.getswitchinterval(), value, places=6)
        finally:
            sys.setswitchinterval(before)

    def test_shorter_interval_shorter_wait(self):
        long = waits_behind_a_busy_thread(0.1, 5)
        short = waits_behind_a_busy_thread(0.002, 40)
        # The waits track the interval (about 100 ms and about 2 ms here).
        self.assertLess(short, long / 3, (short, long))
        self.assertGreater(long, 0.05, long)

    def test_one_interval_not_two(self):
        # The busy thread's turn and the waiter's wait run side by side:
        # a wait is one interval, not two.
        interval = 0.1
        wait = waits_behind_a_busy_thread(interval, 6)
        self.assertGreater(wait, interval * 0.5, wait)
        self.assertLess(wait, interval * 1.6, wait)

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
