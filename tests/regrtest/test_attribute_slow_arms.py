"""The slower arms of attribute resolution keep their exact semantics.

Module attributes, `inst.__dict__`, a `__getattr__` hook reached without
the default lookup's `AttributeError`, both forms of `super().name`, and a
native `__init__` reached through `super()` are each served by a cached or
direct path once warm. Changes to the module, the class, or the receiver's
shape must be seen at once.
"""

import types
import unittest


N = 300


class Plain:
    def __init__(self):
        self.a = 1


class Hooked:
    def __init__(self):
        self.a = 1

    def __getattr__(self, name):
        if name.startswith("x"):
            return name.upper()
        raise AttributeError(f"hooked {name}")


def read_module_attr(m, n):
    out = []
    for _ in range(n):
        out.append(m.value)
    return out


def read_dict(o, n):
    out = None
    for _ in range(n):
        out = o.__dict__
    return out


def read_hooked(o, n):
    out = []
    for _ in range(n):
        out.append(o.xy)
    return out


class ModuleAttrTest(unittest.TestCase):
    def test_rebinding_and_deletion(self):
        m = types.ModuleType("m")
        m.value = 1
        m.other = 2
        self.assertEqual(read_module_attr(m, N), [1] * N)
        m.value = 3
        self.assertEqual(read_module_attr(m, 5), [3] * 5)
        # Keys move: the site must not read a stale position.
        del m.other
        m.extra = 4
        self.assertEqual(read_module_attr(m, 5), [3] * 5)
        del m.value
        with self.assertRaises(AttributeError):
            read_module_attr(m, 1)
        m.value = 7
        self.assertEqual(read_module_attr(m, 5), [7] * 5)

    def test_another_module_at_the_same_site(self):
        a = types.ModuleType("a")
        b = types.ModuleType("b")
        a.value, b.pad, b.value = "a", 0, "b"
        self.assertEqual(read_module_attr(a, N), ["a"] * N)
        self.assertEqual(read_module_attr(b, 3), ["b"] * 3)
        self.assertEqual(read_module_attr(a, 3), ["a"] * 3)

    def test_module_getattr(self):
        m = types.ModuleType("m")
        m.__getattr__ = lambda name: "dyn:" + name
        self.assertEqual(read_module_attr(m, N), ["dyn:value"] * N)
        m.value = 1
        self.assertEqual(read_module_attr(m, 3), [1] * 3)


class InstanceDictTest(unittest.TestCase):
    def test_identity_and_updates(self):
        o = Plain()
        d = read_dict(o, N)
        self.assertIs(d, o.__dict__)
        self.assertEqual(d, {"a": 1})
        o.b = 2
        self.assertEqual(read_dict(o, 3), {"a": 1, "b": 2})

    def test_class_level_override_is_seen(self):
        o = Plain()
        read_dict(o, N)
        Plain.__dict__  # (the class's own mapping proxy is untouched)
        try:
            Plain.__dict__ = property(lambda self: "custom")
        except (AttributeError, TypeError):
            pass

        class Sub(Plain):
            @property
            def __dict__(self):
                return "custom"

        self.assertEqual(read_dict(Sub(), 3), "custom")
        self.assertEqual(read_dict(o, 3), {"a": 1})

    def test_slots_class_has_no_dict(self):
        class S:
            __slots__ = ("a",)

        read_dict(Plain(), N)
        with self.assertRaises(AttributeError):
            read_dict(S(), 1)

    def test_getattribute_override(self):
        class G(Plain):
            def __getattribute__(self, name):
                if name == "__dict__":
                    return "intercepted"
                return object.__getattribute__(self, name)

        read_dict(Plain(), N)
        self.assertEqual(read_dict(G(), 3), "intercepted")


class GetattrHookTest(unittest.TestCase):
    def test_hook_values_and_errors(self):
        o = Hooked()
        self.assertEqual(read_hooked(o, N), ["XY"] * N)
        with self.assertRaises(AttributeError) as cm:
            o.missing
        self.assertEqual(str(cm.exception), "hooked missing")
        self.assertEqual(cm.exception.name, "missing")
        self.assertIs(cm.exception.obj, o)
        self.assertFalse(hasattr(o, "missing"))
        self.assertEqual(getattr(o, "missing", 9), 9)

    def test_real_attributes_win(self):
        o = Hooked()
        read_hooked(o, N)
        o.xy = "own"
        self.assertEqual(read_hooked(o, 3), ["own"] * 3)
        del o.xy
        Hooked.xy = "class"
        try:
            self.assertEqual(read_hooked(o, 3), ["class"] * 3)
        finally:
            del Hooked.xy
        self.assertEqual(read_hooked(o, 3), ["XY"] * 3)

    def test_data_descriptor_raising(self):
        class D:
            @property
            def xy(self):
                raise AttributeError("from property")

            def __getattr__(self, name):
                return "hook:" + name

        self.assertEqual(read_hooked(D(), N), ["hook:xy"] * N)

    def test_hook_added_and_removed(self):
        class C:
            pass

        o = C()
        with self.assertRaises(AttributeError):
            read_hooked(o, 1)
        C.__getattr__ = lambda self, name: 1
        self.assertEqual(read_hooked(o, N), [1] * N)
        del C.__getattr__
        with self.assertRaises(AttributeError) as cm:
            read_hooked(o, 1)
        self.assertEqual(str(cm.exception), "'C' object has no attribute 'xy'")


class Base:
    def __new__(cls, *args):
        return super().__new__(cls)

    def __init__(self, x):
        self.x = x

    def m(self):
        return "base"


class Explicit(Base):
    def __new__(cls, *args):
        return super(Explicit, cls).__new__(cls)

    def __init__(self, x):
        super(Explicit, self).__init__(x * 2)

    def m(self):
        return "explicit+" + super(Explicit, self).m()


class SuperFormsTest(unittest.TestCase):
    def test_explicit_form(self):
        for i in range(N):
            e = Explicit(i)
            self.assertEqual(e.x, i * 2)
            self.assertEqual(e.m(), "explicit+base")

    def test_explicit_form_errors(self):
        def bad(cls, obj):
            return super(cls, obj).m

        for _ in range(N):
            bad(Explicit, Explicit(1))
        with self.assertRaises(TypeError):
            bad(Explicit, Base(1))
        with self.assertRaises(TypeError):
            bad(1, Explicit(1))

    def test_mro_change_is_seen(self):
        class A:
            def f(self):
                return "A"

        class B(A):
            def f(self):
                return super(B, self).f()

        b = B()
        for _ in range(N):
            self.assertEqual(b.f(), "A")
        A.f = lambda self: "A2"
        self.assertEqual(b.f(), "A2")


class ObjectInitTest(unittest.TestCase):
    def test_object_init_through_super(self):
        class O:
            def __init__(self, x):
                super().__init__()
                self.x = x

        for i in range(N):
            self.assertEqual(O(i).x, i)

    def test_object_init_excess_arguments(self):
        class O:
            def __init__(self, *args):
                super().__init__(*args)

        for _ in range(N):
            O()
        with self.assertRaises(TypeError) as cm:
            O(1)
        self.assertIn("object.__init__() takes exactly one argument", str(cm.exception))

    def test_dict_payload_at_a_settled_site(self):
        class B:
            def __init__(self, *args):
                super().__init__(*args)

        class D(dict, B):
            def __init__(self, *args):
                dict.__init__(self, *args)
                # `object.__init__` follows `B` in the MRO: the site sees
                # a dict payload after settling on plain instances.
                B.__init__(self)

        for _ in range(N):
            B()
        d = D([("k", 1)])
        self.assertEqual(d, {"k": 1})


if __name__ == "__main__":
    unittest.main()
