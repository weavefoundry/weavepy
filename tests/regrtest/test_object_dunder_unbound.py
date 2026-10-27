"""`object`'s own `__init__` and `__new__` called unbound on any receiver.

`object.__init__` shares its name with the built-in types' `__init__`s
(`dict.__init__` merges its arguments), and `object.__new__` with
`type.__new__` (which builds a class). Called through `object`, each must
run object's own policy: `object.__init__` with surplus arguments raises
when the receiver's type overrides `__init__` (a dict's, a list's) and
leaves the receiver alone, and `object.__new__` refuses a metaclass.
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


if __name__ == "__main__":
    unittest.main()
