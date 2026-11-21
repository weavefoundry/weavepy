"""`deque.rotate`, `extend` and `extendleft` over long deques.

A rotation moves its items as one block, with room made at the front
for the next few rotations at once; `extend` and `extendleft` of a list
or tuple take its items in at once, trimming to `maxlen` as item-by-item
appends would. Every result matches a list model, through any mix of
rotations, extensions and pops, with and without `maxlen`.
"""

import collections
import random
import time
import unittest


def rotated(items, k):
    if not items:
        return items
    k %= len(items)
    return items[-k:] + items[:-k] if k else list(items)


class RotateTests(unittest.TestCase):
    def test_rotations_match_a_list(self):
        for n in (0, 1, 2, 7, 100, 1001):
            for k in (0, 1, -1, 2, -3, n // 3, -(n // 3), n // 2, n - 1, n, n + 5, -(2 * n) - 1, 10**9):
                with self.subTest(n=n, k=k):
                    d = collections.deque(range(n))
                    d.rotate(k)
                    self.assertEqual(list(d), rotated(list(range(n)), k))
                    d.rotate(-k)
                    self.assertEqual(list(d), list(range(n)))

    def test_repeated_rotations(self):
        d = collections.deque(range(4000))
        model = list(range(4000))
        for step in (1333, -1333, 1, -1, 1999, -2001, 4000, 7):
            for _ in range(13):
                d.rotate(step)
                model = rotated(model, step)
            self.assertEqual(list(d), model)
            self.assertEqual(d[0], model[0])
            self.assertEqual(d[-1], model[-1])
            self.assertEqual(len(d), 4000)

    def test_rotation_keeps_items_alive_and_unique(self):
        items = [object() for _ in range(300)]
        d = collections.deque(items)
        for _ in range(50):
            d.rotate(97)
        self.assertEqual(sorted(map(id, d)), sorted(map(id, items)))
        d.rotate(-(97 * 50) % 300)
        self.assertTrue(all(a is b for a, b in zip(d, items)))

    def test_rotation_with_maxlen_and_after_pops(self):
        d = collections.deque(range(10), maxlen=10)
        d.rotate(3)
        self.assertEqual(list(d), [7, 8, 9, 0, 1, 2, 3, 4, 5, 6])
        d.append(10)
        self.assertEqual(list(d), [8, 9, 0, 1, 2, 3, 4, 5, 6, 10])
        d.appendleft(-1)
        self.assertEqual(list(d), [-1, 8, 9, 0, 1, 2, 3, 4, 5, 6])
        for _ in range(4):
            d.popleft()
        d.rotate(-2)
        self.assertEqual(list(d), [3, 4, 5, 6, 1, 2])

    def test_rotation_during_iteration_is_an_error(self):
        d = collections.deque(range(10))
        it = iter(d)
        next(it)
        d.rotate(2)
        with self.assertRaises(RuntimeError):
            next(it)


class ExtendTests(unittest.TestCase):
    def test_extend_and_extendleft_match_a_list(self):
        for src in ([], [1], [1, 2, 3], (4, 5), tuple(range(50)), list(range(1000))):
            with self.subTest(n=len(src)):
                d = collections.deque([0])
                d.extend(src)
                self.assertEqual(list(d), [0] + list(src))
                d = collections.deque([0])
                d.extendleft(src)
                self.assertEqual(list(d), list(reversed(src)) + [0])

    def test_maxlen_trims_as_appends_would(self):
        for maxlen in (0, 1, 3, 10):
            for src in ([1, 2, 3], tuple(range(25)), []):
                with self.subTest(maxlen=maxlen, n=len(src)):
                    d = collections.deque(range(5), maxlen=maxlen)
                    d.extend(src)
                    want = collections.deque(range(5), maxlen=maxlen)
                    for x in src:
                        want.append(x)
                    self.assertEqual(list(d), list(want))
                    d = collections.deque(range(5), maxlen=maxlen)
                    d.extendleft(src)
                    want = collections.deque(range(5), maxlen=maxlen)
                    for x in src:
                        want.appendleft(x)
                    self.assertEqual(list(d), list(want))

    def test_other_iterables(self):
        d = collections.deque()
        d.extend(x * 2 for x in range(3))
        d.extendleft(iter("ab"))
        d.extend(range(3))
        d.extend({7: 0})
        self.assertEqual(list(d), ["b", "a", 0, 2, 4, 0, 1, 2, 7])
        d.extend(d)
        self.assertEqual(len(d), 18)
        d.extendleft(d)
        self.assertEqual(len(d), 36)

        def failing():
            yield 1
            yield 2
            raise ValueError("stop")

        d = collections.deque()
        with self.assertRaises(ValueError):
            d.extend(failing())
        self.assertEqual(list(d), [1, 2])
        with self.assertRaises(TypeError):
            d.extend(5)
        with self.assertRaises(TypeError):
            d.extendleft(None)

    def test_subclasses_and_identity(self):
        class D(collections.deque):
            pass

        d = D([1])
        d.extend((2, 3))
        d.extendleft([0])
        self.assertEqual(list(d), [0, 1, 2, 3])
        self.assertIs(type(d), D)
        items = [object(), object()]
        e = collections.deque()
        e.extend(items)
        self.assertIs(e[0], items[0])
        items.append(object())
        self.assertEqual(len(e), 2)

    def test_extension_during_iteration_is_an_error(self):
        d = collections.deque(range(3))
        it = iter(d)
        next(it)
        d.extend([9])
        with self.assertRaises(RuntimeError):
            next(it)


class RandomTests(unittest.TestCase):
    def test_random_mixes(self):
        r = random.Random(7)
        for maxlen in (None, 20):
            d = collections.deque(range(30), maxlen=maxlen)
            model = list(range(30))[-maxlen:] if maxlen else list(range(30))
            for step in range(4000):
                op = r.randrange(7)
                if op == 0:
                    k = r.randrange(-80, 80)
                    d.rotate(k)
                    model = rotated(model, k)
                elif op == 1:
                    items = [r.randrange(100) for _ in range(r.randrange(6))]
                    d.extend(items)
                    model = model + items
                elif op == 2:
                    items = tuple(r.randrange(100) for _ in range(r.randrange(6)))
                    d.extendleft(items)
                    model = list(reversed(items)) + model
                    if maxlen:
                        model = model[:maxlen]
                elif op == 3 and model:
                    self.assertEqual(d.popleft(), model.pop(0))
                elif op == 4 and model:
                    self.assertEqual(d.pop(), model.pop())
                elif op == 5:
                    x = r.randrange(100)
                    d.appendleft(x)
                    model.insert(0, x)
                    if maxlen:
                        model = model[:maxlen]
                else:
                    x = r.randrange(100)
                    d.append(x)
                    model.append(x)
                if maxlen:
                    model = model[-maxlen:]
                self.assertEqual(list(d), model, step)


class CostTests(unittest.TestCase):
    def test_a_rotation_costs_what_it_moves(self):
        def best(n):
            d = collections.deque(range(n))
            k = n // 3
            t = float("inf")
            for _ in range(3):
                start = time.perf_counter()
                for _ in range(30):
                    d.rotate(k)
                t = min(t, time.perf_counter() - start)
            return t

        small = best(3_000)
        large = best(300_000)
        self.assertLess(large, small * 300 + 0.05)


if __name__ == "__main__":
    unittest.main()
