"""Buffers read in place by `int.from_bytes`, `struct.iter_unpack` and
`array.fromlist`.

`int.from_bytes` of a short bytearray or memoryview reads its bytes in
place; `struct.iter_unpack` reads its iterator's state at the class's slot
places and takes a `bytes` subclass's buffer through a view; `array`
checks a list's item types natively before packing it whole. Each must
answer as the general path does.
"""

import array
import struct
import unittest


class FromBytesTests(unittest.TestCase):
    def test_buffers(self):
        data = bytes(range(1, 9))
        views = [data, bytearray(data), memoryview(data), memoryview(bytearray(data))]
        for _ in range(500):
            for v in views:
                for n in range(0, 9):
                    for order in ("little", "big"):
                        self.assertEqual(int.from_bytes(v[:n], order), int.from_bytes(bytes(v[:n]), order))
        self.assertEqual(int.from_bytes(memoryview(data)[::2], "little"), int.from_bytes(data[::2], "little"))
        self.assertEqual(int.from_bytes(bytearray(b"\x01\x02")), 258)

    def test_released_and_signed(self):
        m = memoryview(b"\x01\x02")
        m.release()
        with self.assertRaises(ValueError):
            int.from_bytes(m, "little")
        self.assertEqual(int.from_bytes(bytearray(b"\xff"), "little", signed=True), -1)

        class Sub(int):
            pass

        self.assertIs(type(Sub.from_bytes(memoryview(b"\x05"), "little")), Sub)


class IterUnpackTests(unittest.TestCase):
    def test_formats_and_sources(self):
        raw = bytes(range(256)) * 4
        for fmt in ("<I", ">h", "<2H", "<d", "B", "<Ib"):
            size = struct.calcsize(fmt)
            src = raw[: size * 37]
            expected = repr([struct.unpack_from(fmt, src, i * size) for i in range(37)])
            for buf in (src, bytearray(src), memoryview(src)):
                # (Compared as text: a NaN field is never equal to itself.)
                self.assertEqual(repr(list(struct.iter_unpack(fmt, buf))), expected)

    def test_length_hint_and_end(self):
        it = struct.iter_unpack("<H", bytearray(b"\x01\x00\x02\x00"))
        self.assertEqual(next(it), (1,))
        self.assertEqual(it.__length_hint__(), 1)
        self.assertEqual(list(it), [(2,)])
        self.assertEqual(list(it), [])

    def test_subclass_buffers(self):
        class S(bytes):
            pass

        class B(bytearray):
            pass

        self.assertEqual(list(struct.iter_unpack("<H", S(b"\x05\x00\x06\x00"))), [(5,), (6,)])
        self.assertEqual(list(struct.iter_unpack("<H", B(b"\x07\x00"))), [(7,)])
        self.assertEqual(struct.unpack("<H", S(b"\x08\x00")), (8,))
        self.assertEqual(struct.unpack_from("<H", B(b"\x00\x09\x00"), 1), (9,))


class ArrayFromListTests(unittest.TestCase):
    def test_types(self):
        cases = [("d", [1.5, 2.0]), ("d", [1, 2.0]), ("d", [True, 2.0]), ("i", [1, 2, 3]), ("i", [True]), ("q", [2**63 - 1])]
        for tc, data in cases:
            for _ in range(50):
                a = array.array(tc, data)
                self.assertEqual(a.tolist(), [type(a[0])(v) for v in data])
                b = array.array(tc)
                b.fromlist(data)
                self.assertEqual(b, a)

    def test_errors(self):
        with self.assertRaises(TypeError):
            array.array("i", [1, 2.0])
        with self.assertRaises(OverflowError):
            array.array("b", [1, 300])
        a = array.array("i", [1])
        with self.assertRaises(TypeError):
            a.fromlist([2, "x"])
        self.assertEqual(a.tolist(), [1])

    def test_float_subclass_and_big(self):
        class F(float):
            pass

        self.assertEqual(array.array("d", [F(1.5), 2.0]).tolist(), [1.5, 2.0])
        floats = [float(i) for i in range(5000)]
        self.assertEqual(array.array("d", floats).tolist(), floats)


if __name__ == "__main__":
    unittest.main()
