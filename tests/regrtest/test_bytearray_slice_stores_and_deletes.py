"""Storing to and deleting a slice of a bytearray.

`b[i:j] = data` and `del b[i:j]` work on the bytes themselves: a store
of the same length changes them in place, any other moves what follows
the slice once, and an extended slice (`b[::2]`) is stored byte by byte
or deleted by closing the gaps in one pass. The cost of a store or a
delete near the front depends on the bytes moved, not on a copy of the
whole buffer per byte.
"""

import time
import unittest

BOUNDS = (None, 0, 1, 3, 9, 20, -1, -2, -20)
STEPS = (None, 1, 2, 3, 5, -1, -2, -7)


def model_store(data, sl, rep):
    """What `bytearray(data)[sl] = rep` leaves, on a list of ints."""
    items = list(data)
    items[sl] = list(rep)
    return bytes(items)


def model_delete(data, sl):
    items = list(data)
    del items[sl]
    return bytes(items)


class SliceStoreTests(unittest.TestCase):
    def test_stores_match_a_list_of_ints(self):
        for src in (b"0123456789", b"", b"a"):
            for start in BOUNDS:
                for stop in BOUNDS:
                    for step in STEPS:
                        sl = slice(start, stop, step)
                        for rep in (b"", b"x", b"xyz", b"0123456789ab"):
                            b = bytearray(src)
                            try:
                                want = model_store(src, sl, rep)
                            except ValueError:
                                # (An empty store to an extended slice is a
                                # bytearray's deletion; a list refuses it
                                # unless the slice is empty too.)
                                if rep:
                                    with self.assertRaises(ValueError):
                                        b[sl] = rep
                                    self.assertEqual(b, src)
                                else:
                                    b[sl] = rep
                                    self.assertEqual(bytes(b), model_delete(src, sl))
                                continue
                            b[sl] = rep
                            if not rep and step not in (None, 1):
                                want = model_delete(src, sl)
                            self.assertEqual(bytes(b), want, (src, sl, rep))

    def test_deletes_match_a_list_of_ints(self):
        for src in (b"0123456789", b"", b"a", bytes(range(97))):
            for start in BOUNDS:
                for stop in BOUNDS:
                    for step in STEPS:
                        sl = slice(start, stop, step)
                        b = bytearray(src)
                        del b[sl]
                        self.assertEqual(bytes(b), model_delete(src, sl), (src, sl))

    def test_extended_slice_size_mismatch(self):
        b = bytearray(10)
        with self.assertRaises(ValueError) as cm:
            b[::2] = b"abc"
        self.assertEqual(
            str(cm.exception), "attempt to assign bytes of size 3 to extended slice of size 5"
        )
        self.assertEqual(b, bytearray(10))

    def test_other_sources(self):
        b = bytearray(b"0123456789")
        b[2:5] = [65, 66]
        self.assertEqual(b, b"01AB56789")
        b[2:4] = bytearray(b"zzz")
        self.assertEqual(b, b"01zzz56789")
        b[::2] = range(5)
        self.assertEqual(b, bytes([0, 49, 1, 122, 2, 53, 3, 55, 4, 57]))
        b[:] = memoryview(b"pq")
        self.assertEqual(b, b"pq")
        b[1:1] = (ord(c) for c in "xyz")
        self.assertEqual(b, b"pxyzq")
        with self.assertRaises(ValueError):
            b[0:1] = [256]
        with self.assertRaises(TypeError):
            b[0:1] = ["a"]
        self.assertEqual(b, b"pxyzq")

    def test_a_bytearray_stored_into_itself(self):
        b = bytearray(b"0123456789")
        b[1:4] = b
        self.assertEqual(b, b"00123456789456789")
        b = bytearray(b"abc")
        b[:] = b
        self.assertEqual(b, b"abc")
        b[1:1] = b
        self.assertEqual(b, b"aabcbc")

    def test_index_objects_as_bounds(self):
        class Index:
            def __init__(self, value):
                self.value = value

            def __index__(self):
                return self.value

        b = bytearray(b"0123456789")
        b[Index(1):Index(3)] = b"Q"
        self.assertEqual(b, b"0Q3456789")
        del b[Index(0):Index(2)]
        self.assertEqual(b, b"3456789")
        del b[Index(-2):]
        self.assertEqual(b, b"34567")
        b[::Index(2)] = b"abc"
        self.assertEqual(b, b"a4b6c")


class ExportedBufferTests(unittest.TestCase):
    def test_a_resize_is_refused_while_a_view_is_exported(self):
        b = bytearray(b"0123456789")
        view = memoryview(b)
        with self.assertRaises(BufferError):
            b[0:2] = b"abc"
        with self.assertRaises(BufferError):
            del b[0:2]
        with self.assertRaises(BufferError):
            del b[1:1:2]
        with self.assertRaises(BufferError):
            b[::2] = b""
        self.assertEqual(b, b"0123456789")
        # A store of the same length, and an empty plain delete, resize
        # nothing.
        b[0:2] = b"ab"
        del b[1:1]
        b[::5] = b"XY"
        self.assertEqual(b, b"Xb234Y6789")
        self.assertEqual(view.tobytes(), b"Xb234Y6789")
        view.release()
        del b[0:2]
        self.assertEqual(b, b"234Y6789")

    def test_a_same_length_store_shows_through_a_view(self):
        b = bytearray(1000)
        view = memoryview(b)
        b[100:104] = b"abcd"
        self.assertEqual(bytes(view[98:106]), b"\0\0abcd\0\0")
        view[200:202] = b"zz"
        self.assertEqual(b[199:203], b"\0zz\0")
        view.release()


class CostTests(unittest.TestCase):
    """A store or delete costs the bytes it moves, once."""

    @staticmethod
    def best(fn, repeat=5):
        best = float("inf")
        for _ in range(repeat):
            start = time.perf_counter()
            fn()
            best = min(best, time.perf_counter() - start)
        return best

    def test_deleting_a_run_moves_the_tail_once(self):
        def consume(run):
            b = bytearray(200_000)

            def go():
                for _ in range(40):
                    del b[:run]

            return go

        short = self.best(consume(1))
        long = self.best(consume(64))
        # (One move per byte deleted made the 64-byte run 64 times the
        # 1-byte run's cost.)
        self.assertLess(long, short * 12 + 0.002)

    def test_a_small_store_does_not_copy_the_buffer(self):
        def store(size):
            b = bytearray(size)

            def go():
                for i in range(300):
                    b[8:12] = b"abcd"
                    b[16:20] = b"xy"
                    b[16:18] = b"wxyz"

            return go

        small = self.best(store(4_000))
        large = self.best(store(400_000))
        # (The large buffer's stores move 100 times the bytes, in two of
        # the three stores; rebuilt as a list of ints per store, it cost
        # thousands of times the small one's.)
        self.assertLess(large, small * 400 + 0.05)

    def test_same_length_stores_do_not_depend_on_the_size(self):
        def store(size):
            b = bytearray(size)

            def go():
                for i in range(2000):
                    b[8:12] = b"abcd"

            return go

        small = self.best(store(1_000))
        large = self.best(store(1_000_000))
        self.assertLess(large, small * 20 + 0.01)


if __name__ == "__main__":
    unittest.main()
