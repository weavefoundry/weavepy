"""Compiled loops call simple method bodies directly or in line.

A method whose body is pure numeric work on its arguments and on scalar
fields of `self` runs from a compiled caller without the method-call
helper once the site's guard (the receiver's class version, no instance
attribute shadowing the name, the function's `__code__`) holds. Each loop below runs long enough to compile,
then the class, the instance, the function or the arguments change under
the compiled code, and every change must reach the generic path with the
interpreter's result, error and traceback.
"""

import os
import subprocess
import sys
import unittest


class Adder:
    def m(self, a):
        return a + 1

    def div(self, a):
        return 100 // a


class Fields:
    def __init__(self):
        self.x = 1
        self.y = 2

    def m(self, a):
        return a * 2


class Slotted:
    __slots__ = ('x',)

    def __init__(self):
        self.x = 5

    def m(self, a):
        return a - 1


def loop(o, n, hook=None, at=-1):
    t = 0
    for i in range(n):
        if i == at:
            hook()
        t += o.m(i)
    return t


# One compiled caller per receiver class, so each site's guard holds.
def fields_loop(o, n, hook=None, at=-1):
    t = 0
    for i in range(n):
        if i == at:
            hook()
        t += o.m(i)
    return t


def slotted_loop(o, n):
    t = 0
    for i in range(n):
        t += o.m(i)
    return t


def div_loop(o, values):
    t = 0
    for v in values:
        t += o.div(v)
    return t


def expected(n, f):
    return sum(f(i) for i in range(n))


def warm():
    for _ in range(40):
        loop(Adder(), 200)
        fields_loop(Fields(), 200)
        slotted_loop(Slotted(), 200)


warm()


class DirectMethodCalls(unittest.TestCase):
    def test_plain_receivers(self):
        self.assertEqual(loop(Adder(), 5000), expected(5000, lambda i: i + 1))
        self.assertEqual(fields_loop(Fields(), 5000), expected(5000, lambda i: i * 2))
        self.assertEqual(slotted_loop(Slotted(), 5000), expected(5000, lambda i: i - 1))

    def test_class_attribute_reassigned_mid_loop(self):
        saved = Adder.m

        def hook():
            Adder.m = lambda self, a: a + 100

        try:
            got = loop(Adder(), 3000, hook, 1000)
        finally:
            Adder.m = saved
        want = expected(1000, lambda i: i + 1) + sum(i + 100 for i in range(1000, 3000))
        self.assertEqual(got, want)
        self.assertEqual(loop(Adder(), 3000), expected(3000, lambda i: i + 1))

    def test_instance_attribute_shadows_method_mid_loop(self):
        o = Fields()

        def hook():
            o.m = lambda a: -a

        got = fields_loop(o, 3000, hook, 1500)
        want = expected(1500, lambda i: i * 2) - sum(range(1500, 3000))
        self.assertEqual(got, want)
        del o.m
        self.assertEqual(fields_loop(o, 3000), expected(3000, lambda i: i * 2))
        # An attribute set before the loop shadows it from the start.
        p = Adder()
        p.m = lambda a: 7
        self.assertEqual(loop(p, 100), 700)

    def test_code_swapped_mid_loop(self):
        def other(self, a):
            return a + 1000

        saved = Adder.m.__code__

        def hook():
            Adder.m.__code__ = other.__code__

        try:
            got = loop(Adder(), 3000, hook, 2000)
        finally:
            Adder.m.__code__ = saved
        want = expected(2000, lambda i: i + 1) + sum(i + 1000 for i in range(2000, 3000))
        self.assertEqual(got, want)
        self.assertEqual(loop(Adder(), 3000), expected(3000, lambda i: i + 1))

    def test_subclass_receivers(self):
        class Sub(Adder):
            def m(self, a):
                return a + 2

        class Inherits(Adder):
            pass

        self.assertEqual(loop(Sub(), 3000), expected(3000, lambda i: i + 2))
        self.assertEqual(loop(Inherits(), 3000), expected(3000, lambda i: i + 1))
        self.assertEqual(loop(Adder(), 3000), expected(3000, lambda i: i + 1))

    def test_method_raises(self):
        o = Adder()
        values = [5] * 3000
        for _ in range(5):
            self.assertEqual(div_loop(o, values), 60000)
        values[2500] = 0
        try:
            div_loop(o, values)
        except ZeroDivisionError as error:
            tb = error.__traceback__
        else:
            self.fail('division by zero did not raise')
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[-2:], ['div_loop', 'div'])

    def test_overflow_promotes(self):
        o = Adder()
        big = 2**63 - 1

        def big_loop(n):
            t = 0
            for i in range(n):
                t += o.m(big - n + i + 1)
            return t

        for _ in range(5):
            big_loop(1000)
        self.assertEqual(big_loop(1000), sum(big - 1000 + i + 2 for i in range(1000)))

    def test_recursion_limit(self):
        o = Adder()

        def deep(k):
            if k == 0:
                return loop(o, 50)
            return deep(k - 1)

        limit = sys.getrecursionlimit()
        sys.setrecursionlimit(200)
        try:
            for k in range(150, 220):
                try:
                    deep(k)
                except RecursionError:
                    break
            else:
                self.fail('the recursion limit never fired')
        finally:
            sys.setrecursionlimit(limit)
        self.assertEqual(loop(o, 3000), expected(3000, lambda i: i + 1))

    def test_profiler_sees_every_call(self):
        o = Adder()
        calls = []

        def profile(frame, event, arg):
            if event == 'call' and frame.f_code is Adder.m.__code__:
                calls.append(1)

        sys.setprofile(profile)
        try:
            got = loop(o, 2000)
        finally:
            sys.setprofile(None)
        self.assertEqual(got, expected(2000, lambda i: i + 1))
        self.assertEqual(len(calls), 2000)


class Box:
    def __init__(self, w, h):
        self.w = w
        self.h = h

    def area(self):
        return self.w * self.h

    def add(self, a):
        return self.w + a

    def ratio(self):
        return self.w / self.h


def area_loop(b, n, hook=None, at=-1):
    t = 0
    for i in range(n):
        if i == at:
            hook()
        t += b.area()
    return t


def add_loop(b, n):
    t = 0
    for i in range(n):
        t += b.add(i)
    return t


def ratio_loop(b, n):
    t = 0.0
    for i in range(n):
        t += b.ratio()
    return t


def field_names(error):
    names = []
    tb = error.__traceback__
    while tb is not None:
        names.append(tb.tb_frame.f_code.co_name)
        tb = tb.tb_next
    return names


for _ in range(40):
    area_loop(Box(3, 4), 200)
    add_loop(Box(3, 4), 200)
    ratio_loop(Box(3.0, 4.0), 200)


class InlineFieldMethods(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(area_loop(Box(3, 4), 5000), 60000)
        self.assertEqual(add_loop(Box(3, 4), 5000), 3 * 5000 + sum(range(5000)))
        self.assertEqual(ratio_loop(Box(3.0, 4.0), 4000), 3000.0)

    def test_field_lane_changes_mid_loop(self):
        b = Box(3, 4)

        def hook():
            b.w = 2.5

        self.assertEqual(area_loop(b, 3000, hook, 1000), 12 * 1000 + 10.0 * 2000)
        b.w = 'ab'
        b.h = 2
        with self.assertRaises(TypeError):
            area_loop(b, 3)

    def test_field_overflow(self):
        b = Box(2**62, 4)
        self.assertEqual(area_loop(b, 1000), 1000 * 2**64)

    def test_field_deleted_mid_loop(self):
        b = Box(3, 4)

        def hook():
            del b.w

        try:
            area_loop(b, 3000, hook, 1500)
        except AttributeError as error:
            self.assertEqual(field_names(error)[-2:], ['area_loop', 'area'])
        else:
            self.fail('a deleted field did not raise')

    def test_property_added_mid_loop(self):
        class Local(Box):
            pass

        for _ in range(40):
            area_loop(Local(3, 4), 200)
        b = Local(3, 4)

        def hook():
            Local.w = property(lambda self: 100)

        self.assertEqual(area_loop(b, 3000, hook, 1000), 12 * 1000 + 400 * 2000)

    def test_instance_dict_published(self):
        b = Box(3, 4)
        self.assertEqual(area_loop(b, 2000), 24000)
        b.__dict__['w'] = 5
        self.assertEqual(area_loop(b, 2000), 40000)
        vars(b)['h'] = 1
        self.assertEqual(area_loop(b, 2000), 10000)

    def test_class_reassigned_mid_loop(self):
        class Other:
            def area(self):
                return -1

        b = Box(3, 4)

        def hook():
            b.__class__ = Other

        self.assertEqual(area_loop(b, 3000, hook, 2000), 12 * 2000 - 1000)

    def test_method_shadowed_and_restored(self):
        b = Box(3, 4)

        def hook():
            b.area = lambda: 1

        self.assertEqual(area_loop(b, 3000, hook, 1000), 12 * 1000 + 2000)
        del b.area
        self.assertEqual(area_loop(b, 3000), 36000)

    def test_more_fields_later(self):
        b = Box(3, 4)
        self.assertEqual(area_loop(b, 2000), 24000)
        b.z = 1
        b.zz = 2
        self.assertEqual(area_loop(b, 2000), 24000)
        b.area = lambda: 7
        self.assertEqual(area_loop(b, 2000), 14000)

    def test_zero_division(self):
        b = Box(3.0, 4.0)
        self.assertEqual(ratio_loop(b, 1000), 750.0)
        b.h = 0.0
        try:
            ratio_loop(b, 1000)
        except ZeroDivisionError as error:
            self.assertEqual(field_names(error)[-2:], ['ratio_loop', 'ratio'])
        else:
            self.fail('division by zero did not raise')

    def test_profiler_sees_every_call(self):
        b = Box(3, 4)
        calls = []

        def profile(frame, event, arg):
            if event == 'call' and frame.f_code is Box.area.__code__:
                calls.append(1)

        sys.setprofile(profile)
        try:
            got = area_loop(b, 2000)
        finally:
            sys.setprofile(None)
        self.assertEqual(got, 24000)
        self.assertEqual(len(calls), 2000)

    def test_recursion_limit(self):
        b = Box(3, 4)

        def deep(k):
            if k == 0:
                return area_loop(b, 50)
            return deep(k - 1)

        limit = sys.getrecursionlimit()
        sys.setrecursionlimit(200)
        try:
            for k in range(150, 220):
                try:
                    deep(k)
                except RecursionError:
                    break
            else:
                self.fail('the recursion limit never fired')
        finally:
            sys.setrecursionlimit(limit)


class WithoutInlineLayout(unittest.TestCase):
    """The same calls where compiled code reads no object layout in line:
    each guard then runs in the method enter helper."""

    @unittest.skipIf(
        sys.implementation.name != 'weavepy' or os.environ.get('WEAVEPY_JIT_NO_INLINE_ATTRS'),
        'WeavePy only, once',
    )
    def test_helper_guarded_calls(self):
        env = dict(os.environ, WEAVEPY_JIT_NO_INLINE_ATTRS='1')
        proc = subprocess.run(
            [sys.executable, __file__],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr[-2000:])


if __name__ == '__main__':
    unittest.main()
