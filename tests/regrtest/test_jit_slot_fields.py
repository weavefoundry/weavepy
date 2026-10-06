"""Hot code reads `__slots__` members in place.

A plain class declaring `__slots__` lays its instances' member slots out
over names its class shares, so a hot method body reading `self.x` or
`other.x` reads a fixed position once the class's version is checked.
Each loop below runs long enough for the method body and the loop to be
compiled, then the class, the instance or its slots change under the
compiled code, and every change must reach the generic path with the
interpreter's result, error and traceback.
"""

import sys
import unittest


class V:
    __slots__ = ("x", "y", "z")

    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

    def dot(self, o):
        return self.x * o.x + self.y * o.y + self.z * o.z

    def norm2(self):
        return self.x * self.x + self.y * self.y + self.z * self.z

    def scaled_x(self, f):
        return self.x * f


class W:
    __slots__ = ("x", "y", "z")

    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

    def dot(self, o):
        return -1


class Sub(V):
    __slots__ = ("w",)


class D:
    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

    def dot(self, o):
        return self.x * o.x + self.y * o.y + self.z * o.z


def dot_loop(a, b, n, hook=None, at=-1):
    t = 0.0
    for i in range(n):
        if i == at:
            hook()
        t += a.dot(b)
    return t


def norm_loop(a, n, hook=None, at=-1):
    t = 0.0
    for i in range(n):
        if i == at:
            hook()
        t += a.norm2()
    return t


def scale_loop(a, n):
    t = 0.0
    for i in range(n):
        t += a.scaled_x(i)
    return t


def mixed_loop(a, b, n):
    t = 0.0
    for i in range(n):
        t += a.dot(b)
    return t


def dict_loop(a, b, n):
    t = 0.0
    for i in range(n):
        t += a.dot(b)
    return t


def frames(error):
    names = []
    tb = error.__traceback__
    while tb is not None:
        names.append(tb.tb_frame.f_code.co_name)
        tb = tb.tb_next
    return names


def warm():
    a = V(1.0, 2.0, 3.0)
    b = V(0.5, 0.5, 0.5)
    for _ in range(40):
        dot_loop(a, b, 600)
        norm_loop(a, 600)
        scale_loop(a, 600)
        mixed_loop(a, D(0.5, 0.5, 0.5), 600)
        dict_loop(D(1.0, 2.0, 3.0), D(0.5, 0.5, 0.5), 600)


warm()


class SlotFieldReads(unittest.TestCase):
    def test_plain(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)
        self.assertEqual(dot_loop(a, b, 30000), 3.0 * 30000)
        self.assertEqual(norm_loop(a, 30000), 14.0 * 30000)
        self.assertEqual(scale_loop(a, 3000), float(sum(range(3000))))
        self.assertEqual(mixed_loop(a, D(0.5, 0.5, 0.5), 3000), 3.0 * 3000)
        self.assertEqual(
            dict_loop(D(1.0, 2.0, 3.0), D(0.5, 0.5, 0.5), 3000), 3.0 * 3000
        )

    def test_int_fields(self):
        a = V(1, 2, 3)
        b = V(4, 5, 6)
        self.assertEqual(dot_loop(a, b, 20000), 32 * 20000)

    def test_field_changes_type_mid_loop(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)

        def hook():
            b.x = 2

        self.assertEqual(dot_loop(a, b, 20000, hook, 5000), 3.0 * 5000 + 4.5 * 15000)

        def to_str():
            a.y = "s"

        with self.assertRaises(TypeError):
            dot_loop(a, b, 20000, to_str, 1000)

    def test_slot_deleted_mid_loop(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)

        def hook():
            del b.y

        try:
            dot_loop(a, b, 20000, hook, 7000)
        except AttributeError as error:
            self.assertEqual(frames(error)[-2:], ["dot_loop", "dot"])
        else:
            self.fail("a deleted slot did not raise")

    def test_self_slot_deleted_mid_loop(self):
        a = V(1.0, 2.0, 3.0)

        def hook():
            del a.z

        try:
            norm_loop(a, 20000, hook, 7000)
        except AttributeError as error:
            self.assertEqual(frames(error)[-2:], ["norm_loop", "norm2"])
        else:
            self.fail("a deleted slot did not raise")

    def test_slot_never_set(self):
        a = V(1.0, 2.0, 3.0)
        b = V.__new__(V)
        b.x = 1.0
        b.y = 1.0
        try:
            dot_loop(a, b, 100)
        except AttributeError as error:
            self.assertIn("z", str(error))
            self.assertEqual(frames(error)[-2:], ["dot_loop", "dot"])
        else:
            self.fail("an unset slot did not raise")

    def test_class_attribute_reassigned_mid_loop(self):
        class Local(V):
            __slots__ = ()

        a = Local(1.0, 2.0, 3.0)
        b = Local(0.5, 0.5, 0.5)
        for _ in range(40):
            dot_loop(a, b, 600)

        def hook():
            Local.x = property(lambda self: 10.0)

        got = dot_loop(a, b, 20000, hook, 5000)
        self.assertEqual(got, 3.0 * 5000 + (100.0 + 1.0 + 1.5) * 15000)

    def test_method_reassigned_mid_loop(self):
        class Local(V):
            __slots__ = ()

        a = Local(1.0, 2.0, 3.0)
        b = Local(0.5, 0.5, 0.5)
        for _ in range(40):
            dot_loop(a, b, 600)

        def hook():
            Local.dot = lambda self, o: 1.0

        self.assertEqual(dot_loop(a, b, 20000, hook, 5000), 3.0 * 5000 + 15000)

    def test_class_reassigned_mid_loop(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)

        def hook():
            a.__class__ = W

        self.assertEqual(dot_loop(a, b, 20000, hook, 5000), 3.0 * 5000 - 15000)

    def test_argument_class_reassigned_mid_loop(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)

        def hook():
            b.__class__ = W
            b.x = 2.0

        self.assertEqual(dot_loop(a, b, 20000, hook, 5000), 3.0 * 5000 + 4.5 * 15000)

    def test_subclass_receivers(self):
        a = Sub(1.0, 2.0, 3.0)
        b = Sub(0.5, 0.5, 0.5)
        a.w = 9
        self.assertEqual(dot_loop(a, b, 20000), 3.0 * 20000)
        self.assertEqual(dot_loop(V(1.0, 2.0, 3.0), b, 20000), 3.0 * 20000)

    def test_argument_of_another_kind(self):
        a = V(1.0, 2.0, 3.0)
        self.assertEqual(dot_loop(a, D(0.5, 0.5, 0.5), 20000), 3.0 * 20000)
        with self.assertRaises(AttributeError):
            dot_loop(a, 5, 10)
        with self.assertRaises(AttributeError):
            dot_loop(a, None, 10)

    def test_profiler_installed_mid_loop(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)
        calls = []

        def profile(frame, event, arg):
            if event == "call" and frame.f_code is V.dot.__code__:
                calls.append(1)

        def hook():
            sys.setprofile(profile)

        try:
            got = dot_loop(a, b, 20000, hook, 5000)
        finally:
            sys.setprofile(None)
        self.assertEqual(got, 3.0 * 20000)
        self.assertEqual(len(calls), 15000)

    def test_tracer_installed_mid_loop(self):
        a = V(1.0, 2.0, 3.0)
        b = V(0.5, 0.5, 0.5)
        lines = []

        def trace(frame, event, arg):
            if frame.f_code is V.dot.__code__:
                if event == "line":
                    lines.append(frame.f_lineno)
                return trace
            return trace

        def hook():
            sys.settrace(trace)

        try:
            got = dot_loop(a, b, 20000, hook, 19000)
        finally:
            sys.settrace(None)
        self.assertEqual(got, 3.0 * 20000)
        self.assertEqual(len(lines), 1000)

    def test_dict_instances(self):
        self.assertEqual(
            dict_loop(D(1.0, 2.0, 3.0), D(0.5, 0.5, 0.5), 20000), 3.0 * 20000
        )
        a = D(1.0, 2.0, 3.0)
        b = D(0.5, 0.5, 0.5)
        b.extra = 1
        self.assertEqual(dict_loop(a, b, 20000), 3.0 * 20000)
        b.__dict__["x"] = 2.0
        self.assertEqual(dict_loop(a, b, 20000), 4.5 * 20000)
        del b.y
        with self.assertRaises(AttributeError):
            dict_loop(a, b, 10)


if __name__ == "__main__":
    unittest.main()
