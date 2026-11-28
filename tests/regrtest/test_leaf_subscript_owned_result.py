"""A frameless evaluation that subscripts a container it built itself
(`return s.split(".")[0]`) returns an item that outlives the container:
the result stays correct once the call runs frameless, through functions,
properties, callbacks, and chained subscripts."""

import gc
import unittest


def head(s):
    return s.split(".")[0]


def last_pair(s):
    return s.rpartition(".")[2]


def nested(s):
    return (s.split("."), 1)[0][1]


def dget(k):
    return {"a": [k], "b": 2}["a"][0]


class P:
    def __init__(self, s):
        self.s = s

    @property
    def head(self):
        return self.s.split(".")[0]


class OwnedSubscript(unittest.TestCase):
    def test_functions(self):
        for i in range(3000):
            s = "x%d.y.z" % (i % 50)
            self.assertEqual(head(s), "x%d" % (i % 50))
            self.assertEqual(last_pair(s), "z")
            self.assertEqual(nested(s), "y")
            self.assertEqual(dget(i), i)
            if i % 500 == 0:
                gc.collect()

    def test_property_and_callbacks(self):
        ps = [P("a%d.b" % i) for i in range(50)]
        for _ in range(100):
            self.assertEqual([p.head for p in ps], ["a%d" % i for i in range(50)])
            self.assertEqual(list(map(head, [p.s for p in ps]))[-1], "a49")
            self.assertEqual(sorted(ps, key=lambda p: p.s.split(".")[0])[0].s, "a0.b")


if __name__ == "__main__":
    unittest.main()
