"""`_queue.SimpleQueue` wakes one parked getter per queued item.

Only the first `put` of a burst finds the wake gate held, so each getter
has to pass the gate on as it pops. A getter that doesn't leaves the others
parked over a non-empty queue: `multiprocessing.pool.ThreadPool.terminate()`
queues its stop sentinels that way and then hung in `join()`.
"""

import _queue
import threading
import time
import unittest
from multiprocessing.pool import ThreadPool

JOIN_TIMEOUT = 30.0


class WakeupTests(unittest.TestCase):
    def park_getters(self, queue, count):
        results = []

        def getter():
            results.append(queue.get())

        threads = [threading.Thread(target=getter, daemon=True) for _ in range(count)]
        for thread in threads:
            thread.start()
        # Let them all reach the gate.
        time.sleep(0.05)
        return threads, results

    def test_burst_of_puts_wakes_every_parked_getter(self):
        queue = _queue.SimpleQueue()
        threads, results = self.park_getters(queue, 5)
        for item in range(5):
            queue.put(item)
        for thread in threads:
            thread.join(JOIN_TIMEOUT)
        self.assertFalse([t for t in threads if t.is_alive()])
        self.assertEqual(sorted(results), list(range(5)))
        self.assertTrue(queue.empty())

    def test_gate_works_after_a_burst(self):
        queue = _queue.SimpleQueue()
        threads, _ = self.park_getters(queue, 2)
        queue.put(0)
        queue.put(1)
        for thread in threads:
            thread.join(JOIN_TIMEOUT)
        threads, results = self.park_getters(queue, 1)
        queue.put(2)
        threads[0].join(JOIN_TIMEOUT)
        self.assertFalse(threads[0].is_alive())
        self.assertEqual(results, [2])

    def test_timed_get_still_times_out(self):
        queue = _queue.SimpleQueue()
        with self.assertRaises(_queue.Empty):
            queue.get(timeout=0.01)
        queue.put(1)
        self.assertEqual(queue.get(timeout=0.01), 1)
        with self.assertRaises(_queue.Empty):
            queue.get(block=False)

    def test_competing_getters_lose_no_items(self):
        queue = _queue.SimpleQueue()
        count = 2000
        results = []
        errors = []

        def getter():
            try:
                while True:
                    item = queue.get(timeout=JOIN_TIMEOUT)
                    if item is None:
                        return
                    results.append(item)
            except BaseException as e:
                errors.append(e)

        threads = [threading.Thread(target=getter, daemon=True) for _ in range(4)]
        for thread in threads:
            thread.start()
        for item in range(count):
            queue.put(item)
        for _ in threads:
            queue.put(None)
        for thread in threads:
            thread.join(JOIN_TIMEOUT * 2)
        self.assertEqual(errors, [])
        self.assertEqual(sorted(results), list(range(count)))

    def test_thread_pool_terminate_and_join(self):
        # test_multiprocessing's WithThreadsTestPool.test_async_timeout.
        for _ in range(5):
            pool = ThreadPool(3)
            event = threading.Event()
            pool.apply_async(event.wait, (JOIN_TIMEOUT,))
            event.set()
            pool.terminate()
            joiner = threading.Thread(target=pool.join, daemon=True)
            joiner.start()
            joiner.join(JOIN_TIMEOUT)
            self.assertFalse(joiner.is_alive(), "ThreadPool.join() hung")


if __name__ == "__main__":
    unittest.main()
