"""Behavior at the native deque and timedelta boundaries."""

from collections import deque
from datetime import date, datetime, timedelta, timezone
import operator
import pickle
import random
import threading
import unittest


class CollectionsDatetimeFastPaths(unittest.TestCase):
    def test_timedelta_integer_normalization(self):
        rng = random.Random(4177)
        factors = (86400000000, 1000000, 1, 1000, 60000000,
                   3600000000, 604800000000)
        for _ in range(500):
            values = [rng.randrange(-10**8, 10**8) for _ in factors]
            total = sum(a * b for a, b in zip(values, factors))
            days, rest = divmod(total, factors[0])
            seconds, micros = divmod(rest, 1000000)
            delta = timedelta(*values)
            self.assertEqual((delta.days, delta.seconds, delta.microseconds),
                             (days, seconds, micros))

    def test_timedelta_limits_and_cancellation(self):
        self.assertEqual(timedelta(days=999999999), timedelta.max -
                         timedelta(seconds=86399, microseconds=999999))
        self.assertEqual(timedelta(days=-999999999), timedelta.min)
        for values in (dict(days=1000000000), dict(days=-1000000000),
                       dict(days=-999999999, microseconds=-1),
                       dict(days=999999999, seconds=86400)):
            with self.assertRaises(OverflowError):
                timedelta(**values)
        for huge in (10**10, 2**100, 10**100):
            self.assertEqual(timedelta(days=huge, seconds=-huge * 86400),
                             timedelta())
            self.assertEqual(timedelta(seconds=-(huge // 1000000),
                                       microseconds=huge),
                             timedelta(microseconds=huge % 1000000))
        self.assertEqual(timedelta(True, False, True),
                         timedelta(days=1, microseconds=1))

    def test_timedelta_fallbacks_and_subclass(self):
        for micros, rounded in ((0.5, 0), (1.5, 2), (2.5, 2), (-1.5, -2)):
            self.assertEqual(timedelta(microseconds=micros),
                             timedelta(microseconds=rounded))
        self.assertEqual(timedelta(days=0.5, hours=-12, seconds=-0.25),
                         timedelta(microseconds=-250000))

        class Integer(int):
            pass

        class Delta(timedelta):
            def __init__(self, *args, **kwargs):
                self.initialized = True

        self.assertEqual(timedelta(Integer(2), Integer(-1)),
                         timedelta(days=1, seconds=86399))
        value = Delta(days=2, seconds=-1)
        self.assertIs(type(value), Delta)
        self.assertTrue(value.initialized)
        for invalid in (None, "1", [], object()):
            with self.assertRaises(TypeError):
                timedelta(days=invalid)
            # An early native fallback must still validate later components.
            with self.assertRaises(TypeError):
                timedelta(days=0.5, weeks=invalid)
            with self.assertRaises(TypeError):
                timedelta(days=10**100, weeks=invalid)
        for nonfinite in (float("inf"), -float("inf")):
            with self.assertRaises(OverflowError):
                timedelta(days=nonfinite)
        with self.assertRaises(ValueError):
            timedelta(seconds=float("nan"))

    def test_calendar_arithmetic(self):
        for year in (1, 4, 100, 400, 1900, 2000, 2024, 9999):
            for month in range(1, 13):
                start = datetime(year, month, 1, 1, 2, 3, 4,
                                 tzinfo=timezone.utc)
                step = timedelta(days=2, hours=25, microseconds=-5)
                end = start + step
                self.assertEqual(end - start, step)
                self.assertEqual(end - step, start)
                self.assertEqual(date.fromordinal(start.toordinal()),
                                 start.date())
                self.assertEqual(datetime.fromisoformat(end.isoformat()), end)

    def test_deque_consumed_prefix_and_rotation(self):
        rng = random.Random(4177)
        for bound in (None, 0, 1, 2, 64):
            d = deque(maxlen=bound)
            expected = []
            for step in range(1500):
                operation = rng.randrange(6)
                if operation == 0:
                    d.append(step)
                    expected.append(step)
                    if bound is not None and len(expected) > bound:
                        expected.pop(0)
                elif operation == 1:
                    d.appendleft(step)
                    expected.insert(0, step)
                    if bound is not None and len(expected) > bound:
                        expected.pop()
                elif operation == 2 and expected:
                    self.assertEqual(d.popleft(), expected.pop(0))
                elif operation == 3 and expected:
                    self.assertEqual(d.pop(), expected.pop())
                else:
                    turns = rng.randrange(-1000, 1000)
                    d.rotate(turns)
                    if expected:
                        k = turns % len(expected)
                        expected[:] = expected[-k:] + expected[:-k]
                self.assertEqual(len(d), len(expected))
                self.assertEqual(bool(d), bool(expected))
                self.assertEqual(list(d), expected)
                if expected:
                    self.assertEqual(d[0], expected[0])
                    self.assertEqual(d[-1], expected[-1])
                    self.assertEqual(d[-len(d)], expected[0])
                with self.assertRaises(IndexError):
                    d[len(d)]
                with self.assertRaises(IndexError):
                    d[-len(d) - 1]

    def test_deque_iterator_rotation_state(self):
        for n in (0, 1, 2, 20):
            for turns in (0, 1, -1, n, -n):
                d = deque(range(n))
                forward, backward = iter(d), reversed(d)
                d.rotate(turns)
                for it in (forward, backward):
                    if n > 1:
                        with self.assertRaises(RuntimeError):
                            next(it)
                        self.assertEqual(operator.length_hint(it), 0)
                    else:
                        self.assertEqual(list(it), list(d))

    def test_deque_index_callbacks(self):
        d = deque(range(5))

        class Index:
            def __index__(self):
                d.__init__([10, 20, 30])
                return -1

        self.assertEqual(d[Index()], 30)
        d.rotate(Index())
        self.assertEqual(list(d), [20, 30, 10])
        for value in (None, 1.5, "1", slice(1)):
            with self.assertRaises(TypeError):
                d[value]
            with self.assertRaises(TypeError):
                d.rotate(value)
        with self.assertRaises(OverflowError):
            d.rotate(2**100)
        for index in (-2**100, 2**100):
            with self.assertRaises(IndexError):
                d[index]

    def test_deque_large_rotations(self):
        d = deque(range(10000))
        expected = list(d)
        for turns in (1, -1, 4999, 5000, 5001, -4999, -5000, -5001) * 4:
            d.rotate(turns)
            k = turns % len(expected)
            expected[:] = expected[-k:] + expected[:-k]
            self.assertEqual(list(d), expected)
            self.assertEqual(list(reversed(d)), expected[::-1])

    def test_deque_pickle_and_live_replacement(self):
        d = deque(range(100), maxlen=100)
        for _ in range(39):
            d.popleft()
        it = iter(d)
        self.assertEqual(next(it), 39)
        d[1] = 123
        self.assertEqual(next(it), 123)
        for protocol in range(pickle.HIGHEST_PROTOCOL + 1):
            restored = pickle.loads(pickle.dumps(d, protocol))
            self.assertEqual(restored, d)
            self.assertEqual(restored.maxlen, d.maxlen)
            self.assertEqual(list(pickle.loads(pickle.dumps(it, protocol))),
                             list(d)[2:])

    def test_deque_iterator_offsets(self):
        d = deque(range(10))

        class Index:
            def __index__(self):
                return 3

        for factory in (iter, reversed):
            kind = type(factory(d))
            values = list(factory(d))
            for offset in (-10, 0, 3, 10, 100):
                iterator = kind(d, offset)
                expected = values[max(0, offset):]
                self.assertEqual(operator.length_hint(iterator), len(expected))
                self.assertEqual(list(iterator), expected)
                with self.assertRaises(StopIteration):
                    next(iterator)
            self.assertEqual(list(kind(d, Index())), values[3:])
            with self.assertRaises(TypeError):
                kind(d, 1.5)
            with self.assertRaises(OverflowError):
                kind(d, 2**100)
            with self.assertRaises(TypeError):
                kind.__next__(object())

    def test_deque_shared_iterator_threads(self):
        for factory in (iter, reversed):
            iterator = factory(deque(range(2000)))
            results = []
            errors = []
            lock = threading.Lock()

            def consume():
                local = []
                try:
                    for value in iterator:
                        local.append(value)
                except Exception as error:
                    with lock:
                        errors.append(error)
                with lock:
                    results.extend(local)

            threads = [threading.Thread(target=consume) for _ in range(4)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(10)
                self.assertFalse(thread.is_alive())
            self.assertEqual(sorted(results), list(range(2000)))
            self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
