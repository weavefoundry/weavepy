"""Thread handles live as long as their threads and `Thread` objects.

The VM's registry of `_thread._ThreadHandle` states holds them weakly, so
`threading.Thread` objects that come and go leave nothing behind; handles
of started threads still join, report `is_alive()`, and survive their
`Thread` object being dropped while the thread runs.
"""

import gc
import threading
import time
import unittest


class ThreadHandleLifetimeTests(unittest.TestCase):
    def test_many_threads(self):
        out = []
        for i in range(500):
            t = threading.Thread(target=out.append, args=(i,))
            t.start()
            t.join()
            self.assertFalse(t.is_alive())
        self.assertEqual(out, list(range(500)))
        unstarted = [threading.Thread(target=None) for _ in range(2000)]
        del unstarted
        gc.collect()

    def test_handle_outlives_thread_object(self):
        done = threading.Event()
        release = threading.Event()

        def work():
            release.wait(5)
            done.set()

        t = threading.Thread(target=work)
        t.start()
        handle = t._os_thread_handle
        del t
        gc.collect()
        self.assertFalse(handle.is_done())
        release.set()
        handle.join()
        self.assertTrue(handle.is_done())
        self.assertTrue(done.is_set())

    def test_concurrent_threads(self):
        results = []
        lock = threading.Lock()

        def work(n):
            time.sleep(0.001)
            with lock:
                results.append(n)

        ts = [threading.Thread(target=work, args=(i,)) for i in range(20)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        self.assertEqual(sorted(results), list(range(20)))
        self.assertTrue(all(not t.is_alive() for t in ts))
        self.assertEqual(threading.active_count(), 1)


if __name__ == "__main__":
    unittest.main()
