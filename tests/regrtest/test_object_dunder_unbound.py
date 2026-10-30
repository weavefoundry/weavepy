"""`object`'s own dunders called unbound on any receiver.

`object.__init__` shares its name with the built-in types' `__init__`s
(`dict.__init__` merges its arguments), and `object.__new__` with
`type.__new__` (which builds a class). Called through `object`, each must
run object's own policy: `object.__init__` with surplus arguments raises
when the receiver's type overrides `__init__` (a dict's, a list's) and
leaves the receiver alone, and `object.__new__` refuses a class that a
built-in type's own `__new__` allocates (a metaclass, `dict`, a `dict`
subclass). `object.__setattr__` and `object.__delattr__` on a value with
no `__dict__` raise `AttributeError`, and `object.__repr__` is the
identity repr of any receiver.
"""

import unittest

N = 300

INIT_MSG = "object.__init__() takes exactly one argument (the instance to initialize)"


class Q(dict):
    pass


class L(list):
    pass


class I(int):
    pass


class P:
    pass


class NewOnly:
    def __new__(cls, *args):
        return object.__new__(cls)


class M(type):
    pass


class PyNewMeta(type):
    def __new__(mcs, *args):
        return super().__new__(mcs, *args)


class SubPyNewMeta(PyNewMeta):
    pass


class ObjectInitTests(unittest.TestCase):
    def check_raises(self, msg, call):
        # Repeated, so warmed and specialized call sites decide too.
        for _ in range(N):
            with self.assertRaises(TypeError) as cm:
                call()
            self.assertEqual(str(cm.exception), msg)

    def test_dict_receiver_positional(self):
        d = {}
        self.check_raises(INIT_MSG, lambda: object.__init__(d, {"a": 1}))
        self.assertEqual(d, {})

    def test_dict_receiver_keywords(self):
        d = {}
        self.check_raises(INIT_MSG, lambda: object.__init__(d, a=1))
        self.assertEqual(d, {})

    def test_dict_subclass_receiver(self):
        q = Q()
        self.check_raises(INIT_MSG, lambda: object.__init__(q, {"a": 1}))
        self.check_raises(INIT_MSG, lambda: object.__init__(q, a=1))
        self.assertEqual(q, {})
        self.assertEqual(dict(q), {})

    def test_list_receivers(self):
        self.check_raises(INIT_MSG, lambda: object.__init__([], 1))
        lst = L([9])
        self.check_raises(INIT_MSG, lambda: object.__init__(lst, [1]))
        self.assertEqual(list(lst), [9])

    def test_plain_instance(self):
        p = P()
        self.check_raises(
            "P.__init__() takes exactly one argument (the instance to initialize)",
            lambda: object.__init__(p, 1),
        )
        self.check_raises(
            "P.__init__() takes exactly one argument (the instance to initialize)",
            lambda: object.__init__(p, a=1),
        )

    def test_lenient_receivers(self):
        # A type that overrides `__new__` but not `__init__` owns the
        # signature through `__new__`.
        for _ in range(N):
            self.assertIsNone(object.__init__(5, 1))
            self.assertIsNone(object.__init__(I(5), 1))
            self.assertIsNone(object.__init__(NewOnly(), 1))
            self.assertIsNone(object.__init__(NewOnly(), a=1))

    def test_no_arguments(self):
        for _ in range(N):
            self.assertIsNone(object.__init__({}))
            self.assertIsNone(object.__init__(Q()))
            self.assertIsNone(object.__init__(P()))
        with self.assertRaises(TypeError):
            object.__init__()

    def test_dict_init_still_merges(self):
        d = {}
        dict.__init__(d, {"b": 2})
        self.assertEqual(d, {"b": 2})
        q = Q()
        dict.__init__(q, c=3)
        self.assertEqual(q, {"c": 3})

    def test_construction_unchanged(self):
        self.assertEqual(Q(a=1), {"a": 1})
        self.assertEqual(L([1, 2]), [1, 2])
        self.assertEqual(I(5), 5)
        self.assertIsInstance(NewOnly(1, 2), NewOnly)
        with self.assertRaises(TypeError):
            P(1)


class ObjectNewTests(unittest.TestCase):
    def test_metaclass_refused(self):
        for _ in range(N):
            with self.assertRaises(TypeError) as cm:
                object.__new__(M, "X", (), {})
            self.assertEqual(
                str(cm.exception), "object.__new__(M) is not safe, use M.__new__()"
            )
        with self.assertRaises(TypeError) as cm:
            object.__new__(type)
        self.assertEqual(
            str(cm.exception), "object.__new__(type) is not safe, use type.__new__()"
        )
        with self.assertRaises(TypeError) as cm:
            object.__new__(SubPyNewMeta, "X", (), {})
        self.assertEqual(
            str(cm.exception),
            "object.__new__(SubPyNewMeta) is not safe, use type.__new__()",
        )

    def test_type_new_still_builds(self):
        X = type.__new__(M, "X", (), {})
        self.assertIs(type(X), M)
        self.assertEqual(X.__name__, "X")
        Y = SubPyNewMeta("Y", (), {})
        self.assertIs(type(Y), SubPyNewMeta)

    def test_plain_allocation(self):
        for _ in range(N):
            self.assertIs(type(object.__new__(P)), P)
            self.assertIs(type(object.__new__(NewOnly)), NewOnly)

    def test_builtin_types_refused(self):
        for cls, base in [
            (dict, "dict"),
            (int, "int"),
            (list, "list"),
            (str, "str"),
            (tuple, "tuple"),
            (Q, "Q"),
            (L, "L"),
            (I, "I"),
        ]:
            for _ in range(N // 10):
                with self.assertRaises(TypeError) as cm:
                    object.__new__(cls)
                self.assertEqual(
                    str(cm.exception),
                    f"object.__new__({cls.__name__}) is not safe, use {base}.__new__()",
                )

    def test_python_new_over_builtin_refused(self):
        class PyNewDict(dict):
            def __new__(cls):
                return object.__new__(cls)

        class Sub(PyNewDict):
            pass

        for cls in (PyNewDict, Sub):
            with self.assertRaises(TypeError) as cm:
                cls()
            self.assertEqual(
                str(cm.exception),
                f"object.__new__({cls.__name__}) is not safe, use dict.__new__()",
            )

    def test_abstract_class_refused(self):
        import abc

        class A(abc.ABC):
            @abc.abstractmethod
            def f(self):
                pass

        class Done(A):
            def f(self):
                return 1

        for _ in range(N):
            with self.assertRaises(TypeError) as cm:
                object.__new__(A)
            self.assertIn("abstract", str(cm.exception))
            self.assertIs(type(object.__new__(Done)), Done)

    def test_python_new_calls_object_new(self):
        class Path:
            __slots__ = ("parts",)

            def __new__(cls, *args, **kwargs):
                return object.__new__(cls)

            def __init__(self, *args):
                self.parts = args

        class Sub(Path):
            __slots__ = ()

        for i in range(N):
            p = Sub("a", i)
            self.assertIs(type(p), Sub)
            self.assertEqual(p.parts, ("a", i))
            self.assertEqual(type(p)("b").parts, ("b",))

    def test_builtin_construction_unchanged(self):
        for _ in range(N // 10):
            self.assertEqual(dict.__new__(dict), {})
            self.assertEqual(type(dict.__new__(Q)), Q)
            self.assertEqual(int.__new__(I, 7), 7)
            self.assertEqual(Q(a=1), {"a": 1})
            self.assertEqual(I(3), 3)


class ObjectSetattrTests(unittest.TestCase):
    def check(self, call, msg):
        for _ in range(N // 10):
            with self.assertRaises(AttributeError) as cm:
                call()
            self.assertEqual(str(cm.exception), msg)

    def test_dictless_receivers(self):
        d = {}
        self.check(
            lambda: object.__setattr__(d, "x", 1),
            "'dict' object has no attribute 'x' and no __dict__ for setting new attributes",
        )
        self.check(
            lambda: object.__delattr__(d, "x"),
            "'dict' object has no attribute 'x' and no __dict__ for setting new attributes",
        )
        self.check(
            lambda: object.__setattr__(5, "y", 1),
            "'int' object has no attribute 'y' and no __dict__ for setting new attributes",
        )
        self.check(
            lambda: object.__setattr__([], "y", 1),
            "'list' object has no attribute 'y' and no __dict__ for setting new attributes",
        )
        self.assertEqual(d, {})

    def test_read_only_names(self):
        self.check(
            lambda: object.__setattr__({}, "keys", 1),
            "'dict' object attribute 'keys' is read-only",
        )
        self.check(
            lambda: object.__delattr__({}, "keys"),
            "'dict' object attribute 'keys' is read-only",
        )

    def test_instances_and_types_unchanged(self):
        p = P()
        object.__setattr__(p, "x", 1)
        self.assertEqual(p.x, 1)
        object.__delattr__(p, "x")
        self.assertFalse(hasattr(p, "x"))
        with self.assertRaises(TypeError):
            object.__setattr__(int, "x", 1)


class ObjectReprTests(unittest.TestCase):
    def check(self, value, type_name):
        for _ in range(N // 10):
            text = object.__repr__(value)
            self.assertTrue(
                text.startswith(f"<{type_name} object at 0x"), (value, text)
            )
            self.assertTrue(text.endswith(">"), text)

    def test_builtin_values(self):
        self.check({}, "dict")
        self.check([1], "list")
        self.check(5, "int")
        self.check("s", "str")
        self.check(None, "NoneType")
        self.check(int, "type")

    def test_subclass_instances(self):
        self.check(Q(a=1), f"{__name__}.Q")
        self.check(L([1]), f"{__name__}.L")
        self.check(P(), f"{__name__}.P")

    def test_override_not_consulted(self):
        class R:
            def __repr__(self):
                return "R!"

        self.check(R(), f"{__name__}.{R.__qualname__}")
        self.assertEqual(repr(R()), "R!")

    def test_distinct_objects_distinct_text(self):
        a, b = {}, {}
        self.assertNotEqual(object.__repr__(a), object.__repr__(b))
        self.assertEqual(object.__repr__(a), object.__repr__(a))


if __name__ == "__main__":
    unittest.main()
