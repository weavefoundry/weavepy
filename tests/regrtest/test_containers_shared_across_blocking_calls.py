"""A container one thread is using while it blocks stays usable from others.

A comparison or a `repr` that a container operation calls can block (a
sleep, a lock, a write), which gives the GIL to another thread in the
middle of that operation. The other thread's operations on the same
container then finish, with the results CPython gives: none of them
hangs waiting for the first thread while holding the GIL, and none is
lost.
"""

import collections
import threading
import time
import unittest

PAUSE = 0.15
entered = threading.Event()


class Slow:
    """Hashes alike with every other `Slow`; equality and `repr` block."""

    def __repr__(self):
        entered.set()
        time.sleep(PAUSE)
        return "slow"

    def __hash__(self):
        return 1

    def __eq__(self, other):
        entered.set()
        time.sleep(PAUSE)
        return self is other


def slow_key(v):
    entered.set()
    time.sleep(PAUSE)
    return v


class BlockedOperationTests(unittest.TestCase):
    def both(self, holder, poke):
        """`holder()` in one thread and, once it blocks, `poke()` in another."""
        entered.clear()
        results = {}

        def a():
            try:
                results["holder"] = holder()
            except Exception as e:
                results["holder"] = type(e)

        def b():
            self.assertTrue(entered.wait(10))
            try:
                results["poke"] = poke()
            except Exception as e:
                results["poke"] = type(e)

        ta = threading.Thread(target=a, daemon=True)
        tb = threading.Thread(target=b, daemon=True)
        ta.start()
        tb.start()
        ta.join(30)
        tb.join(30)
        self.assertFalse(ta.is_alive(), "the blocked operation never finished")
        self.assertFalse(tb.is_alive(), "the other thread's operation never finished")
        return results

    def test_set_add_during_a_membership_test(self):
        k1, k2 = Slow(), Slow()
        s = {k1}
        r = self.both(lambda: k2 in s, lambda: s.add(5))
        self.assertEqual(r, {"holder": False, "poke": None})
        self.assertEqual(len(s), 2)
        self.assertIn(5, s)

    def test_set_discard_and_update_during_a_membership_test(self):
        k1, k2 = Slow(), Slow()
        s = {k1, 7}
        r = self.both(lambda: k2 in s, lambda: s.discard(7))
        self.assertEqual(r["holder"], False)
        self.assertNotIn(7, s)
        r = self.both(lambda: k2 in s, lambda: s.add("x"))
        self.assertIn("x", s)
        self.assertEqual(len(s), 2)

    def test_several_threads_add_during_one_membership_test(self):
        k1, k2 = Slow(), Slow()
        s = {k1}
        entered.clear()
        found = []
        holder = threading.Thread(target=lambda: found.append(k2 in s), daemon=True)
        holder.start()
        self.assertTrue(entered.wait(10))
        adders = [threading.Thread(target=s.add, args=(i,), daemon=True) for i in range(8)]
        for t in adders:
            t.start()
        for t in adders + [holder]:
            t.join(30)
            self.assertFalse(t.is_alive())
        self.assertEqual(found, [False])
        self.assertEqual(len(s), 9)
        self.assertTrue(all(i in s for i in range(8)))

    def test_list_operations_during_a_count(self):
        k1, k2 = Slow(), Slow()
        lst = [k1, 2]
        r = self.both(lambda: lst.count(k2), lambda: lst.append(9))
        self.assertEqual(r, {"holder": 0, "poke": None})
        self.assertEqual(lst[1:], [2, 9])
        r = self.both(lambda: lst.index(2), lambda: lst.pop())
        self.assertEqual(r, {"holder": 1, "poke": 9})
        self.assertEqual(len(lst), 2)

    def test_list_length_and_append_during_a_repr(self):
        lst = [Slow(), 2, 3]
        r = self.both(lambda: repr(lst), lambda: len(lst))
        self.assertEqual(r, {"holder": "[slow, 2, 3]", "poke": 3})
        r = self.both(lambda: repr(lst)[:6], lambda: lst.append(4))
        self.assertEqual(r["holder"], "[slow,")
        self.assertEqual(lst[1:], [2, 3, 4])

    def test_dict_stores_during_a_lookup_and_a_store(self):
        k1, k2 = Slow(), Slow()
        d = {k1: 1}
        r = self.both(lambda: d.get(k2), lambda: d.__setitem__("x", 3))
        self.assertEqual(r, {"holder": None, "poke": None})
        self.assertEqual((len(d), d["x"]), (2, 3))
        r = self.both(lambda: d.__setitem__(k2, 2), lambda: d.__setitem__("y", 4))
        self.assertEqual((len(d), d["y"]), (4, 4))
        r = self.both(lambda: d.get(Slow()), lambda: len(d))
        self.assertEqual(r["poke"], 4)

    def test_deque_append_during_a_count(self):
        k1, k2 = Slow(), Slow()
        dq = collections.deque([k1, 2])
        r = self.both(lambda: dq.count(k2), lambda: dq.append(7))
        self.assertIs(r["holder"], RuntimeError)
        self.assertEqual(list(dq)[1:], [2, 7])

    def test_list_append_during_sorts(self):
        lst = [3, 1, 2]
        r = self.both(lambda: sorted(lst, key=slow_key), lambda: lst.append(0))
        self.assertEqual(r["holder"], [1, 2, 3])
        self.assertEqual(lst, [3, 1, 2, 0])
        r = self.both(lambda: lst.sort(key=slow_key), lambda: lst.append(-1))
        self.assertIs(r["holder"], ValueError)

    def test_a_busy_thread_beside_a_blocked_operation(self):
        # The blocked thread gets the GIL back from a thread that never
        # blocks, though it holds its container the whole time.
        k1, k2 = Slow(), Slow()
        s = {k1}
        stop = False
        spins = [0]

        def spin():
            while not stop:
                spins[0] += 1

        t = threading.Thread(target=spin, daemon=True)
        t.start()
        try:
            t0 = time.monotonic()
            for _ in range(5):
                self.assertFalse(k2 in s)
            took = time.monotonic() - t0
        finally:
            stop = True
            t.join(30)
        self.assertFalse(t.is_alive())
        self.assertGreater(spins[0], 0)
        self.assertLess(took, 20)


if __name__ == "__main__":
    unittest.main()
