"""`io.BytesIO.getvalue()` hands back one bytes object until the buffer changes.

Repeated calls on an unchanged stream share their result, and a stream made
from a bytes object returns that object. Every way of changing the buffer
(writes at the end or after a seek, `writelines`, `truncate`, re-running
`__init__`, `__setstate__`, and writes through a `getbuffer()` view) makes
the next call see the new contents.
"""

import io
import unittest


class GetvalueTests(unittest.TestCase):
    def test_unchanged_stream_shares(self):
        f = io.BytesIO()
        f.write(b"abc" * 100)
        a = f.getvalue()
        self.assertIs(f.getvalue(), a)
        f.seek(0)
        f.read(10)
        self.assertIs(f.getvalue(), a)

    def test_initial_bytes(self):
        data = bytes(range(256)) * 10
        f = io.BytesIO(data)
        self.assertEqual(f.getvalue(), data)
        self.assertIs(f.getvalue(), f.getvalue())

    def test_writes(self):
        f = io.BytesIO(b"hello world")
        self.assertEqual(f.getvalue(), b"hello world")
        f.seek(0)
        f.write(b"J")
        self.assertEqual(f.getvalue(), b"Jello world")
        f.seek(0, 2)
        f.write(b"!")
        self.assertEqual(f.getvalue(), b"Jello world!")
        f.seek(1)
        f.write(b"ELLO")
        self.assertEqual(f.getvalue(), b"JELLO world!")
        f.writelines([b"-", b"+"])
        self.assertEqual(f.getvalue(), b"JELLO-+orld!")
        f.write(b"")
        self.assertEqual(f.getvalue(), b"JELLO-+orld!")

    def test_overseek_write(self):
        f = io.BytesIO(b"ab")
        self.assertEqual(f.getvalue(), b"ab")
        f.seek(4)
        f.write(b"c")
        self.assertEqual(f.getvalue(), b"ab\x00\x00c")

    def test_truncate(self):
        f = io.BytesIO(b"0123456789")
        self.assertEqual(f.getvalue(), b"0123456789")
        f.truncate(4)
        self.assertEqual(f.getvalue(), b"0123")
        f.seek(0)
        f.write(b"ab")
        f.truncate()
        self.assertEqual(f.getvalue(), b"ab")

    def test_reinit(self):
        f = io.BytesIO(b"first")
        self.assertEqual(f.getvalue(), b"first")
        f.__init__(b"later")
        self.assertEqual(f.getvalue(), b"later")
        f.__init__(b"")
        self.assertEqual(f.getvalue(), b"")

    def test_setstate(self):
        f = io.BytesIO(b"aaaa")
        self.assertEqual(f.getvalue(), b"aaaa")
        f.__setstate__((b"bbbb", 0, None))
        self.assertEqual(f.getvalue(), b"bbbb")

    def test_getbuffer_writes(self):
        f = io.BytesIO(b"abcdef")
        self.assertEqual(f.getvalue(), b"abcdef")
        view = f.getbuffer()
        view[0] = ord("X")
        self.assertEqual(f.getvalue(), b"Xbcdef")
        view[1] = ord("Y")
        self.assertEqual(f.getvalue(), b"XYcdef")
        del view
        self.assertEqual(f.getvalue(), b"XYcdef")
        with f.getbuffer() as view:
            view[2] = ord("Z")
        self.assertEqual(f.getvalue(), b"XYZdef")

    def test_results_are_independent(self):
        f = io.BytesIO()
        f.write(b"one")
        a = f.getvalue()
        f.write(b"two")
        self.assertEqual(a, b"one")
        self.assertEqual(f.getvalue(), b"onetwo")

    def test_text_wrapper_writes(self):
        raw = io.BytesIO()
        t = io.TextIOWrapper(raw, encoding="utf-8")
        t.write("abc")
        t.flush()
        self.assertEqual(raw.getvalue(), b"abc")
        t.write("def")
        t.flush()
        self.assertEqual(raw.getvalue(), b"abcdef")
        t.detach()


if __name__ == "__main__":
    unittest.main()
