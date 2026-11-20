"""UTF-16 and UTF-32 text, string slices with negative bounds or steps,
and byte buffers turned into lists and back.

Well-formed text is encoded and decoded in one pass, ASCII text is
sliced by its bytes, and a buffer's bytes become a list of ints (or a
list of ints a buffer) without a detour through each item's general
conversion. Malformed input, lone surrogates, error handlers and items
that aren't bytes behave as they always did.
"""

import codecs
import hashlib
import struct
import unittest

TEXTS = (
    "", "a", "plain ascii", "café", "中文", "x\U0001f600y", "\U0001f600" * 3,
    "a" * 1000 + "é", "﻿bom", "tab\tnl\n\x00nul", "z" * 70000,
)
CODECS = ("utf-16", "utf-16-le", "utf-16-be", "utf-32", "utf-32-le", "utf-32-be")


class WideCodecTests(unittest.TestCase):
    def test_round_trips(self):
        for text in TEXTS:
            for name in CODECS:
                with self.subTest(text=text[:12], codec=name):
                    raw = text.encode(name)
                    self.assertIs(type(raw), bytes)
                    self.assertEqual(raw.decode(name), text)
                    self.assertEqual(bytearray(raw).decode(name), text)
                    self.assertEqual(str(raw, name), text)
                    self.assertEqual(str(memoryview(raw), name), text)
                    self.assertEqual(codecs.decode(raw, name), text)
                    self.assertEqual(codecs.encode(text, name), raw)

    def test_encoded_bytes(self):
        self.assertEqual("ab".encode("utf-16-le"), b"a\x00b\x00")
        self.assertEqual("ab".encode("utf-16-be"), b"\x00a\x00b")
        self.assertEqual("é".encode("utf-16-le"), b"\xe9\x00")
        self.assertEqual("\U0001f600".encode("utf-16-le"), b"\x3d\xd8\x00\xde")
        self.assertEqual("\U0001f600".encode("utf-16-be"), b"\xd8\x3d\xde\x00")
        self.assertEqual("ab".encode("utf-32-le"), b"a\x00\x00\x00b\x00\x00\x00")
        self.assertEqual("ab".encode("utf-32-be"), b"\x00\x00\x00a\x00\x00\x00b")
        self.assertEqual("\U0001f600".encode("utf-32-be"), b"\x00\x01\xf6\x00")
        self.assertEqual("".encode("utf-16-le"), b"")
        # The plain names write a byte order mark first, in the machine's
        # order, and read either order back.
        marked = "hi".encode("utf-16")
        self.assertIn(marked[:2], (b"\xff\xfe", b"\xfe\xff"))
        self.assertEqual(len(marked), 6)
        self.assertEqual(b"\xff\xfeh\x00i\x00".decode("utf-16"), "hi")
        self.assertEqual(b"\xfe\xff\x00h\x00i".decode("utf-16"), "hi")
        self.assertEqual(len("hi".encode("utf-32")), 12)
        self.assertEqual(b"\x00\x00\xfe\xff\x00\x00\x00h".decode("utf-32"), "h")

    def test_malformed_input_and_handlers(self):
        raws = (
            b"a\x00b", b"\x00\xd8", b"\x00\xd8\x00\xdc", b"\x00\xdc\x00\xd8", b"a\x00\x00\xd8b\x00",
            b"\x00\x00\x11\x00", b"\x00\xd8\x00\x00", b"abc", b"\xff\xfe\x00\x00a\x00\x00\x00",
        )
        out = []
        for raw in raws:
            for name in CODECS:
                for errors in ("strict", "replace", "ignore", "surrogatepass", "backslashreplace"):
                    try:
                        out.append((name, errors, raw.hex(), ascii(raw.decode(name, errors))))
                    except UnicodeDecodeError as e:
                        out.append((name, errors, raw.hex(), e.start, e.end, e.reason))
        digest = hashlib.sha256(repr(out).encode()).hexdigest()
        self.assertEqual(len(out), 270)
        self.assertEqual(digest, "44ffcb4ef7823b77816864f3619e6856ac706800c71ae626f5364dd1a5f026a8")

    def test_lone_surrogates(self):
        for name in ("utf-16", "utf-16-le", "utf-32-be"):
            with self.subTest(codec=name):
                with self.assertRaises(UnicodeEncodeError):
                    "a\udc00b".encode(name)
                self.assertEqual(
                    "a\udc00b".encode(name, "ignore"), "ab".encode(name)
                )
                self.assertEqual(
                    "a\udc00b".encode(name, "replace"), "a?b".encode(name)
                )
                passed = "\ud800".encode(name, "surrogatepass")
                self.assertEqual(passed.decode(name, "surrogatepass"), "\ud800")

    def test_incremental_decoding_keeps_a_split_unit(self):
        self.assertEqual(codecs.utf_16_le_decode(b"a\x00b", "strict", False), ("a", 2))
        self.assertEqual(codecs.utf_16_le_decode(b"a\x00\x00\xd8", "strict", False), ("a", 2))
        self.assertEqual(codecs.utf_32_le_decode(b"a\x00\x00\x00b\x00", "strict", False), ("a", 4))
        decoder = codecs.getincrementaldecoder("utf-16-le")()
        raw = "héllo \U0001f600".encode("utf-16-le")
        text = "".join(decoder.decode(raw[i:i + 3]) for i in range(0, len(raw), 3))
        self.assertEqual(text + decoder.decode(b"", True), "héllo \U0001f600")


class StringSliceTests(unittest.TestCase):
    def test_ascii_slices_match_a_list_of_characters(self):
        text = "".join(chr(33 + i % 90) for i in range(57))
        chars = list(text)
        bounds = (None, 0, 1, 5, 56, 57, 100, -1, -5, -57, -100)
        for lo in bounds:
            for hi in bounds:
                for step in (None, 1, 2, 7, -1, -2, -7):
                    with self.subTest(lo=lo, hi=hi, step=step):
                        got = text[lo:hi:step]
                        self.assertIs(type(got), str)
                        self.assertEqual(got, "".join(chars[lo:hi:step]))
                        self.assertEqual(len(got), len(chars[lo:hi:step]))

    def test_other_text_slices_the_same_way(self):
        text = "héllo wörld \U0001f600 中文!"
        chars = list(text)
        for lo, hi, step in ((None, -1, None), (-3, None, None), (None, None, -1), (1, -1, 2), (-2, 0, -3)):
            self.assertEqual(text[lo:hi:step], "".join(chars[lo:hi:step]))

    def test_common_idioms(self):
        line = "key = value\n"
        self.assertEqual(line[:-1], "key = value")
        self.assertEqual(line[-6:-1], "value")
        self.assertEqual(line[::-1], "\neulav = yek")
        self.assertEqual(line[::2], "ky=vle")
        self.assertEqual("abc"[-10:], "abc")
        self.assertEqual("abc"[:-10], "")
        self.assertIs("abc"[-10:10], "abc")
        self.assertEqual(""[::-1], "")
        self.assertEqual("x"[::-1], "x")
        big = "0123456789" * 5000
        self.assertEqual(big[:-1][-3:], "678")
        self.assertEqual(big[::-1][:5], "98765")
        self.assertEqual(len(big[1::7]), len(range(1, 50000, 7)))
        self.assertTrue(big[::-1].isascii())
        self.assertEqual(hash(big[:-1]), hash(big[: len(big) - 1]))


class ByteListTests(unittest.TestCase):
    def test_buffers_to_lists(self):
        data = bytes(range(256)) * 3
        want = list(range(256)) * 3
        self.assertEqual(list(data), want)
        self.assertEqual(list(bytearray(data)), want)
        self.assertEqual(list(memoryview(data)), want)
        self.assertEqual(memoryview(data).tolist(), want)
        self.assertEqual(memoryview(bytearray(data))[5:9].tolist(), [5, 6, 7, 8])
        self.assertEqual(memoryview(data)[::-1].tolist(), want[::-1])
        self.assertEqual(memoryview(data).cast("b").tolist()[200], -56)
        self.assertEqual(memoryview(data).cast("H").tolist()[0], 0x0100)
        self.assertEqual(memoryview(data).cast("B", (3, 256)).tolist()[1][7], 7)
        self.assertEqual(tuple(bytearray(b"abc")), (97, 98, 99))
        self.assertEqual(sorted(bytearray(b"cab")), [97, 98, 99])
        self.assertEqual(sum(bytearray(data)), sum(want))
        self.assertTrue(all(type(x) is int for x in list(bytearray(b"ab"))))
        buf = bytearray(b"abc")
        items = list(buf)
        buf[0] = 0
        self.assertEqual(items, [97, 98, 99])

    def test_lists_to_buffers(self):
        want = bytes(range(256))
        self.assertEqual(bytes(list(range(256))), want)
        self.assertEqual(bytes(tuple(range(256))), want)
        self.assertEqual(bytearray(list(range(256))), want)
        self.assertEqual(bytes([]), b"")
        self.assertEqual(bytearray(()), b"")
        self.assertEqual(bytes([True, False, 2]), b"\x01\x00\x02")

        class Index:
            def __index__(self):
                return 65

        self.assertEqual(bytes([Index(), 66]), b"AB")
        for bad, error in (([256], ValueError), ([-1], ValueError), ([1, "a"], TypeError), ([1.0], TypeError), ([None], TypeError)):
            with self.subTest(bad=bad):
                with self.assertRaises(error):
                    bytes(bad)
                with self.assertRaises(error):
                    bytearray(bad)

    def test_membership(self):
        buf = bytearray(b"hello world")
        self.assertIn(ord("w"), buf)
        self.assertNotIn(0, buf)
        self.assertIn(b"lo w", buf)
        self.assertIn(bytearray(b"world"), buf)
        self.assertIn(memoryview(b"hell"), buf)
        self.assertIn(buf, buf)
        self.assertIn(b"", buf)
        self.assertNotIn(b"worlds", buf)
        self.assertIn(True, bytearray(b"\x01"))
        with self.assertRaises(ValueError):
            256 in buf
        with self.assertRaises(TypeError):
            "h" in buf
        with self.assertRaises(TypeError):
            None in buf

    def test_stride_slices(self):
        data = bytes(range(200))
        buf = bytearray(data)
        items = list(data)
        for lo in (None, 0, 3, 199, 300, -1, -50):
            for hi in (None, 0, 3, 199, 300, -1, -50):
                for step in (2, 3, 50, 1000, -2, -3):
                    with self.subTest(lo=lo, hi=hi, step=step):
                        self.assertEqual(list(data[lo:hi:step]), items[lo:hi:step])
                        self.assertEqual(list(buf[lo:hi:step]), items[lo:hi:step])
                        self.assertEqual(items[lo:hi:step], list(items)[lo:hi:step])
        self.assertIs(type(buf[::2]), bytearray)

    def test_translate(self):
        table = bytes((i * 7 + 1) & 0xFF for i in range(256))
        data = bytes(range(256)) * 2
        want = bytes(table[b] for b in data)
        self.assertEqual(data.translate(table), want)
        self.assertEqual(bytearray(data).translate(table), want)
        self.assertIs(type(bytearray(data).translate(table)), bytearray)
        self.assertEqual(data.translate(None), data)
        self.assertEqual(data.translate(None, b"\x00\x01"), bytes(b for b in data if b > 1))
        self.assertEqual(data.translate(table, delete=b"\x05"), bytes(table[b] for b in data if b != 5))
        with self.assertRaises(ValueError):
            data.translate(b"short")

    def test_iter_unpack(self):
        raw = struct.pack("<6H", 1, 2, 3, 4, 5, 6)
        for buf in (raw, bytearray(raw), memoryview(raw)):
            it = struct.iter_unpack("<HH", buf)
            self.assertEqual(it.__length_hint__(), 3)
            self.assertEqual(next(it), (1, 2))
            self.assertEqual(it.__length_hint__(), 2)
            self.assertEqual(list(it), [(3, 4), (5, 6)])
            self.assertEqual(list(it), [])
            with self.assertRaises(StopIteration):
                next(it)
        record = struct.Struct("<I")
        big = struct.pack("<5000I", *range(5000))
        self.assertEqual(sum(v for (v,) in record.iter_unpack(big)), sum(range(5000)))
        with self.assertRaises(struct.error):
            struct.iter_unpack("<I", b"12345")
        with self.assertRaises(TypeError):
            type(struct.iter_unpack("<H", b"ab"))()


if __name__ == "__main__":
    unittest.main()
