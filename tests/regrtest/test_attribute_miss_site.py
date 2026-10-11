"""A `LOAD_ATTR` site that keeps failing raises its `AttributeError` early.

A site whose generic load failed cools down; while it does, a plain
instance with no such attribute anywhere gets the `AttributeError` the
default lookup ends with, without the resolution passes. Anything that
makes the attribute exist, or changes how it is looked up, must be seen
at once.
"""

import unittest


class Plain:
    pass


def read(o):
    return o.missing


def probe(o, n):
    hits = 0
    for _ in range(n):
        try:
            read(o)
        except AttributeError as e:
            assert e.name == "missing" and e.obj is o
        else:
            hits += 1
    return hits


class AttributeMissSiteTest(unittest.TestCase):
    def test_error_fields(self):
        o = Plain()
        for _ in range(300):
            with self.assertRaises(AttributeError) as cm:
                read(o)
        self.assertEqual(cm.exception.name, "missing")
        self.assertIs(cm.exception.obj, o)
        self.assertEqual(str(cm.exception), "'Plain' object has no attribute 'missing'")

    def test_instance_attribute_appears(self):
        o = Plain()
        self.assertEqual(probe(o, 300), 0)
        o.missing = 5
        self.assertEqual(probe(o, 10), 10)
        del o.missing
        self.assertEqual(probe(o, 10), 0)

    def test_class_attribute_appears(self):
        class C:
            pass

        o = C()
        self.assertEqual(probe(o, 300), 0)
        C.missing = 7
        self.assertEqual(probe(o, 10), 10)
        del C.missing
        self.assertEqual(probe(o, 10), 0)

    def test_getattr_hook_appears(self):
        class C:
            pass

        o = C()
        self.assertEqual(probe(o, 300), 0)
        C.__getattr__ = lambda self, name: 1
        self.assertEqual(probe(o, 10), 10)
        del C.__getattr__
        self.assertEqual(probe(o, 10), 0)

    def test_property_raising(self):
        calls = []

        class C:
            @property
            def missing(self):
                calls.append(1)
                raise AttributeError("from property")

        o = C()
        self.assertEqual(probe(Plain(), 300), 0)
        for _ in range(10):
            with self.assertRaises(AttributeError) as cm:
                read(o)
            self.assertEqual(str(cm.exception), "from property")
        self.assertEqual(len(calls), 10)

    def test_receivers_change(self):
        class Has:
            missing = "here"

        self.assertEqual(probe(Plain(), 300), 0)
        self.assertEqual(probe(Has(), 10), 10)
        self.assertEqual(probe(Plain(), 10), 0)


if __name__ == "__main__":
    unittest.main()
