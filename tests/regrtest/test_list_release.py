"""Releasing a list of scalars keeps every other owner's view intact.

The last owner of a list whose items are all scalars frees it without
releasing the items one at a time. Only the last owner may: a list still
shared, whether reference counts are biased to one thread or shared
because other threads run, keeps its items. Each case below drops one
reference through a container (a dict, a tuple, a list, a generator).
"""

import threading
import unittest


# Module-level code: a name's rebinding or deletion releases its old
# value through the owner's drop.
SCENARIO = """
a = [1, 2.5, None, True]
b = a
d = {"k": a}
del d
results.append(list(b))
t = (a,)
del t
results.append(list(b))
holder = [a]
del holder
results.append(list(b))
nested = [[1, 2], [3, 4]]
keep = nested[0]
del nested
results.append(list(keep))
g = (x for x in [a])
next(g)
del g
results.append(list(b))
"""

EXPECTED = [[1, 2.5, None, True]] * 3 + [[1, 2], [1, 2.5, None, True]]


class ListReleaseTest(unittest.TestCase):
    def check_survives(self):
        results = []
        exec(SCENARIO, {"results": results})
        self.assertEqual(results, EXPECTED)

    def test_single_thread(self):
        self.check_survives()

    def test_with_threads_running(self):
        stop = threading.Event()
        worker = threading.Thread(target=stop.wait)
        worker.start()
        try:
            self.check_survives()
        finally:
            stop.set()
            worker.join()

    def test_comprehension_results(self):
        rows = [[i * j for j in range(10)] for i in range(10)]
        firsts = [row for row in rows]
        del rows
        self.assertEqual(firsts[3], [3 * j for j in range(10)])


if __name__ == "__main__":
    unittest.main()
