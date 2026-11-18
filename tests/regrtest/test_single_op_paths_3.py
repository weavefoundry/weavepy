"""Results of operations given in-place fast paths.

Complex arithmetic in compiled code, `setattr` on plain instances,
`str.center`/`ljust`/`rjust`/`zfill`, `struct.unpack` of `bytes`,
`sorted` of a dict, a dict display or keyword splat into an empty dict,
`functools.reduce` run in place, and `bytes.join` of `bytes` parts. Each
loop runs long enough for the compiled paths to take over.
"""

import functools
import operator
import struct
import sys
import unittest

N = 3000


class ComplexTests(unittest.TestCase):
    def test_arithmetic(self):
        c = complex(1, 2)
        for _ in range(N):
            r = (c * c, c + 1.5, 2 * c, c / 2, c - c, 1.5 / c, c * 3, 4 - c)
        self.assertEqual(r, ((-3 + 4j), (2.5 + 2j), (2 + 4j), (0.5 + 1j), 0j,
                             (0.3 - 0.6j), (3 + 6j), (3 - 2j)))

    def test_errors_and_specials(self):
        c = complex(1, 2)
        for divisor in (0, 0.0, 0j):
            with self.assertRaises(ZeroDivisionError):
                for _ in range(N):
                    c / divisor
        big = complex(1e308, 1e308)
        for _ in range(N):
            r = big * 2
        self.assertEqual(r, complex(float("inf"), float("inf")))
        with self.assertRaises(TypeError):
            for _ in range(N):
                c | 1

    def test_subclass_and_bool(self):
        class C(complex):
            def __mul__(self, other):
                return "mul"

        x = C(1, 1)
        for _ in range(N):
            r = (x * 2, complex(1, 1) * True)
        self.assertEqual(r, ("mul", (1 + 1j)))


class SetattrTests(unittest.TestCase):
    def test_plain_and_protocols(self):
        class O:
            pass

        class P:
            @property
            def x(self):
                return self._x

            @x.setter
            def x(self, v):
                self._x = v * 2

        class S:
            def __setattr__(self, k, v):
                object.__setattr__(self, k, (k, v))

        class SL:
            __slots__ = ("a",)

        o, p, s, sl = O(), P(), S(), SL()
        for i in range(N):
            setattr(o, "x", i)
            setattr(o, "y" + str(i % 3), i)
            setattr(p, "x", i)
            setattr(s, "q", i)
            setattr(sl, "a", i)
        self.assertEqual((o.x, o.y0, o.y1, o.y2), (N - 1, N - 3, N - 2, N - 1))
        self.assertEqual((p.x, s.q, sl.a), (2 * (N - 1), ("q", N - 1), N - 1))
        with self.assertRaises(AttributeError):
            setattr(sl, "b", 1)
        with self.assertRaises(TypeError):
            setattr(o, 1, 1)

    def test_interned_key_and_finalizer_order(self):
        class O:
            pass

        events = []

        class D:
            def __init__(self, n):
                self.n = n

            def __del__(self):
                events.append(self.n)

        o = O()
        name = "".join(["z", "z"])
        for _ in range(N):
            setattr(o, name, 5)
        self.assertIs([k for k in vars(o) if k == "zz"][0], sys.intern("zz"))
        for i in range(N):
            setattr(o, "d", D(i))
        events.clear()
        setattr(o, "d", None)
        self.assertEqual(events, [N - 1])


class PaddingTests(unittest.TestCase):
    def test_center_just_zfill(self):
        for _ in range(N):
            r = ("ab".center(7), "ab".center(6, "*"), "Monday".center(9),
                 "x".ljust(4, "é"), "x".rjust(3), "-42".zfill(6), "+7".zfill(4),
                 "abc".center(2), "7".zfill(-1))
        self.assertEqual(r, ("   ab  ", "**ab**", "  Monday ", "xééé", "  x",
                             "-00042", "+007", "abc", "7"))

    def test_identity_when_wide_enough(self):
        s = "hello"
        for _ in range(N):
            r = (s.center(3), s.ljust(5), s.rjust(0), s.zfill(2))
        for x in r:
            self.assertIs(x, s)


class StructTests(unittest.TestCase):
    def test_unpack_bytes_and_buffers(self):
        packed = struct.pack("<ih", 7, -2)
        for _ in range(N):
            r = (struct.unpack("<ih", packed),
                 struct.unpack("<ih", bytearray(packed)),
                 struct.unpack("<ih", memoryview(packed)),
                 struct.unpack_from("<h", packed, 4))
        self.assertEqual(r, ((7, -2), (7, -2), (7, -2), (-2,)))
        with self.assertRaises(struct.error):
            struct.unpack("<i", b"ab")


class DictTests(unittest.TestCase):
    def test_sorted_dict_and_splats(self):
        def kw(**k):
            return k

        d = {"b": 1, "a": 2}
        for i in range(N):
            r = (sorted(d), {**d}, {**{"x": 1}, **{1: 2}}, kw(**{"a": 1, "b": i}))
        self.assertEqual(r, (["a", "b"], {"b": 1, "a": 2}, {"x": 1, 1: 2},
                             {"a": 1, "b": N - 1}))
        copy = r[1]
        copy["c"] = 3
        self.assertNotIn("c", d)
        with self.assertRaises(TypeError):
            kw(**{1: 2})


class ReduceTests(unittest.TestCase):
    def test_reduce_shapes(self):
        def f(a, b):
            if b == 77:
                raise KeyError(b)
            return a + b

        out = []
        for i in range(N):
            out.append(functools.reduce(operator.add, [1, 2, i]))
            out.append(functools.reduce(lambda a, b: a * b, (1, 2, 3), 1))
            out.append(functools.reduce(f, range(10)))
            try:
                functools.reduce(f, range(100))
            except KeyError as e:
                out.append(e.args)
            out.append(functools.reduce(operator.add, (x for x in range(i % 5 + 1))))
        self.assertEqual(out[-5:], [N + 2, 6, 45, (77,), 10])
        with self.assertRaises(TypeError):
            functools.reduce(operator.add, [])

    def test_callback_sees_caller_frame(self):
        def g(a, b):
            return sys._getframe(1).f_code.co_name

        def caller():
            return functools.reduce(g, [1, 2])

        for _ in range(N):
            r = caller()
        self.assertEqual(r, "caller")


class BytesJoinTests(unittest.TestCase):
    def test_join(self):
        for _ in range(N):
            r = (b" ".join([b"a", b"bc"]), b"".join(()), b"-".join((b"x",)),
                 b", ".join([b"1", bytearray(b"2")]), bytearray(b"+").join([b"a", b"b"]))
        self.assertEqual(r, (b"a bc", b"", b"x", b"1, 2", bytearray(b"a+b")))
        with self.assertRaises(TypeError):
            b"".join([b"a", "b"])


if __name__ == "__main__":
    unittest.main()
