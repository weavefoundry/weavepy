"""`with` over memoryviews and files enters and exits through their types.

`with f.getbuffer() as v`, `with memoryview(b) as v` and `with open(...)`
in loops past the compile threshold: the view is released after the block
(the `BytesIO` can resize again, the view refuses use), an exception in
the block still exits (and propagates), `__exit__` sees it, and an
attribute set on a file doesn't shadow its type's `__enter__`.
"""

import io
import os
import tempfile
import unittest


class WithViewsTests(unittest.TestCase):
    def test_getbuffer_released(self):
        f = io.BytesIO(b"abcdef")
        for i in range(300):
            with f.getbuffer() as v:
                self.assertEqual(v[0], 97)
                self.assertEqual(len(v), 6 + (i > 0))
            with self.assertRaises(ValueError):
                v[0]
            f.seek(0, 2)
            if i == 0:
                f.write(b"g")
        self.assertEqual(f.getvalue(), b"abcdefg")

    def test_exception_in_block(self):
        b = bytearray(b"xyz")
        for _ in range(200):
            with self.assertRaises(KeyError):
                with memoryview(b) as v:
                    self.assertEqual(bytes(v), b"xyz")
                    raise KeyError("k")
            with self.assertRaises(ValueError):
                v.tobytes()
            b.append(0)
            b.pop()

    def test_returned_value_and_nesting(self):
        data = bytearray(range(10))
        for _ in range(200):
            with memoryview(data) as a, memoryview(data)[2:] as c:
                self.assertIs(a.obj, data)
                self.assertEqual(c[0], 2)
            with self.assertRaises(ValueError):
                a.tobytes()
            with self.assertRaises(ValueError):
                c.tobytes()

    def test_files(self):
        fd, path = tempfile.mkstemp()
        os.close(fd)
        try:
            for i in range(200):
                with open(path, "w") as f:
                    f.write("line %d\n" % i)
                self.assertTrue(f.closed)
                with open(path) as f:
                    self.assertEqual(f.read(), "line %d\n" % i)
            f = open(path)
            f.custom = 1
            with f as g:
                self.assertIs(g, f)
            self.assertTrue(f.closed)
            with self.assertRaises(ZeroDivisionError):
                with open(path) as f:
                    1 / 0
            self.assertTrue(f.closed)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
