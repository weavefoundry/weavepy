"""object.__reduce_ex__ for plain instances, __dict__ and bound-method
reads, through copy and pickle."""

import copy
import copyreg
import pickle
import unittest


class Plain:
    def __init__(self):
        self.x = 1
        self.y = [1, 2]


class Empty:
    pass


class Slots:
    __slots__ = ("p",)

    def __init__(self):
        self.p = 3


class NewArgs:
    def __init__(self, v):
        self.v = v

    def __getnewargs__(self):
        return (self.v,)

    def __new__(cls, v=None):
        return super().__new__(cls)


class State:
    def __getstate__(self):
        return {"g": 1}


class ReduceExTest(unittest.TestCase):
    def test_plain_shapes(self):
        p = Plain()
        r = p.__reduce_ex__(4)
        self.assertIs(r[0], copyreg.__newobj__)
        self.assertEqual(r[1], (Plain,))
        self.assertIs(r[2], p.__dict__)
        self.assertEqual(r[3:], (None, None))
        self.assertIsNone(Empty().__reduce_ex__(2)[2])

    def test_other_shapes_unchanged(self):
        self.assertEqual(Slots().__reduce_ex__(4)[2], (None, {"p": 3}))
        self.assertEqual(NewArgs(5).__reduce_ex__(4)[1], (NewArgs, 5))
        self.assertEqual(State().__reduce_ex__(4)[2], {"g": 1})

    def test_round_trips(self):
        for obj in (Plain(), Empty(), Slots(), NewArgs(7), State()):
            for _ in range(3):
                c = copy.deepcopy(obj)
                p = pickle.loads(pickle.dumps(obj))
                self.assertIs(type(c), type(obj))
                self.assertIs(type(p), type(obj))
        c = copy.deepcopy(Plain())
        self.assertEqual((c.x, c.y), (1, [1, 2]))

    def test_dict_and_bound_method_reads(self):
        p = Plain()
        for _ in range(3):
            d = p.__dict__
            self.assertIs(d, p.__dict__)
        lst = []
        for i in range(3):
            append = lst.append
            append(i)
        self.assertEqual(lst, [0, 1, 2])
        self.assertIs(append.__self__, lst)
        self.assertEqual(type(append).__name__, "builtin_function_or_method")

        class P:
            @property
            def __dict__(self):
                return "prop"

        for _ in range(3):
            self.assertEqual(P().__dict__, "prop")
        with self.assertRaises(AttributeError):
            for _ in range(3):
                Slots().__dict__


if __name__ == "__main__":
    unittest.main()
