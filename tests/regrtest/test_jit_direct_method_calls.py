"""Compiled loops call a compiled method's body directly.

A method whose body is pure numeric work on its arguments runs from a
compiled caller without the method-call helper once the site's guard (the
receiver's class version, no instance attribute shadowing the name, the
function's `__code__`) holds. Each loop below runs long enough to compile,
then the class, the instance, the function or the arguments change under
the compiled code, and every change must reach the generic path with the
interpreter's result, error and traceback.
"""

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


if __name__ == '__main__':
    unittest.main()
