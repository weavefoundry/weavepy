"""`pickle.dumps` and `pickle.loads` take every documented argument form.

Positional and keyword protocols, `fix_imports`, `buffer_callback`,
`encoding`, `errors` and `buffers` give the results the pickle engine
gives, wrong arguments raise `TypeError`, and a `str` passed to `loads`
is refused. A method of the Python pickler or unpickler whose code or
defaults change after many calls is seen at once, however many calls ran
before it changed.
"""

import io
import pickle
import types
import unittest


class Point:
    def __init__(self, x):
        self.x = x


VALUES = [1, -5, 2**40, 2.5, "abc", b"xyz", [1, (2, 3)], {"a": 1}, None, True]


class ArgumentTests(unittest.TestCase):
    def test_round_trips(self):
        for value in VALUES:
            for protocol in (None, 0, 2, 4, 5, -1, pickle.HIGHEST_PROTOCOL):
                data = pickle.dumps(value, protocol)
                self.assertEqual(pickle.loads(data), value)
                self.assertEqual(pickle.dumps(value, protocol=protocol), data)

    def test_keywords(self):
        data = pickle.dumps([1, 2], fix_imports=False)
        self.assertEqual(pickle.loads(data, fix_imports=False), [1, 2])
        self.assertEqual(pickle.dumps(3, 4, buffer_callback=None), pickle.dumps(3, 4))
        self.assertEqual(pickle.loads(data, encoding="latin1", errors="ignore"), [1, 2])
        self.assertEqual(pickle.loads(data, buffers=None), [1, 2])
        self.assertEqual(pickle.loads(bytearray(data)), [1, 2])
        self.assertEqual(pickle.loads(memoryview(data)), [1, 2])

    def test_out_of_band_buffers(self):
        buffers = []
        value = pickle.PickleBuffer(b"payload")
        data = pickle.dumps(value, 5, buffer_callback=buffers.append)
        self.assertEqual(len(buffers), 1)
        self.assertEqual(bytes(pickle.loads(data, buffers=buffers)), b"payload")

    def test_bad_arguments(self):
        with self.assertRaises(TypeError):
            pickle.dumps()
        with self.assertRaises(TypeError):
            pickle.dumps(1, 2, 3)
        with self.assertRaises(TypeError):
            pickle.dumps(1, protocol=2, nonsense=1)
        with self.assertRaises(TypeError):
            pickle.dumps(1, 2, protocol=2)
        with self.assertRaises(TypeError):
            pickle.loads()
        with self.assertRaises(TypeError):
            pickle.loads(b".", b".")
        with self.assertRaises(TypeError):
            pickle.loads(data=pickle.dumps(1))
        with self.assertRaises(TypeError):
            pickle.loads("not bytes")
        with self.assertRaises(ValueError):
            pickle.dumps(1, pickle.HIGHEST_PROTOCOL + 1)

    def test_failures(self):
        with self.assertRaises(Exception):
            pickle.dumps(lambda: None)
        with self.assertRaises(Exception):
            pickle.loads(b"\x80\x05garbage")
        with self.assertRaises(EOFError):
            pickle.loads(b"")

    def test_instances(self):
        for _ in range(50):
            self.assertEqual(pickle.loads(pickle.dumps(Point(7))).x, 7)

    def test_names(self):
        self.assertEqual(pickle.dumps.__name__, "dumps")
        self.assertEqual(pickle.loads.__name__, "loads")
        self.assertEqual(pickle.dumps.__module__, "_pickle")
        self.assertEqual(pickle.loads.__module__, "_pickle")
        self.assertEqual(repr(pickle.dumps), "<built-in function dumps>")
        self.assertEqual(repr(pickle.loads), "<built-in function loads>")


class ChangeTests(unittest.TestCase):
    def warm(self):
        for _ in range(200):
            self.assertEqual(pickle.loads(pickle.dumps("abc")), "abc")
            self.assertEqual(pickle.loads(pickle.dumps(["x", 1])), ["x", 1])

    def test_pickler_method_code(self):
        if not issubclass(pickle.Pickler, pickle._Pickler):
            self.skipTest("the pickler is not the Python one")
        self.warm()
        save_str = pickle._Pickler.save_str
        original = save_str.__code__
        pickle._front_test_save_str = types.FunctionType(
            original, save_str.__globals__, "save_str_copy")

        def patched(self, obj):
            _front_test_save_str(self, "patched" if obj == "abc" else obj)

        save_str.__code__ = patched.__code__
        try:
            self.assertEqual(pickle.loads(pickle.dumps("abc")), "patched")
        finally:
            save_str.__code__ = original
            del pickle._front_test_save_str
        self.warm()

    def test_unpickler_method_code(self):
        if not issubclass(pickle.Unpickler, pickle._Unpickler):
            self.skipTest("the unpickler is not the Python one")
        self.warm()
        opcode = pickle.BININT1[0]
        load = pickle._Unpickler.dispatch[opcode]
        original = load.__code__

        def patched(self):
            self.append(int.from_bytes(self.read(1), "little") + 100)

        load.__code__ = patched.__code__
        try:
            self.assertEqual(pickle.loads(pickle.dumps(5)), 105)
        finally:
            load.__code__ = original
        self.assertEqual(pickle.loads(pickle.dumps(5)), 5)
        self.warm()

    def test_pickler_method_defaults(self):
        if not issubclass(pickle.Pickler, pickle._Pickler):
            self.skipTest("the pickler is not the Python one")
        self.warm()
        save = pickle._Pickler.save
        saved = save.__defaults__
        save.__defaults__ = saved
        try:
            self.assertEqual(pickle.loads(pickle.dumps(["x"])), ["x"])
        finally:
            save.__defaults__ = saved
        self.warm()

    def test_dispatch_entry_replaced(self):
        if not issubclass(pickle.Pickler, pickle._Pickler):
            self.skipTest("the pickler is not the Python one")
        self.warm()
        table = pickle._Pickler.dispatch
        original = table[str]
        calls = []

        def counting(self, obj):
            calls.append(obj)
            original(self, obj)

        table[str] = counting
        try:
            self.assertEqual(pickle.loads(pickle.dumps("abc")), "abc")
        finally:
            table[str] = original
        self.assertEqual(calls, ["abc"])
        self.warm()

    def test_through_a_file(self):
        f = io.BytesIO()
        pickle.dump([1, "a"], f)
        f.seek(0)
        self.assertEqual(pickle.load(f), [1, "a"])


if __name__ == "__main__":
    unittest.main()
