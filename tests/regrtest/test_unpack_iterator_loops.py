"""`for` loops over `struct.iter_unpack` yield the records `unpack_from` reads.

Formats of one and several fields, of ints, floats and byte strings, over
bytes, bytearrays and memoryviews (sliced too), through the module function
and `Struct.iter_unpack`, run past the compile threshold; the loop variable
is rebound each step, so each record's tuple is freed as the next arrives.
`__length_hint__` counts down inside the loop, an iterator stopped partway
resumes where it was, and one that has finished stays finished.
"""

import struct
import unittest

DATA = bytes((i * 37 + 11) % 256 for i in range(4096))
FORMATS = ["<I", ">h", "<hh", "<IHb", "4s", "<2sH", "<d", "=Bxx?"]


def records(fmt, buf):
    size = struct.calcsize(fmt)
    return [struct.unpack_from(fmt, buf, at) for at in range(0, len(buf) - len(buf) % size, size)]


class UnpackIteratorLoopTests(unittest.TestCase):
    def test_loops(self):
        for fmt in FORMATS:
            size = struct.calcsize(fmt)
            data = DATA[: len(DATA) // size * size]
            want = records(fmt, data)
            for buf in (data, bytearray(data), memoryview(data), memoryview(bytearray(data))[size:]):
                for _ in range(3):
                    got = []
                    for t in struct.iter_unpack(fmt, buf):
                        got.append(t)
                    self.assertEqual(got, records(fmt, buf), (fmt, type(buf)))
            s = struct.Struct(fmt)
            count = 0
            for t in s.iter_unpack(data):
                self.assertEqual(t, want[count])
                count += 1
            self.assertEqual(count, len(want))

    def test_hint_and_resume(self):
        for _ in range(20):
            it = struct.iter_unpack("<H", DATA[:200])
            seen = 0
            for t in it:
                seen += 1
                self.assertEqual(it.__length_hint__(), 100 - seen)
                if seen == 40:
                    break
            self.assertEqual(next(it), struct.unpack_from("<H", DATA, 80))
            rest = [t for t in it]
            self.assertEqual(len(rest), 59)
            self.assertEqual(list(it), [])
            with self.assertRaises(StopIteration):
                next(it)

    def test_last_record_kept(self):
        last = None
        for _ in range(50):
            for t in struct.iter_unpack("<4s", DATA):
                last = t
        self.assertEqual(last, (DATA[-4:],))
        self.assertIs(type(last[0]), bytes)


if __name__ == "__main__":
    unittest.main()
