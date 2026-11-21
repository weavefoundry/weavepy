"""Removing a key costs the same wherever it sits in a dict.

A dict keeps its keys in insertion order and answers "the i-th key" for
its iterators, views and caches. Removing a key from the front or the
middle of a large dict leaves a marker in the dict's entry list instead of
moving every later entry down, and the dict closes the gaps in one pass
later; none of that may show: order, positions, views, iteration, copies
and the errors for a dict changed during iteration all stay as they are in
a dict that never lost a key.

Every check runs against a model (a list of pairs) through the removal
idioms programs use: `del d[k]`, `d.pop(k)`, pop and re-insert (an LRU
touch), `popitem`, and `OrderedDict`'s `popitem(last=False)` and
`move_to_end`. A last group checks that the cost per removal stays flat as
the dict grows.
"""

import collections
import copy
import pickle
import time
import unittest
import weakref

SIZES = (3, 9, 17, 40, 200, 3000)


class Rng:
    """A fixed sequence, the same on every implementation."""

    def __init__(self, seed):
        self.state = seed

    def below(self, n):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (self.state >> 33) % n


def same(test, d, model):
    """`d` shows exactly `model`'s pairs, in order, through every reader."""
    keys = [k for k, _ in model]
    values = [v for _, v in model]
    test.assertEqual(len(d), len(model))
    test.assertEqual(list(d), keys)
    test.assertEqual(list(d.keys()), keys)
    test.assertEqual(list(d.values()), values)
    test.assertEqual(list(d.items()), model)
    test.assertEqual(list(reversed(d)), keys[::-1])
    test.assertEqual(list(reversed(d.items())), model[::-1])
    test.assertEqual([k for k in d], keys)
    test.assertEqual([d[k] for k in keys], values)
    test.assertEqual(dict(d), dict(model))
    test.assertEqual(list(d.copy().items()), model)
    test.assertEqual(list({**d}.items()), model)
    test.assertEqual(sorted(d, key=repr), sorted(keys, key=repr))
    test.assertEqual(repr(d) if type(d) is dict else None, repr(dict(model)) if type(d) is dict else None)
    if model:
        test.assertEqual(next(iter(d)), keys[0])
        test.assertEqual(next(reversed(d)), keys[-1])
        test.assertEqual(next(iter(d.items())), model[0])
    for k in keys[:: max(1, len(keys) // 25)]:
        test.assertIn(k, d)
    test.assertNotIn("never a key", d)


class RemovalOrderTests(unittest.TestCase):
    def test_del_from_the_front(self):
        for n in SIZES:
            d = {i: str(i) for i in range(n)}
            model = [(i, str(i)) for i in range(n)]
            for i in range(n):
                del d[i]
                del model[0]
                if i % max(1, n // 20) == 0:
                    same(self, d, model)
            self.assertEqual(d, {})
            d["again"] = 1
            self.assertEqual(list(d.items()), [("again", 1)])

    def test_del_from_the_middle_outwards(self):
        for n in SIZES:
            d = {("k", i): i for i in range(n)}
            model = [(("k", i), i) for i in range(n)]
            order = sorted(range(n), key=lambda i: abs(i - n // 2))
            for step, i in enumerate(order):
                del d[("k", i)]
                model.remove((("k", i), i))
                if step % max(1, n // 20) == 0:
                    same(self, d, model)
            self.assertEqual(len(d), 0)

    def test_random_removals_and_additions(self):
        for n in SIZES:
            rng = Rng(n)
            d = {}
            model = []
            fresh = 0
            for step in range(6 * n + 50):
                roll = rng.below(10)
                if roll < 4 or not model:
                    fresh += 1
                    key = "key%d" % fresh if fresh % 3 else fresh
                    d[key] = step
                    model.append((key, step))
                elif roll < 6:
                    at = rng.below(len(model))
                    key, _ = model.pop(at)
                    del d[key]
                elif roll < 8:
                    at = rng.below(len(model))
                    key, value = model.pop(at)
                    self.assertEqual(d.pop(key), value)
                elif roll == 8:
                    at = rng.below(len(model))
                    key, value = model[at]
                    d[key] = value + 1
                    model[at] = (key, value + 1)
                else:
                    self.assertEqual(d.popitem(), model.pop())
                if step % 37 == 0:
                    same(self, d, model)
            same(self, d, model)

    def test_pop_and_reinsert_moves_a_key_to_the_end(self):
        for n in SIZES:
            rng = Rng(7 * n)
            d = {i: i * i for i in range(n)}
            model = [(i, i * i) for i in range(n)]
            for step in range(4 * n + 20):
                at = rng.below(n)
                key, value = model.pop(at)
                got = d.pop(key)
                self.assertEqual(got, value)
                d[key] = got
                model.append((key, value))
                if step % 53 == 0:
                    same(self, d, model)
            same(self, d, model)

    def test_oldest_key_out_for_each_new_one(self):
        for n in SIZES:
            d = {i: None for i in range(n)}
            for i in range(n, 12 * n + 40):
                d[i] = None
                oldest = next(iter(d))
                self.assertEqual(oldest, i - n)
                del d[oldest]
                self.assertEqual(len(d), n)
            self.assertEqual(list(d), list(range(11 * n + 40, 12 * n + 40)))

    def test_missing_keys_still_raise(self):
        d = {i: i for i in range(100)}
        for i in range(0, 100, 2):
            del d[i]
        for i in range(0, 100, 2):
            with self.assertRaises(KeyError):
                del d[i]
            with self.assertRaises(KeyError):
                d.pop(i)
            self.assertEqual(d.pop(i, "gone"), "gone")
            self.assertIsNone(d.get(i))
        self.assertEqual(list(d), list(range(1, 100, 2)))

    def test_keys_that_compare_through_python(self):
        class Key:
            def __init__(self, n):
                self.n = n

            def __hash__(self):
                return self.n % 5

            def __eq__(self, other):
                return isinstance(other, Key) and self.n == other.n

        keys = [Key(i) for i in range(120)]
        d = {k: k.n for k in keys}
        model = [(k, k.n) for k in keys]
        for i in range(0, 120, 3):
            del d[Key(i)]
            model = [(k, v) for k, v in model if v != i]
        self.assertEqual([v for v in d.values()], [v for _, v in model])
        for k, v in model:
            self.assertEqual(d[Key(v)], v)
        self.assertNotIn(Key(3), d)

    def test_values_are_released_when_removed(self):
        class Token:
            pass

        d = {}
        refs = []
        for i in range(300):
            t = Token()
            d[i] = t
            refs.append(weakref.ref(t))
            del t
        for i in range(0, 300, 2):
            del d[i]
        self.assertTrue(all(refs[i]() is None for i in range(0, 300, 2)))
        self.assertTrue(all(refs[i]() is not None for i in range(1, 300, 2)))
        d.clear()
        self.assertTrue(all(r() is None for r in refs))


class ReadersAfterRemovalTests(unittest.TestCase):
    def holed(self, n=400):
        d = {i: -i for i in range(n)}
        for i in range(0, n, 3):
            del d[i]
        model = [(i, -i) for i in range(n) if i % 3]
        return d, model

    def test_views_and_copies(self):
        d, model = self.holed()
        same(self, d, model)
        self.assertEqual(d.keys() & {1, 2, 3, 4}, {1, 2, 4})
        self.assertEqual(d.keys() - set(range(100, 1000)), {i for i in range(100) if i % 3})
        self.assertEqual(list(copy.copy(d).items()), model)
        self.assertEqual(list(copy.deepcopy(d).items()), model)
        self.assertEqual(list(pickle.loads(pickle.dumps(d)).items()), model)
        self.assertEqual(list(dict.fromkeys(d)), [k for k, _ in model])
        self.assertEqual(sum(d.values()), sum(v for _, v in model))
        self.assertEqual(max(d), model[-1][0])
        self.assertEqual(d == dict(model), True)
        self.assertEqual({k: v for k, v in d.items()}, dict(model))

    def test_update_and_merge(self):
        d, model = self.holed()
        other = {i: "new" for i in range(390, 420)}
        d.update(other)
        want = dict(model)
        want.update(other)
        self.assertEqual(list(d.items()), list(want.items()))
        merged = d | {"last": 1}
        self.assertEqual(list(merged)[-1], "last")
        self.assertEqual(len(merged), len(want) + 1)

    def test_setdefault_and_in_place_values(self):
        d, model = self.holed()
        for k, v in model[::7]:
            d[k] += 1000
            self.assertEqual(d.setdefault(k, None), v + 1000)
        self.assertEqual(d.setdefault("fresh", 5), 5)
        self.assertEqual(list(d)[-1], "fresh")
        self.assertEqual(len(d), len(model) + 1)

    def test_iteration_notices_changes(self):
        d, _ = self.holed()
        with self.assertRaises(RuntimeError):
            for k in d:
                del d[k]
        d, _ = self.holed()
        with self.assertRaises(RuntimeError):
            for k in d:
                d[("new", k)] = 1
        d, model = self.holed()
        seen = []
        for k in d:
            d[k] = "replaced"
            seen.append(k)
        self.assertEqual(seen, [k for k, _ in model])

    def test_iterators_resume_across_positions(self):
        d, model = self.holed(1000)
        it = iter(d.items())
        got = [next(it) for _ in range(100)]
        got += list(it)
        self.assertEqual(got, model)
        back = reversed(d)
        tail = [next(back) for _ in range(50)]
        self.assertEqual(tail, [k for k, _ in model][::-1][:50])
        self.assertEqual(len(list(back)), len(model) - 50)

    def test_sorting_in_place_users(self):
        d, model = self.holed()
        self.assertEqual(sorted(d.items(), reverse=True), sorted(model, reverse=True))
        self.assertEqual(list(dict(sorted(d.items(), key=lambda kv: -kv[0]))), [k for k, _ in model][::-1])

    def test_kwargs_and_namespaces(self):
        def collect(**kw):
            return kw

        d = {"name%d" % i: i for i in range(60)}
        for i in range(0, 60, 2):
            del d["name%d" % i]
        self.assertEqual(collect(**d), {"name%d" % i: i for i in range(1, 60, 2)})
        ns = {"x%d" % i: i for i in range(80)}
        for i in range(0, 40):
            ns.pop("x%d" % i)
        self.assertEqual(eval("x40 + x79", ns), 119)
        with self.assertRaises(NameError):
            eval("x5", ns)

    def test_instance_namespaces(self):
        class Box:
            pass

        b = Box()
        for i in range(60):
            setattr(b, "f%d" % i, i)
        for i in range(0, 60, 2):
            if i % 4:
                delattr(b, "f%d" % i)
            else:
                del b.__dict__["f%d" % i]
        self.assertEqual([getattr(b, "f%d" % i) for i in range(1, 60, 2)], list(range(1, 60, 2)))
        self.assertFalse(hasattr(b, "f2"))
        self.assertFalse(hasattr(b, "f4"))
        b.f2 = "back"
        self.assertEqual(list(vars(b))[-1], "f2")
        self.assertEqual(len(vars(b)), 31)


class OrderedDictTests(unittest.TestCase):
    def check(self, od, model):
        self.assertEqual(list(od.items()), model)
        self.assertEqual(list(reversed(od)), [k for k, _ in model][::-1])
        self.assertEqual(len(od), len(model))
        if model:
            self.assertEqual(next(iter(od)), model[0][0])

    def test_lru_eviction(self):
        for n in SIZES:
            od = collections.OrderedDict((i, i) for i in range(n))
            model = [(i, i) for i in range(n)]
            for i in range(n, 6 * n + 30):
                od[i] = i
                model.append((i, i))
                self.assertEqual(od.popitem(last=False), model.pop(0))
                if i % 41 == 0:
                    self.check(od, model)
            self.check(od, model)

    def test_move_to_end_both_ways(self):
        for n in SIZES:
            rng = Rng(n + 1)
            od = collections.OrderedDict((i, str(i)) for i in range(n))
            model = [(i, str(i)) for i in range(n)]
            for step in range(5 * n + 30):
                at = rng.below(n)
                item = model.pop(at)
                if rng.below(5) == 0:
                    od.move_to_end(item[0], last=False)
                    model.insert(0, item)
                else:
                    od.move_to_end(item[0])
                    model.append(item)
                if step % 29 == 0:
                    self.check(od, model)
            self.check(od, model)
            with self.assertRaises(KeyError):
                od.move_to_end("absent")

    def test_mixed_operations(self):
        for n in SIZES:
            rng = Rng(3 * n + 2)
            od = collections.OrderedDict()
            model = []
            fresh = 0
            for step in range(8 * n + 60):
                roll = rng.below(12)
                if roll < 5 or not model:
                    fresh += 1
                    od[fresh] = step
                    model.append((fresh, step))
                elif roll < 7:
                    self.assertEqual(od.popitem(last=False), model.pop(0))
                elif roll == 7:
                    self.assertEqual(od.popitem(), model.pop())
                elif roll < 10:
                    at = rng.below(len(model))
                    key, value = model.pop(at)
                    if roll == 8:
                        self.assertEqual(od.pop(key), value)
                    else:
                        del od[key]
                else:
                    at = rng.below(len(model))
                    item = model.pop(at)
                    od.move_to_end(item[0])
                    model.append(item)
                if step % 31 == 0:
                    self.check(od, model)
            self.check(od, model)
            self.assertEqual(od, collections.OrderedDict(model))

    def test_iteration_notices_changes(self):
        od = collections.OrderedDict((i, i) for i in range(100))
        for i in range(0, 60):
            del od[i]
        with self.assertRaises(RuntimeError):
            for k in od:
                od.move_to_end(k)
        with self.assertRaises(RuntimeError):
            for k in od:
                del od[k]


class WeakDictionaryTests(unittest.TestCase):
    def test_sliding_window(self):
        class Node:
            def __init__(self, n):
                self.n = n

        d = weakref.WeakValueDictionary()
        keep = collections.deque()
        for i in range(2000):
            node = Node(i)
            d[i] = node
            keep.append(node)
            if len(keep) > 150:
                keep.popleft()
            del node
        self.assertEqual(sorted(d.keys()), list(range(1850, 2000)))
        self.assertEqual([d[i].n for i in range(1850, 2000)], list(range(1850, 2000)))
        keep.clear()
        self.assertEqual(len(d), 0)


class CostTests(unittest.TestCase):
    """The cost of a removal doesn't grow with the dict."""

    def per_op(self, body, n, ops):
        best = None
        for _ in range(3):
            state = body(n, 0)
            t0 = time.perf_counter()
            body(n, ops, state)
            dt = (time.perf_counter() - t0) / ops
            best = dt if best is None else min(best, dt)
        return best

    def flat(self, body):
        # (Each removal used to move every later entry: a dict 64 times
        # the size cost about 64 times as much. Allow 8, far past noise.)
        body(2000, 30_000, body(2000, 0))
        small = self.per_op(body, 2000, 30_000)
        large = self.per_op(body, 128_000, 30_000)
        self.assertLess(large, 8 * small + 2e-6, (small, large))

    def test_fifo_dict(self):
        def body(n, ops, d=None):
            if d is None:
                return {i: i for i in range(n)}
            for i in range(n, n + ops):
                d[i] = i
                del d[i - n]

        self.flat(body)

    def test_pop_and_reinsert(self):
        def body(n, ops, d=None):
            if d is None:
                return {i: i for i in range(n)}
            for i in range(ops):
                k = (i * 7919) % n
                v = d.pop(k)
                d[k] = v

        self.flat(body)

    def test_ordered_dict_lru(self):
        def body(n, ops, od=None):
            if od is None:
                return collections.OrderedDict((i, i) for i in range(n))
            for i in range(n, n + ops):
                od[i] = i
                od.popitem(last=False)

        self.flat(body)

    def test_ordered_dict_move_to_end(self):
        def body(n, ops, od=None):
            if od is None:
                return collections.OrderedDict((i, i) for i in range(n))
            for i in range(ops):
                od.move_to_end((i * 7919) % n)

        self.flat(body)


if __name__ == "__main__":
    unittest.main()
