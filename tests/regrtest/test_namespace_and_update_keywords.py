"""types.SimpleNamespace(k=v) and d.update(k=v) from compiled loops: the
namespace's attributes, repr, equality, vars() and cycles through it; a
subclass of SimpleNamespace; update over existing keys (order kept, values
replaced), over a dict with non-string keys, through a bound method held
in a local, on a dict subclass, and with a key whose hash collides with an
instance key whose __eq__ runs Python code."""

import gc
import types
import unittest


class NS(types.SimpleNamespace):
    pass


class EqLog:
    """A key equal to the string 'a', hashing as it does, that logs its
    comparisons."""

    log = []

    def __hash__(self):
        return hash("a")

    def __eq__(self, other):
        EqLog.log.append(other)
        return other == "a"


def namespaces(n):
    out = None
    for i in range(n):
        ns = types.SimpleNamespace(a=i, b="x")
        sub = NS(a=1)
        out = (ns.a, ns.b, repr(types.SimpleNamespace(z=1, y=2)), vars(ns) == {"a": i, "b": "x"},
               ns == types.SimpleNamespace(a=i, b="x"), type(sub).__name__, sub.a)
        ns.self = ns
    return out


def updates(n):
    out = None
    for i in range(n):
        d = {"b": 0, "a": 0}
        d.update(a=i, c=2)
        e = {1: "one"}
        e.update(x=1)
        up = d.update
        up(b=5)
        out = (list(d.items()), e)
    return out


class Sub(dict):
    def __setitem__(self, k, v):
        super().__setitem__(k, v * 2)


def subclass_update(n):
    s = None
    for i in range(n):
        s = Sub()
        s.update(a=1)
    return dict(s)


def colliding(n):
    out = None
    for i in range(n):
        d = {EqLog(): 1}
        d.update(a=2)
        out = len(d)
    return out


class Tests(unittest.TestCase):
    def test_namespaces(self):
        got = namespaces(3000)
        self.assertEqual(got, (2999, "x", "namespace(z=1, y=2)", True, True, "NS", 1))
        gc.collect()

    def test_updates(self):
        self.assertEqual(updates(3000), ([("b", 5), ("a", 2999), ("c", 2)], {1: "one", "x": 1}))

    def test_subclass(self):
        # (dict.update doesn't call a subclass's __setitem__.)
        self.assertEqual(subclass_update(2000), {"a": 1})

    def test_colliding_key(self):
        EqLog.log.clear()
        self.assertEqual(colliding(1000), 1)
        self.assertEqual(len(EqLog.log), 1000)


if __name__ == "__main__":
    unittest.main()
