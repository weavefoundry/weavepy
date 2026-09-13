"""Nested key callbacks must keep errors and lookup state in their own scope."""

import threading
import unittest


class MarkerError(Exception):
    pass


class BadHash:
    def __init__(self, error):
        self.error = error

    def __hash__(self):
        raise self.error


class StringKey:
    def __init__(self, value, callback=lambda: None):
        self.value = value
        self.callback = callback

    def __hash__(self):
        return hash(self.value)

    def __eq__(self, other):
        self.callback()
        return self.value == other


def exercise_nested_lookups():
    marker = MarkerError("inner hash")
    inside = {StringKey("inside"): 42}
    calls = []

    def nested():
        try:
            {"unrelated": 0}[BadHash(marker)]
        except MarkerError as error:
            assert error is marker
        else:
            raise AssertionError("hash exception was lost")
        assert inside["inside"] == 42
        assert "inside" in inside
        calls.append(True)

    key = StringKey("needle", nested)
    table = {key: 17}
    assert table["needle"] == 17
    assert table.get("needle") == 17
    assert table.setdefault("needle", 99) == 17
    table["needle"] = 23
    assert len(table) == 1
    assert next(iter(table)) is key
    assert table.pop("needle") == 23
    assert not table
    assert len(calls) >= 5


class KeyComparisonTests(unittest.TestCase):
    def test_nested_hash_errors_and_stored_key_callbacks(self):
        exercise_nested_lookups()

    def test_outer_comparison_error_survives_inner_lookup(self):
        inner = {StringKey("inside"): 42}
        marker = MarkerError("outer equality")

        def raising():
            self.assertEqual(inner["inside"], 42)
            raise marker

        table = {StringKey("needle", raising): 17}
        with self.assertRaises(MarkerError) as caught:
            table["needle"]
        self.assertIs(caught.exception, marker)
        self.assertEqual(inner["inside"], 42)
        self.assertEqual({"ordinary": 9}["ordinary"], 9)

    def test_mutation_restarts_lookup_after_nested_callback(self):
        inner = {StringKey("inside"): 42}
        table = {}
        calls = []

        def mutate():
            self.assertEqual(inner["inside"], 42)
            if not calls:
                calls.append(True)
                table.clear()
                table["needle"] = "replacement"

        table[StringKey("needle", mutate)] = "old"
        self.assertEqual(table["needle"], "replacement")
        self.assertEqual(table, {"needle": "replacement"})

    def test_thread_local_errors_remain_independent(self):
        barrier = threading.Barrier(4)
        failures = []

        def worker():
            try:
                barrier.wait(timeout=10)
                for _ in range(50):
                    exercise_nested_lookups()
            except BaseException as error:
                failures.append(error)

        threads = [threading.Thread(target=worker) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=30)
        self.assertTrue(all(not thread.is_alive() for thread in threads))
        self.assertEqual(failures, [])


if __name__ == "__main__":
    unittest.main()
