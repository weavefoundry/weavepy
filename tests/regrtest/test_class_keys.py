"""Classes and plain instances as dict and set keys, and `type(x)`."""

import unittest


class Plain:
    pass


class HashingMeta(type):
    calls = 0

    def __hash__(cls):
        HashingMeta.calls += 1
        return 42

    def __eq__(cls, other):
        return cls is other or getattr(other, "tag", None) == "same"


class EqMeta(type):
    def __eq__(cls, other):
        return isinstance(other, str) and other == cls.__name__

    __hash__ = type.__hash__


class ClassKeysTest(unittest.TestCase):
    def test_type_of(self):
        def f(x):
            return type(x)

        for v in (1, "a", 1.5, None, [], {}, Plain(), Plain, True, b"x", (1,)):
            self.assertIs(f(v), v.__class__)

        class Sub(int):
            pass

        self.assertIs(f(Sub(3)), Sub)

    def test_type_of_wrong_arity(self):
        with self.assertRaises(TypeError):
            type()
        with self.assertRaises(TypeError):
            type(1, 2)

    def test_class_in_containers(self):
        atomic = frozenset({int, str, float})
        mutable = {int, str}
        mapping = {int: "i", str: "s"}

        def probe(cls):
            return (cls in atomic, cls in mutable, mapping.get(cls), mapping.get(cls, 0))

        self.assertEqual(probe(int), (True, True, "i", "i"))
        self.assertEqual(probe(float), (True, False, None, 0))
        self.assertEqual(probe(list), (False, False, None, 0))
        self.assertEqual(probe(Plain), (False, False, None, 0))
        mutable.add(Plain)
        self.assertIn(Plain, mutable)
        mutable.discard(Plain)
        self.assertNotIn(Plain, mutable)

    def test_metaclass_hash_runs(self):
        class K(metaclass=HashingMeta):
            pass

        HashingMeta.calls = 0
        d = {K: 1}
        s = {K}

        def probe():
            return K in d, K in s, d.get(K)

        self.assertEqual(probe(), (True, True, 1))

    def test_metaclass_eq_runs(self):
        class Name(metaclass=EqMeta):
            pass

        d = {Name: 1}
        self.assertIn(Name, d)
        self.assertEqual(d.get(Name), 1)

    def test_instances_as_keys(self):
        a, b = Plain(), Plain()
        s = {a}
        d = {a: 1}

        def probe(x):
            return x in s, x in d, d.get(x, "missing")

        self.assertEqual(probe(a), (True, True, 1))
        self.assertEqual(probe(b), (False, False, "missing"))

    def test_instance_with_eq_on_collision(self):
        class AlwaysEq:
            def __eq__(self, other):
                return True

            def __hash__(self):
                return hash(probe_key)

        probe_key = Plain()
        stored = AlwaysEq()
        d = {stored: "stored"}
        s = {stored}
        self.assertEqual(d.get(probe_key), "stored")
        self.assertIn(probe_key, s)

    def test_discard_releases_class(self):
        import gc
        import weakref

        class Temp:
            pass

        s = {Temp}
        r = weakref.ref(Temp)
        s.discard(Temp)
        del Temp
        gc.collect()
        self.assertIsNone(r())
        self.assertEqual(s, set())


if __name__ == "__main__":
    unittest.main()
