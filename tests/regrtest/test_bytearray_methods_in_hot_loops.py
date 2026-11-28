"""A bytearray built and read a byte at a time, in loops long enough to
be compiled.

`b.append(n)`, `b.extend(data)`, `b.pop()`, `b[i]`, `len(b)` and
`b.decode()` give the same results however many times the loop has run,
and refuse the same things: a value that isn't a byte, a resize while a
view of the buffer is exported, an index out of range, a pop from an
empty buffer. The same holds for the neighbours that share their paths:
a byte stored through a `memoryview`, an item of a `range`, and
`slice.indices`.
"""

import hashlib
import itertools
import unittest

ROUNDS = 3000


def build_by_append(n):
    b = bytearray()
    for i in range(n):
        b.append(i & 0xFF)
    return b


def build_by_extend(n, chunk):
    b = bytearray()
    for _ in range(n):
        b.extend(chunk)
    return b


def drain(b):
    out = []
    while b:
        out.append(b.pop())
    return out


def checksum(b):
    total = 0
    for i in range(len(b)):
        total += b[i] * (i & 7)
    return total


def fill_view(view, n):
    for i in range(n):
        view[i] = (i * 7) & 0xFF


def range_total(r, n):
    total = 0
    for i in range(n):
        total += r[i] + r[-1 - i]
    return total


class BuildTests(unittest.TestCase):
    def test_append(self):
        b = build_by_append(ROUNDS)
        self.assertEqual(len(b), ROUNDS)
        self.assertEqual(bytes(b), bytes(i & 0xFF for i in range(ROUNDS)))
        self.assertIs(type(b), bytearray)

    def test_append_refuses_what_is_not_a_byte(self):
        b = bytearray(b"ab")
        for i in range(ROUNDS):
            b.append(1)
        for bad, error in ((256, ValueError), (-1, ValueError), (1.5, TypeError), ("a", TypeError), (None, TypeError)):
            with self.subTest(bad=bad):
                with self.assertRaises(error):
                    b.append(bad)
        self.assertEqual(len(b), ROUNDS + 2)
        b.append(True)
        self.assertEqual(b[-1], 1)

        class Index:
            def __index__(self):
                return 65

        b.append(Index())
        self.assertEqual(b[-1], 65)

    def test_extend(self):
        self.assertEqual(build_by_extend(ROUNDS, b"ab"), b"ab" * ROUNDS)
        self.assertEqual(build_by_extend(ROUNDS, bytearray(b"xyz")), b"xyz" * ROUNDS)
        self.assertEqual(build_by_extend(50, [1, 2, 3]), bytes([1, 2, 3]) * 50)
        self.assertEqual(build_by_extend(50, memoryview(b"mv")), b"mv" * 50)
        self.assertEqual(build_by_extend(50, range(4)), bytes(range(4)) * 50)
        self.assertEqual(build_by_extend(5, b""), b"")
        b = bytearray(b"abc")
        for _ in range(3):
            b.extend(b)
        self.assertEqual(b, b"abc" * 8)
        with self.assertRaises(TypeError):
            b.extend("text")
        with self.assertRaises(ValueError):
            b.extend([1, 256])
        with self.assertRaises(TypeError):
            b.extend(5)

    def test_pop(self):
        b = build_by_append(ROUNDS)
        out = drain(b)
        self.assertEqual(out, [i & 0xFF for i in reversed(range(ROUNDS))])
        self.assertEqual(b, b"")
        with self.assertRaises(IndexError):
            b.pop()
        c = bytearray(b"abcd")
        self.assertEqual(c.pop(0), ord("a"))
        self.assertEqual(c.pop(-2), ord("c"))
        self.assertEqual(c, b"bd")
        with self.assertRaises(IndexError):
            c.pop(5)

    def test_an_exported_buffer_is_not_resized(self):
        b = bytearray(b"abc")
        view = memoryview(b)
        for _ in range(ROUNDS):
            with self.assertRaises(BufferError):
                b.append(1)
        with self.assertRaises(BufferError):
            b.extend(b"x")
        with self.assertRaises(BufferError):
            b.pop()
        self.assertEqual(b, b"abc")
        view.release()
        b.append(1)
        b.extend(b"x")
        self.assertEqual(b.pop(), ord("x"))
        self.assertEqual(b, b"abc\x01")

    def test_decode(self):
        b = bytearray()
        text = ""
        for i in range(300):
            b.extend(b"caf\xc3\xa9 ")
            text += "café "
            if i % 50 == 0:
                self.assertEqual(b.decode(), text)
        self.assertEqual(b.decode("utf-8"), text)
        self.assertEqual(b.decode(errors="strict"), text)
        self.assertEqual(len(b.decode()), 5 * 300)
        b.append(0xFF)
        with self.assertRaises(UnicodeDecodeError):
            b.decode()
        self.assertEqual(b.decode(errors="ignore"), text)


class ReadTests(unittest.TestCase):
    def test_items_and_length(self):
        b = build_by_append(ROUNDS)
        self.assertEqual(checksum(b), sum((i & 0xFF) * (i & 7) for i in range(ROUNDS)))
        self.assertEqual([b[-1 - i] for i in range(5)], [(ROUNDS - 1 - i) & 0xFF for i in range(5)])
        for bad in (ROUNDS, -ROUNDS - 1, 10**12):
            with self.assertRaises(IndexError):
                b[bad]
        with self.assertRaises(TypeError):
            b["0"]
        self.assertEqual(b[True], 1)
        total = 0
        for _ in range(ROUNDS):
            total += len(b)
            if len(b) > 2000:
                del b[:1000]
        self.assertEqual(len(b), 2000)

    def test_lengths_of_other_kinds(self):
        fs = frozenset(range(37))
        total = 0
        for _ in range(ROUNDS):
            total += len(fs)
        self.assertEqual(total, 37 * ROUNDS)

    def test_bytes_stored_through_a_view(self):
        buf = bytearray(ROUNDS)
        view = memoryview(buf)
        fill_view(view, ROUNDS)
        self.assertEqual(buf, bytes((i * 7) & 0xFF for i in range(ROUNDS)))
        for bad, error in ((256, ValueError), (-1, ValueError), ("a", TypeError), (1.5, TypeError)):
            with self.subTest(bad=bad):
                with self.assertRaises(error):
                    view[0] = bad
        with self.assertRaises(IndexError):
            view[ROUNDS] = 0
        self.assertEqual(buf[0], 0)
        frozen = memoryview(bytes(8))
        for _ in range(100):
            with self.assertRaises(TypeError):
                frozen[0] = 1
        view.release()
        for _ in range(100):
            with self.assertRaises(ValueError):
                view[0] = 1

    def test_range_items(self):
        for r in (range(ROUNDS), range(5, 5 + 3 * ROUNDS, 3), range(10 * ROUNDS, 0, -10), range(-ROUNDS, 0)):
            with self.subTest(r=r):
                items = list(r)
                want = sum(items[i] + items[-1 - i] for i in range(ROUNDS))
                self.assertEqual(range_total(r, ROUNDS), want)
        r = range(10)
        for _ in range(200):
            with self.assertRaises(IndexError):
                r[10]
            with self.assertRaises(IndexError):
                r[-11]
        big = range(2**70, 2**70 + 100)
        self.assertEqual(sum(big[i] - 2**70 for i in range(100)), 4950)

    def test_slice_indices(self):
        bounds = [None, 0, 1, 2, 5, 10, 11, -1, -2, -10, -11, 100, -100]
        steps = [None, 1, 2, 3, -1, -2, -3, 100, -100]
        out = []
        for a, b, c in itertools.product(bounds, bounds, steps):
            for n in (0, 1, 2, 10, 11):
                got = slice(a, b, c).indices(n)
                out.append(got)
                # What the indices mean: the items a list of that length
                # gives for the slice.
                self.assertEqual(list(range(n))[a:b:c], list(range(*got)))
        digest = hashlib.sha256(repr(out).encode()).hexdigest()
        self.assertEqual(digest, "ce3bf762ebb40629a68d9503c9bd3563c20f0a4606dab995328682a14695e21b")
        self.assertEqual(slice(None, None, -1).indices(2**70), (2**70 - 1, -1, -1))
        self.assertEqual(slice(2**70, None).indices(5), (5, 5, 1))
        self.assertEqual(slice(None).indices(True), (0, 1, 1))
        with self.assertRaises(ValueError):
            slice(1).indices(-1)
        with self.assertRaises(ValueError):
            slice(1, 2, 0).indices(5)
        with self.assertRaises(TypeError):
            slice("a").indices(5)
        with self.assertRaises(TypeError):
            slice(1).indices("5")


def byte_total(data):
    total = 0
    for b in data:
        total += b
    return total


class IterationTests(unittest.TestCase):
    def test_loops_over_bytes_and_buffers(self):
        data = bytes(range(256)) * 20
        want = sum(range(256)) * 20
        for kind in (bytes, bytearray):
            with self.subTest(kind=kind.__name__):
                value = kind(data)
                for _ in range(5):
                    self.assertEqual(byte_total(value), want)
                self.assertEqual(sum(value), want)
                self.assertEqual(max(value), 255)
                self.assertEqual(min(value), 0)
                self.assertEqual(max(kind(b"q")), ord("q"))
                with self.assertRaises(ValueError):
                    max(kind())
                with self.assertRaises(ValueError):
                    min(kind())
                self.assertEqual(sum(kind()), 0)
                self.assertEqual(sum(kind(b"ab"), 1000), 1000 + 97 + 98)
                self.assertEqual(max(kind(b"abc"), default=0), 99)
                self.assertEqual(max(kind(), default=-1), -1)
                self.assertEqual(min(kind(b"abc"), key=lambda b: -b), 99)
                self.assertEqual(max(kind(b"ab"), kind(b"b")), kind(b"b"))
                self.assertTrue(all(type(b) is int for b in kind(b"xy")))

    def test_a_buffer_that_changes_during_the_loop(self):
        buf = bytearray(range(100))
        seen = []
        for b in buf:
            seen.append(b)
            if b == 10:
                del buf[20:]
            if b == 15:
                buf.extend(b"\xff\xfe")
        self.assertEqual(seen, list(range(20)) + [255, 254])
        buf = bytearray(b"abc")
        it = iter(buf)
        self.assertEqual(list(it), [97, 98, 99])
        buf.extend(b"de")
        self.assertEqual(list(it), [])
        buf = bytearray(b"abc")
        out = []
        for i in range(ROUNDS):
            for b in buf:
                out.append(b)
            if i == ROUNDS - 2:
                buf.clear()
        self.assertEqual(len(out), 3 * (ROUNDS - 1))


if __name__ == "__main__":
    unittest.main()
