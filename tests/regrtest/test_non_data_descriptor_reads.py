"""Reads through a Python non-data descriptor.

A class attribute that is an instance of a class with a plain `__get__`
and no `__set__` or `__delete__` yields to the instance's own entry, and
otherwise runs its `__get__`: both in place in compiled code once warm.
`functools.cached_property` is the common case. Shadowing, deletion, a
descriptor class gaining `__set__`, and an `AttributeError` from `__get__`
falling back to `__getattr__` all read as CPython reads them.
"""

import functools
import unittest

N = 3000


class Get:
    calls = 0

    def __get__(self, inst, owner=None):
        Get.calls += 1
        return ("got", inst is None, owner.__name__)


class Host:
    v = Get()


class CP:
    made = 0

    @functools.cached_property
    def v(self):
        CP.made += 1
        return [self]


class Raising:
    def __get__(self, inst, owner=None):
        raise AttributeError("nope")


class Fallback:
    v = Raising()

    def __getattr__(self, name):
        return ("fallback", name)


class NonDataDescriptorTests(unittest.TestCase):
    def test_get_runs_without_instance_entry(self):
        h = Host()
        Get.calls = 0
        for _ in range(N):
            r = h.v
        self.assertEqual(r, ("got", False, "Host"))
        self.assertEqual(Get.calls, N)
        self.assertEqual(Host.v, ("got", True, "Host"))

    def test_instance_entry_shadows(self):
        h = Host()
        out = []
        for i in range(N):
            if i == N // 3:
                h.v = "own"
            if i == 2 * N // 3:
                del h.v
            out.append(h.v)
        self.assertEqual(out[0], ("got", False, "Host"))
        self.assertEqual(out[N // 3], "own")
        self.assertEqual(out[N // 2], "own")
        self.assertEqual(out[-1], ("got", False, "Host"))

    def test_descriptor_class_gains_set(self):
        class G:
            def __get__(self, inst, owner=None):
                return "descr"

        class K:
            v = G()

        k = K()
        k.__dict__["v"] = "own"
        out = []
        for i in range(N):
            if i == N // 2:
                G.__set__ = lambda self, inst, value: None
            out.append(k.v)
        self.assertEqual(out[0], "own")
        self.assertEqual(out[-1], "descr")

    def test_cached_property(self):
        CP.made = 0
        objs = []
        for _ in range(N):
            o = CP()
            first = o.v
            again = o.v
            self.assertIs(first, again)
            objs.append(o)
        self.assertEqual(CP.made, N)
        self.assertEqual(objs[-1].v, [objs[-1]])
        del objs[-1].v
        self.assertEqual(objs[-1].v, [objs[-1]])
        self.assertEqual(CP.made, N + 1)

    def test_attribute_error_falls_back(self):
        f = Fallback()
        for _ in range(N):
            r = f.v
        self.assertEqual(r, ("fallback", "v"))

    def test_slots_instance(self):
        class S:
            __slots__ = ("x",)
            v = Get()

        s = S()
        for _ in range(N):
            r = s.v
        self.assertEqual(r, ("got", False, "S"))


if __name__ == "__main__":
    unittest.main()
