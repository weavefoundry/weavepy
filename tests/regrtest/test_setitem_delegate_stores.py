"""Item stores into instances whose __setitem__ is exactly
self.<attr>[key] = value (a UserDict, a wrapper), or self.maps[0][key] = value (a ChainMap), from compiled loops: the
target dict updated and read back, an unhashable key's TypeError, a key
whose __hash__/__eq__ is Python, a target that isn't a dict (a list, a
dict subclass with its own __setitem__), the attribute rebound partway, a
class whose __setitem__ changes, and the value's old reference released,
each matching CPython."""

import collections
import unittest

log = []


class Wrap:
    def __init__(self, target):
        self.data = target

    def __setitem__(self, key, value):
        self.data[key] = value


class Loud(dict):
    def __setitem__(self, k, v):
        log.append(k)
        super().__setitem__(k, v)


class K:
    def __init__(self, k):
        self.k = k

    def __hash__(self):
        return hash(self.k)

    def __eq__(self, other):
        return isinstance(other, K) and self.k == other.k


class Gone:
    def __init__(self, n):
        self.n = n

    def __del__(self):
        log.append(("del", self.n))


def store(n, c, key="a"):
    for i in range(n):
        c[key] = i
    return c


class DelegateTests(unittest.TestCase):
    def test_dicts(self):
        for n in (1, 400):
            self.assertEqual(store(n, Wrap({})).data, {"a": n - 1})
            self.assertEqual(dict(store(n, collections.UserDict())), {"a": n - 1})
            self.assertEqual(store(n, Wrap({}), 5).data, {5: n - 1})
            self.assertEqual(store(n, Wrap({}), K(1)).data, {K(1): n - 1})
            cm = store(n, collections.ChainMap({}, {"b": 1}))
            self.assertEqual(cm.maps, [{"a": n - 1}, {"b": 1}])

    def test_other_targets(self):
        self.assertEqual(store(400, Wrap([0]), 0).data, [399])
        del log[:]
        w = store(400, Wrap(Loud()))
        self.assertEqual(len(log), 400)
        self.assertEqual(dict(w.data), {"a": 399})
        with self.assertRaises(TypeError):
            store(400, Wrap({}), [])

    def test_chainmap_edges(self):
        cm = collections.ChainMap()
        cm.maps = []
        with self.assertRaises(IndexError):
            store(400, cm)
        cm = collections.ChainMap(collections.UserDict())
        self.assertEqual(dict(store(400, cm).maps[0]), {"a": 399})

    def test_rebound_and_changed(self):
        w = Wrap({})
        store(400, w)
        first = w.data
        w.data = {}
        store(300, w)
        self.assertEqual((first, w.data), ({"a": 399}, {"a": 299}))

        class W(Wrap):
            pass

        x = W({})
        store(400, x)
        W.__setitem__ = lambda self, k, v: self.data.__setitem__(k, -v)
        self.assertEqual(store(300, x).data, {"a": -299})

    def test_release(self):
        del log[:]
        w = Wrap({})
        for i in range(300):
            w["x"] = Gone(i)
        self.assertEqual(len(log), 299)
        self.assertEqual(log[-1], ("del", 298))


if __name__ == "__main__":
    unittest.main()
