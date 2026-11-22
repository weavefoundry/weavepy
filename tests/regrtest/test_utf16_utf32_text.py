"""UTF-16 and UTF-32 encode and decode non-ASCII text as their units spell it.

Text mixing ASCII, Latin-1, other BMP characters (CJK, the last BMP code
points) and astral ones (surrogate pairs in UTF-16), in both byte orders
and with the native BOM, encodes to the bytes a code-point-by-code-point
reference gives and decodes back. Lone surrogates in UTF-16 data reach the
error handlers, and an odd trailing byte is still an error.
"""

import struct
import unittest

TEXTS = [
    "héllo wörld",
    "日本語のテキスト",
    "a\U0001F600b\U0010FFFFc",
    "\x80߿ࠀ￿\U00010000",
    "mixed: ascii, é, 漢字, \U0001F680 and back",
    "é" * 300 + "x" + "\U0001F600" * 50,
]


def units16(text):
    out = []
    for ch in text:
        cp = ord(ch)
        if cp < 0x10000:
            out.append(cp)
        else:
            v = cp - 0x10000
            out += [0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)]
    return out


def ref16(text, order):
    u = units16(text)
    return struct.pack(order + "%dH" % len(u), *u)


def ref32(text, order):
    return struct.pack(order + "%dI" % len(text), *map(ord, text))


class UnitTextTests(unittest.TestCase):
    def test_round_trips(self):
        for _ in range(40):
            for text in TEXTS:
                self.assertEqual(text.encode("utf-16-le"), ref16(text, "<"))
                self.assertEqual(text.encode("utf-16-be"), ref16(text, ">"))
                self.assertEqual(text.encode("utf-16"), b"\xff\xfe" + ref16(text, "<"))
                self.assertEqual(text.encode("utf-32-le"), ref32(text, "<"))
                self.assertEqual(text.encode("utf-32-be"), ref32(text, ">"))
                self.assertEqual(text.encode("utf-32"), b"\xff\xfe\x00\x00" + ref32(text, "<"))
                self.assertEqual(ref16(text, "<").decode("utf-16-le"), text)
                self.assertEqual(ref16(text, ">").decode("utf-16-be"), text)
                self.assertEqual((b"\xfe\xff" + ref16(text, ">")).decode("utf-16"), text)
                self.assertEqual(text.encode("utf-16").decode("utf-16"), text)
                self.assertEqual(text.encode("utf-32").decode("utf-32"), text)

    def test_block_edges(self):
        # A non-ASCII character at each offset of ASCII text, across the
        # eight-byte and four-unit blocks.
        for _ in range(10):
            for ch in ("é", "€", "\U0001F600", "\x7f", "Ā"):
                for at in range(26):
                    text = "abcdefghijklmnopqrstuvwxyz"[:at] + ch + "0123456789abcdefghij"[: 25 - at]
                    for order, name in (("<", "utf-16-le"), (">", "utf-16-be")):
                        self.assertEqual(text.encode(name), ref16(text, order), (ch, at))
                        self.assertEqual(ref16(text, order).decode(name), text, (ch, at))

    def test_lone_surrogates(self):
        for data in (struct.pack("<3H", 0x61, 0xD800, 0x62), struct.pack("<2H", 0x61, 0xDC00),
                     struct.pack("<2H", 0x61, 0xD83D)):
            with self.assertRaises(UnicodeDecodeError):
                data.decode("utf-16-le")
            self.assertEqual(data.decode("utf-16-le", "replace").replace("�", "?")[0], "a")
            back = data.decode("utf-16-le", "surrogatepass")
            self.assertEqual(back.encode("utf-16-le", "surrogatepass"), data)
        with self.assertRaises(UnicodeDecodeError):
            b"a\x00\xe9".decode("utf-16-le")
        self.assertEqual(b"a\x00\xe9".decode("utf-16-le", "ignore"), "a")


if __name__ == "__main__":
    unittest.main()
