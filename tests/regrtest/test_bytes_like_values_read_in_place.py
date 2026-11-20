"""Reading bytes-like values where they are.

Indexing and storing through a `memoryview`, decoding a `bytearray`,
hashing or compressing a `bytes`, concatenating, and converting between
the bytes types read the source's bytes in place and copy them at most
once. The results are those of the general paths: every index form, every
error, and every codec outcome is unchanged. A few neighbours that take
the same kind of shortcut ride along: `range` indexing, sequence
comparison, and sets built from sets and dicts.
"""

import hashlib
import unittest
import zlib


class MemoryViewItemTests(unittest.TestCase):
    def test_reads(self):
        data = bytes(range(256))
        for name, view in (
            ("bytes", memoryview(data)),
            ("bytearray", memoryview(bytearray(data))),
            ("sub-view", memoryview(b"xx" + data + b"yy")[2:-2]),
            ("read-only", memoryview(bytearray(data)).toreadonly()),
        ):
            with self.subTest(view=name):
                self.assertEqual([view[i] for i in range(256)], list(range(256)))
                self.assertEqual([view[i - 256] for i in range(256)], list(range(256)))
                self.assertIs(type(view[5]), int)
                for bad in (256, -257, 10**9):
                    with self.assertRaises(IndexError):
                        view[bad]
                with self.assertRaises(TypeError):
                    view["0"]
                with self.assertRaises(TypeError):
                    view[1.0]

    def test_reads_of_other_layouts(self):
        data = bytes(range(64))
        self.assertEqual(memoryview(data)[::2][3], 6)
        self.assertEqual(memoryview(data)[::-1][0], 63)
        self.assertEqual(memoryview(data).cast("b")[63], 63)
        self.assertEqual(memoryview(bytes([200])).cast("b")[0], -56)
        self.assertEqual(memoryview(data).cast("c")[65 - 64], b"\x01")
        self.assertEqual(memoryview(data).cast("H")[1], int.from_bytes(data[2:4], "little"))
        self.assertEqual(memoryview(data).cast("B", (8, 8))[1, 2], 10)
        with self.assertRaises(NotImplementedError):
            memoryview(data).cast("B", (8, 8))[1]

    def test_stores(self):
        buf = bytearray(16)
        view = memoryview(buf)
        for i in range(16):
            view[i] = i * 3
        self.assertEqual(list(buf), [i * 3 for i in range(16)])
        view[-1] = 255
        view[0] = True
        self.assertEqual(buf[-1], 255)
        self.assertEqual(buf[0], 1)
        sub = view[4:8]
        sub[1] = 99
        self.assertEqual(buf[5], 99)
        for bad in (256, -1, 1.5, "a", None, b"a"):
            with self.subTest(bad=bad):
                with self.assertRaises((ValueError, TypeError)):
                    view[2] = bad
        self.assertEqual(buf[2], 6)
        with self.assertRaises(IndexError):
            view[16] = 0
        with self.assertRaises(TypeError):
            memoryview(bytes(4))[0] = 1
        with self.assertRaises(TypeError):
            view.toreadonly()[0] = 1
        view.release()
        with self.assertRaises(ValueError):
            view[0]
        with self.assertRaises(ValueError):
            view[0] = 1

    def test_run_stores(self):
        buf = bytearray(b"0123456789")
        view = memoryview(buf)
        view[2:5] = b"abc"
        view[5:7] = bytearray(b"XY")
        view[7:9] = memoryview(b"--")
        self.assertEqual(buf, b"01abcXY--9")
        view[:] = view[::-1].tobytes()
        self.assertEqual(buf, b"9--YXcba10")
        view[0:4] = view[6:10]
        self.assertEqual(buf, b"ba10Xcba10")
        with self.assertRaises(ValueError):
            view[0:2] = b"abc"
        with self.assertRaises(ValueError):
            view[0:2] = b"a"
        view[1:1] = b""
        view[::2] = b"....."
        self.assertEqual(buf, b".a.0.c.a.0")
        with self.assertRaises(TypeError):
            view[0:1] = "a"

    def test_loops(self):
        data = bytes(i & 0xFF for i in range(5000))
        view = memoryview(data)
        total = 0
        for i in range(5000):
            total += view[i]
        self.assertEqual(total, sum(data))
        out = bytearray(5000)
        target = memoryview(out)
        for i in range(5000):
            target[i] = view[4999 - i]
        self.assertEqual(out, data[::-1])
        for i in range(0, 4996, 4):
            target[i:i + 4] = view[i:i + 4]
        self.assertEqual(out[:4996], data[:4996])


class DecodeTests(unittest.TestCase):
    TEXTS = ("", "a", "plain ascii text", "café", "中文 and \U0001f600", "x" * 5000 + "é")

    def test_utf8_from_every_bytes_type(self):
        for text in self.TEXTS:
            raw = text.encode()
            for name, value in (
                ("bytes", raw),
                ("bytearray", bytearray(raw)),
                ("memoryview", memoryview(raw)),
            ):
                with self.subTest(text=text[:12], source=name):
                    self.assertEqual(str(value, "utf-8"), text)
                    if name != "memoryview":
                        self.assertEqual(value.decode(), text)
                        self.assertEqual(value.decode("utf-8", "strict"), text)
                        self.assertEqual(value.decode(errors="replace"), text)
                        self.assertEqual(len(value.decode()), len(text))

    def test_malformed_utf8(self):
        for bad in (b"\xff", b"ab\xc3", b"\xed\xa0\x80", b"ok\xf8\x88\x80\x80\x80", b"\xc0\xaf"):
            for kind in (bytes, bytearray):
                with self.subTest(bad=bad, kind=kind.__name__):
                    with self.assertRaises(UnicodeDecodeError):
                        kind(bad).decode()
                    self.assertIn("�", kind(bad).decode(errors="replace"))
                    self.assertEqual(kind(bad).decode(errors="ignore"), bad.decode(errors="ignore"))
        self.assertEqual(bytearray(b"a\xffb").decode(errors="surrogateescape"), "a\udcffb")

    def test_other_codecs(self):
        self.assertEqual(bytearray(b"abc").decode("ascii"), "abc")
        self.assertEqual(bytearray(b"caf\xe9").decode("latin-1"), "café")
        self.assertEqual(b"abc".decode("latin-1"), "abc")
        with self.assertRaises(UnicodeDecodeError):
            bytearray(b"caf\xe9").decode("ascii")
        self.assertEqual(bytearray("hé".encode("utf-16")).decode("utf-16"), "hé")
        import codecs

        self.assertEqual(codecs.decode(bytearray(b"x\xc3\xa9"), "utf-8"), "xé")
        self.assertEqual(codecs.utf_8_decode(b"x\xc3\xa9"), ("xé", 3))
        self.assertEqual(codecs.utf_8_decode(b"x\xc3", "strict", False), ("x", 1))
        self.assertEqual(codecs.ascii_decode(b"plain"), ("plain", 5))
        self.assertEqual(codecs.latin_1_decode(b"pl\xe4in"), ("pläin", 5))

    def test_a_decoded_bytearray_is_independent(self):
        buf = bytearray(b"hello")
        text = buf.decode()
        buf[0] = ord("J")
        self.assertEqual(text, "hello")
        self.assertEqual(buf.decode(), "Jello")


class ConversionTests(unittest.TestCase):
    def test_concatenation(self):
        a, b = b"abc", b"de"
        self.assertEqual(a + b, b"abcde")
        self.assertIs(type(a + b), bytes)
        self.assertEqual(a + b"", a)
        self.assertEqual(b"" + b"", b"")
        self.assertEqual(bytearray(a) + b, bytearray(b"abcde"))
        self.assertIs(type(bytearray(a) + b), bytearray)
        self.assertEqual(a + bytearray(b), b"abcde")
        self.assertIs(type(a + bytearray(b)), bytes)
        big = bytes(100_000)
        self.assertEqual(len(big + b"x" + big), 200_001)
        self.assertEqual((big + b"x")[-1:], b"x")
        with self.assertRaises(TypeError):
            a + "b"

    def test_bytes_of_other_bytes_types(self):
        buf = bytearray(b"mutable")
        frozen = bytes(buf)
        buf[0] = ord("M")
        self.assertEqual(frozen, b"mutable")
        self.assertIs(type(frozen), bytes)
        view = memoryview(b"0123456789")
        self.assertEqual(bytes(view[2:6]), b"2345")
        self.assertEqual(bytes(view[::3]), b"0369")
        self.assertEqual(bytes(memoryview(buf)[1:3]), b"ut")
        self.assertEqual(bytes(memoryview(b"\x01\x00\x02\x00").cast("H")), b"\x01\x00\x02\x00")
        self.assertEqual(bytes(bytearray()), b"")
        released = memoryview(b"x")
        released.release()
        with self.assertRaises(ValueError):
            bytes(released)

    def test_digests_agree_across_bytes_types(self):
        data = bytes(range(256)) * 40
        want = hashlib.sha256(data).hexdigest()
        self.assertEqual(hashlib.sha256(bytearray(data)).hexdigest(), want)
        self.assertEqual(hashlib.sha256(memoryview(data)).hexdigest(), want)
        h = hashlib.sha256()
        h.update(data[:1000])
        h.update(bytearray(data[1000:5000]))
        h.update(memoryview(data)[5000:])
        self.assertEqual(h.hexdigest(), want)
        self.assertEqual(hashlib.md5(data).digest(), hashlib.md5(bytearray(data)).digest())
        self.assertEqual(hashlib.blake2b(data, key=b"k").digest(), hashlib.blake2b(bytearray(data), key=bytearray(b"k")).digest())
        with self.assertRaises(TypeError):
            hashlib.sha256("text")
        self.assertEqual(zlib.crc32(data), zlib.crc32(bytearray(data)))
        self.assertEqual(zlib.adler32(data), zlib.adler32(memoryview(data)))
        packed = zlib.compress(data)
        self.assertEqual(zlib.decompress(packed), data)
        self.assertEqual(zlib.decompress(bytearray(packed)), data)
        self.assertEqual(zlib.compress(bytearray(data)), packed)

    def test_int_from_bytes(self):
        raw = b"\x01\x02\x03\x04"
        for value in (raw, bytearray(raw), memoryview(raw)):
            self.assertEqual(int.from_bytes(value, "big"), 0x01020304)
            self.assertEqual(int.from_bytes(value, "little"), 0x04030201)
        self.assertEqual(int.from_bytes([1, 2], "big"), 258)
        with self.assertRaises(TypeError):
            int.from_bytes("12", "big")


class NeighbourTests(unittest.TestCase):
    def test_range_items(self):
        for r in (
            range(10), range(3, 50, 7), range(50, 3, -7), range(0), range(5, 5), range(-10, 10),
            range(0, 2**62, 2**60), range(2**62, 0, -(2**60)), range(-(2**63), 2**63 - 1, 2**61),
        ):
            items = list(r)
            with self.subTest(r=r):
                for i in range(len(items)):
                    self.assertEqual(r[i], items[i])
                    self.assertEqual(r[i - len(items)], items[i])
                for bad in (len(items), -len(items) - 1, 10**30):
                    with self.assertRaises(IndexError):
                        r[bad]
        big = range(0, 2**100, 2**90)
        self.assertEqual(big[3], 3 * 2**90)
        self.assertEqual(range(2**64, 2**64 + 5)[-1], 2**64 + 4)
        self.assertEqual(range(2**63 - 2, 2**63 + 3)[3], 2**63 + 1)
        self.assertEqual(range(10)[True], 1)
        self.assertIs(type(range(10)[2]), int)

    def test_lists_of_different_lengths_never_compare_items(self):
        calls = []

        class Loud:
            def __eq__(self, other):
                calls.append(1)
                return True

        self.assertNotEqual([Loud(), 1], [Loud()])
        self.assertFalse([Loud()] == [Loud(), Loud()])
        self.assertEqual(calls, [])
        self.assertEqual([Loud()], [Loud()])
        self.assertEqual(len(calls), 1)
        self.assertNotEqual((Loud(), 1), (Loud(),))
        self.assertTrue((Loud(),) != (Loud(), Loud()))
        self.assertNotEqual((1, 2, 3), (1, 2))
        self.assertNotEqual(("a",) * 5, ("a",) * 6)

    def test_sequence_comparisons(self):
        a = list(range(1000))
        self.assertEqual(a, list(range(1000)))
        self.assertNotEqual(a, list(range(999)) + [0])
        self.assertNotEqual(a, a[:-1])
        self.assertLess(a[:-1], a)
        self.assertLess(a, a[:-1] + [1000])
        self.assertEqual(tuple(a), tuple(range(1000)))
        self.assertGreater((1, 2, 3), (1, 2))
        self.assertEqual([1, 2.0, True, "x"], [1.0, 2, 1, "x"])
        nan = float("nan")
        self.assertEqual([nan], [nan])
        self.assertNotEqual([nan], [float("nan")])
        self.assertEqual([2**70, -1], [2**70, -1])
        self.assertLess([1, 2**70], [1, 2**71])

    def test_sets_built_from_sets_and_dicts(self):
        source = {f"k{i}" for i in range(500)} | set(range(500))
        frozen = frozenset(source)
        self.assertEqual(frozen, source)
        self.assertIs(type(frozen), frozenset)
        copy = set(frozen)
        copy.add("extra")
        self.assertNotIn("extra", source)
        self.assertNotIn("extra", frozen)
        self.assertEqual(len(copy), 1001)
        self.assertIs(frozenset(frozen), frozen)
        d = {i: str(i) for i in range(300)}
        for i in range(0, 300, 3):
            del d[i]
        keys = set(d)
        self.assertEqual(keys, {i for i in range(300) if i % 3})
        self.assertEqual(frozenset(d), keys)
        keys.add(-1)
        self.assertNotIn(-1, d)
        self.assertEqual(sorted(set({1: "a", 1.0: "b", True: "c"})), [1])
        holed = set(range(100))
        for i in range(0, 100, 2):
            holed.discard(i)
        self.assertEqual(frozenset(holed), frozenset(range(1, 100, 2)))
        self.assertEqual(hash(frozenset(holed)), hash(frozenset(range(1, 100, 2))))


if __name__ == "__main__":
    unittest.main()
