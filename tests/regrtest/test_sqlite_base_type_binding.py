"""sqlite3 binds exact base-type parameters without adapting them.

An exact int, float, str or bytearray binds as is, as CPython's
`need_adapt` decides, while other values (subclasses, dates) still go
through the adapter registry. Registering an adapter for a base type
makes every later value of that type adapt.
"""

import datetime
import sqlite3
import subprocess
import sys
import unittest


class MyStr(str):
    pass


class BaseTypeBindingTest(unittest.TestCase):
    def roundtrip(self, value):
        con = sqlite3.connect(":memory:")
        try:
            return con.execute("select ?", (value,)).fetchone()[0]
        finally:
            con.close()

    def test_base_types_bind_directly(self):
        self.assertEqual(self.roundtrip(7), 7)
        self.assertEqual(self.roundtrip(2.5), 2.5)
        self.assertEqual(self.roundtrip("text"), "text")
        self.assertEqual(self.roundtrip(bytearray(b"ab")), b"ab")
        self.assertEqual(self.roundtrip(None), None)

    def test_subclasses_and_dates_adapt(self):
        sqlite3.register_adapter(MyStr, lambda s: "adapted:" + str(s))
        try:
            self.assertEqual(self.roundtrip(MyStr("x")), "adapted:x")
        finally:
            del sqlite3.adapters[(MyStr, sqlite3.PrepareProtocol)]
        day = datetime.date(2026, 10, 6)
        self.assertEqual(self.roundtrip(day), "2026-10-06")

    def test_conform_is_used(self):
        class Point:
            def __conform__(self, protocol):
                if protocol is sqlite3.PrepareProtocol:
                    return "1;2"

        self.assertEqual(self.roundtrip(Point()), "1;2")

    def test_base_type_adapter_applies_once_registered(self):
        # In a child process: registering adapts every later int.
        code = (
            "import sqlite3\n"
            "con = sqlite3.connect(':memory:')\n"
            "print(con.execute('select ?', (3,)).fetchone()[0])\n"
            "sqlite3.register_adapter(int, lambda i: i * 10)\n"
            "print(con.execute('select ?', (3,)).fetchone()[0])\n"
        )
        out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True)
        self.assertEqual(out.stdout.split(), ["3", "30"], out.stderr)


if __name__ == "__main__":
    unittest.main()
