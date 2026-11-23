"""The `PyUnicode_*` codec, wide-character and path-converter entry points.

Wheels call these directly (charset_normalizer's mypyc build calls
`PyUnicode_DecodeUTF32`); a missing one is a symbol lookup error at the
first call. Each is reached through `ctypes.pythonapi` and checked against
what the matching Python-level codec gives.
"""

import ctypes
import pathlib
import unittest

api = ctypes.pythonapi
P = ctypes.POINTER


def fn(name, restype, *argtypes):
    f = getattr(api, name)
    f.restype = restype
    f.argtypes = list(argtypes)
    return f


TEXT = "h\xe9€\U0001d11e"


class WideDecoders(unittest.TestCase):
    def test_utf32(self):
        f = fn("PyUnicode_DecodeUTF32", ctypes.py_object, ctypes.c_char_p,
               ctypes.c_ssize_t, ctypes.c_char_p, P(ctypes.c_int))
        data = TEXT.encode("utf-32")
        bo = ctypes.c_int(0)
        self.assertEqual(f(data, len(data), None, ctypes.byref(bo)), TEXT)
        self.assertEqual(bo.value, -1)
        self.assertEqual(f(data[4:], len(data) - 4, None, None), TEXT)
        be = TEXT.encode("utf-32-be")
        bo = ctypes.c_int(1)
        self.assertEqual(f(be, len(be), None, ctypes.byref(bo)), TEXT)
        with self.assertRaises(UnicodeDecodeError):
            f(b"\x00\x00\x11\x00", 4, None, None)
        self.assertEqual(f(b"\x00\x00\x11\x00", 4, b"replace", None), "�")

    def test_utf16_stateful(self):
        f = fn("PyUnicode_DecodeUTF16Stateful", ctypes.py_object, ctypes.c_char_p,
               ctypes.c_ssize_t, ctypes.c_char_p, P(ctypes.c_int), P(ctypes.c_ssize_t))
        data = "ab\U0001d11e".encode("utf-16-be")
        bo = ctypes.c_int(1)
        used = ctypes.c_ssize_t(-1)
        self.assertEqual(f(data[:-1], len(data) - 1, None, ctypes.byref(bo), ctypes.byref(used)), "ab")
        self.assertEqual(used.value, 4)
        self.assertEqual(f(data, len(data), None, ctypes.byref(bo), ctypes.byref(used)), "ab\U0001d11e")
        self.assertEqual(used.value, len(data))
        g = fn("PyUnicode_DecodeUTF16", ctypes.py_object, ctypes.c_char_p,
               ctypes.c_ssize_t, ctypes.c_char_p, P(ctypes.c_int))
        with self.assertRaises(UnicodeDecodeError):
            g(data[:-1], len(data) - 1, None, ctypes.byref(ctypes.c_int(1)))

    def test_utf32_stateful_and_utf7(self):
        f = fn("PyUnicode_DecodeUTF32Stateful", ctypes.py_object, ctypes.c_char_p,
               ctypes.c_ssize_t, ctypes.c_char_p, P(ctypes.c_int), P(ctypes.c_ssize_t))
        data = TEXT.encode("utf-32-le")
        used = ctypes.c_ssize_t()
        self.assertEqual(f(data[:-2], len(data) - 2, None, ctypes.byref(ctypes.c_int(-1)), ctypes.byref(used)), TEXT[:-1])
        self.assertEqual(used.value, len(data) - 4)
        u7 = fn("PyUnicode_DecodeUTF7", ctypes.py_object, ctypes.c_char_p, ctypes.c_ssize_t, ctypes.c_char_p)
        data = TEXT.encode("utf-7")
        self.assertEqual(u7(data, len(data), None), TEXT)

    def test_escapes(self):
        u = fn("PyUnicode_DecodeUnicodeEscape", ctypes.py_object, ctypes.c_char_p, ctypes.c_ssize_t, ctypes.c_char_p)
        self.assertEqual(u(b"a\\n\\u20ac", 9, None), "a\n€")
        r = fn("PyUnicode_DecodeRawUnicodeEscape", ctypes.py_object, ctypes.c_char_p, ctypes.c_ssize_t, ctypes.c_char_p)
        self.assertEqual(r(b"a\\n\\u20ac", 9, None), "a\\n€")


class Encoders(unittest.TestCase):
    def test_as_strings(self):
        for name, codec in (("UTF16", "utf-16"), ("UTF32", "utf-32"),
                            ("UnicodeEscape", "unicode_escape"),
                            ("RawUnicodeEscape", "raw_unicode_escape")):
            f = fn("PyUnicode_As%sString" % name, ctypes.py_object, ctypes.py_object)
            self.assertEqual(f(TEXT + "\n"), (TEXT + "\n").encode(codec), name)
        fn("PyUnicode_GetDefaultEncoding", ctypes.c_char_p)
        self.assertEqual(api.PyUnicode_GetDefaultEncoding(), b"utf-8")

    def test_wide_char(self):
        w = fn("PyUnicode_AsWideChar", ctypes.c_ssize_t, ctypes.py_object, ctypes.c_void_p, ctypes.c_ssize_t)
        need = w(TEXT, None, 0)
        units = len(TEXT) + (ctypes.sizeof(ctypes.c_wchar) == 2)
        self.assertEqual(need, units + 1)
        buf = (ctypes.c_wchar * need)()
        self.assertEqual(w(TEXT, buf, need), units)
        self.assertEqual(ctypes.wstring_at(buf), TEXT)
        short = (ctypes.c_wchar * 2)("x", "y")
        self.assertEqual(w("abc", short, 1), 1)
        self.assertEqual(short[1], "y")
        ws = fn("PyUnicode_AsWideCharString", ctypes.c_void_p, ctypes.py_object, P(ctypes.c_ssize_t))
        n = ctypes.c_ssize_t()
        p = ws("hi\0x", ctypes.byref(n))
        self.assertEqual(n.value, 4)
        self.assertEqual(ctypes.wstring_at(p, 4), "hi\0x")
        fn("PyMem_Free", None, ctypes.c_void_p)(p)
        with self.assertRaises(ValueError):
            ws("hi\0x", None)


class PathConverters(unittest.TestCase):
    def convert(self, name, arg):
        f = fn("PyUnicode_" + name, ctypes.c_int, ctypes.py_object, ctypes.c_void_p)
        slot = ctypes.c_void_p(0)
        rc = f(arg, ctypes.addressof(slot))
        rep = fn("PyObject_Repr", ctypes.py_object, ctypes.c_void_p)(slot.value)
        f.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        self.assertEqual(f(None, ctypes.addressof(slot)), 1)
        self.assertIsNone(slot.value)
        return rc, rep

    def test_converters(self):
        self.assertEqual(self.convert("FSConverter", pathlib.Path("/tmp/\xe9")),
                         (0x20000, repr(b"/tmp/\xc3\xa9")))
        self.assertEqual(self.convert("FSConverter", b"/b"), (0x20000, repr(b"/b")))
        self.assertEqual(self.convert("FSDecoder", b"/tmp/x"), (0x20000, repr("/tmp/x")))
        self.assertEqual(self.convert("FSDecoder", pathlib.Path("q")), (0x20000, repr("q")))
        f = fn("PyUnicode_FSConverter", ctypes.c_int, ctypes.py_object, ctypes.c_void_p)
        with self.assertRaises(ValueError):
            f(b"a\0b", ctypes.addressof(ctypes.c_void_p()))
        with self.assertRaises(TypeError):
            f(3, ctypes.addressof(ctypes.c_void_p()))


if __name__ == "__main__":
    unittest.main()
