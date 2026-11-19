"""`str()` and `format()` of a `mappingproxy`.

As CPython's `mappingproxy_str`, a proxy's `str()` is its mapping's (a
class's `__dict__` prints as a dict), and `format()` with an empty spec
is that `str()`; `repr()` keeps the `mappingproxy(...)` wrapper, and a
non-empty spec raises `TypeError`.
"""

import types
import unittest


class MappingProxyStrTests(unittest.TestCase):
    def test_str_and_format(self):
        m = types.MappingProxyType({"a": 1})
        self.assertEqual(str(m), "{'a': 1}")
        self.assertEqual(f"{m}", "{'a': 1}")
        self.assertEqual(format(m), "{'a': 1}")
        self.assertEqual(repr(m), "mappingproxy({'a': 1})")
        self.assertEqual(f"{m!r}", "mappingproxy({'a': 1})")
        with self.assertRaises(TypeError):
            format(m, "x")

    def test_class_dict(self):
        class C:
            x = 1

        self.assertTrue(str(C.__dict__).startswith("{"))
        self.assertTrue(repr(C.__dict__).startswith("mappingproxy({"))


if __name__ == "__main__":
    unittest.main()
