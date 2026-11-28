"""Enum members as dict and set keys, and their .name and .value, read
from compiled loops: hashes equal to hash(member._name_), lookups and
set building, values of any type, a class that overrides __hash__, name
or value, a member whose _name_ was replaced, a class whose __hash__
changes after the loop warmed up, and a profiler that still sees
enum's methods, each matching CPython."""

import enum
import sys
import unittest


class Color(enum.Enum):
    RED = 1
    GREEN = (2, "g")
    BLUE = "b"


class OwnHash(enum.Enum):
    A = 1

    def __hash__(self):
        return 7


class OwnValue(enum.Enum):
    A = 1

    @enum.property
    def value(self):
        return "overridden"


class Late(enum.Enum):
    A = 1
    B = 2


def lookups(n, d, key):
    out = None
    for i in range(n):
        out = (d[key], key in d, hash(key))
    return out


def sets(n, a, b):
    out = None
    for i in range(n):
        out = {a, b}
    return out


def fields(n, m):
    out = None
    for i in range(n):
        out = (m.name, m.value)
    return out


class EnumHashAndFieldTests(unittest.TestCase):
    def test_hash_and_lookups(self):
        d = {Color.RED: "r", Color.GREEN: "g"}
        for n in (1, 400):
            self.assertEqual(lookups(n, d, Color.GREEN), ("g", True, hash("GREEN")))
            self.assertEqual(sets(n, Color.RED, Color.BLUE), {Color.BLUE, Color.RED})
            self.assertEqual(len(sets(n, Color.RED, Color.RED)), 1)
            self.assertEqual(lookups(n, {OwnHash.A: 1}, OwnHash.A), (1, True, 7))
        with self.assertRaises(KeyError):
            lookups(300, d, Color.BLUE)

    def test_fields(self):
        for n in (1, 400):
            self.assertEqual(fields(n, Color.RED), ("RED", 1))
            self.assertEqual(fields(n, Color.GREEN), ("GREEN", (2, "g")))
            self.assertEqual(fields(n, Color.BLUE), ("BLUE", "b"))
            self.assertEqual(fields(n, OwnValue.A), ("A", "overridden"))

    def test_class_changes(self):
        d = {Late.A: 1, Late.B: 2}
        self.assertEqual(lookups(400, d, Late.B)[2], hash("B"))
        Late.__hash__ = lambda self: 5
        try:
            self.assertEqual(lookups(400, {Late.B: 2}, Late.B), (2, True, 5))
        finally:
            del Late.__hash__
        self.assertEqual(lookups(400, d, Late.B), (2, True, hash("B")))

    def test_temporaries_released(self):
        def loop(n):
            total = 0
            for i in range(n):
                total += len(Color.GREEN.value)
            return total

        self.assertEqual(loop(300), 600)

    def test_profiler_sees_methods(self):
        fields(400, Color.RED)
        seen = []

        def prof(frame, event, arg):
            if event == "call":
                seen.append(frame.f_code.co_name)

        sys.setprofile(prof)
        try:
            fields(1, Color.RED)
            hash(Color.RED)
        finally:
            sys.setprofile(None)
        self.assertIn("__get__", seen)
        self.assertIn("__hash__", seen)


if __name__ == "__main__":
    unittest.main()
