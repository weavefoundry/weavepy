"""Behavior at the native collections boundaries: named tuple fields and
construction, OrderedDict, defaultdict, Counter tallies, ChainMap lookups,
and dict-subclass subscripts. Each operation runs in a loop so the
interpreter's cached fast paths serve it, and the results must not depend
on which path ran."""

import copy
import pickle
import unittest
from collections import ChainMap, Counter, OrderedDict, defaultdict, namedtuple

Point = namedtuple("Point", "x y z", defaults=(0,))


class Slotted(OrderedDict):
    __slots__ = ("x",)


def repeat(fn, n=60):
    out = None
    for _ in range(n):
        out = fn()
    return out


class NamedTupleTests(unittest.TestCase):
    def test_fields(self):
        p = Point(1, 2, 3)
        self.assertEqual(repeat(lambda: (p.x, p.y, p.z)), (1, 2, 3))
        self.assertEqual(type(Point.x).__name__, "_tuplegetter")
        self.assertEqual(Point.x.__doc__, "Alias for field number 0")
        self.assertFalse(hasattr(Point.x, "index"))
        with self.assertRaises(AttributeError):
            p.x = 5
        with self.assertRaises(AttributeError):
            del p.x

    def test_field_ignores_getitem_override(self):
        class P(Point):
            def __getitem__(self, i):
                return "overridden"

        p = P(1, 2, 3)
        self.assertEqual(repeat(lambda: (p.x, p[0])), (1, "overridden"))

    def test_descriptor_get(self):
        get = Point.y.__get__
        self.assertEqual(get((7, 8, 9)), 8)
        self.assertIs(get(None, Point), Point.y)
        with self.assertRaises(TypeError):
            get(5)
        with self.assertRaises(IndexError):
            get((1,))
        clone = pickle.loads(pickle.dumps(Point.z))
        self.assertEqual(clone.__get__(Point(1, 2, 3)), 3)
        self.assertEqual(clone.__doc__, Point.z.__doc__)

    def test_field_after_class_change(self):
        class P(Point):
            pass

        p = P(1, 2, 3)
        self.assertEqual(repeat(lambda: p.y), 2)
        P.y = property(lambda self: "prop")
        self.assertEqual(repeat(lambda: p.y), "prop")

    def test_construction(self):
        self.assertEqual(repeat(lambda: Point(1, 2, 3)), (1, 2, 3))
        self.assertEqual(repeat(lambda: Point(1, 2)), (1, 2, 0))
        self.assertEqual(repeat(lambda: Point(x=1, y=2, z=3)), (1, 2, 3))
        self.assertIs(type(repeat(lambda: Point(1, 2, 3))), Point)
        with self.assertRaises(TypeError):
            Point(1, 2, 3, 4)

        calls = []

        class WithInit(Point):
            def __init__(self, *args):
                calls.append(args)

        w = repeat(lambda: WithInit(1, 2, 3), 5)
        self.assertEqual(w, (1, 2, 3))
        self.assertEqual(len(calls), 5)

    def test_make_and_replace(self):
        p = Point(1, 2, 3)
        self.assertEqual(repeat(lambda: p._replace(y=5)), (1, 5, 3))
        self.assertEqual(repeat(lambda: Point._make([4, 5, 6])), (4, 5, 6))
        self.assertEqual(Point._make(iter((4, 5, 6))), (4, 5, 6))
        with self.assertRaisesRegex(TypeError, "Expected 3 arguments, got 2"):
            Point._make((1, 2))
        with self.assertRaisesRegex(TypeError, r"Got unexpected field names: \['w'\]"):
            p._replace(w=1)

        made = []

        class P(Point):
            @classmethod
            def _make(cls, iterable):
                made.append(cls)
                return super()._make(iterable)

        q = P(1, 2, 3)
        self.assertEqual(repeat(lambda: q._replace(x=9), 3), (9, 2, 3))
        self.assertEqual(made, [P] * 3)
        self.assertIs(type(q._replace(x=9)), P)


class OrderedDictTests(unittest.TestCase):
    def test_order_operations(self):
        def run():
            od = OrderedDict.fromkeys("abcde")
            od.move_to_end("b")
            od.move_to_end("d", last=False)
            od.move_to_end("c", False)
            first = od.popitem(last=False)
            last = od.popitem()
            od["z"] = 1
            return list(od), first, last, od.pop("a"), od.pop("q", "dflt")

        self.assertEqual(repeat(run), (["d", "a", "e", "z"], ("c", None), ("b", None), None, "dflt"))
        od = OrderedDict()
        with self.assertRaises(KeyError):
            od.popitem()
        with self.assertRaises(KeyError):
            od.move_to_end("x")
        with self.assertRaises(KeyError):
            od.pop("x")

    def test_views_and_reversed(self):
        od = OrderedDict([(i, i * i) for i in range(5)])
        od.move_to_end(2)
        self.assertEqual(repeat(lambda: list(od.items())),
                         [(0, 0), (1, 1), (3, 9), (4, 16), (2, 4)])
        self.assertEqual(list(od.values()), [0, 1, 9, 16, 4])
        self.assertEqual(list(reversed(od)), [2, 4, 3, 1, 0])
        self.assertEqual(list(reversed(od.items()))[0], (2, 4))
        self.assertEqual(repr(od.keys()), "odict_keys([0, 1, 3, 4, 2])")

    def test_mutation_during_iteration(self):
        for mutate in (lambda od, k: od.__setitem__("new", 1),
                       lambda od, k: od.__delitem__(k),
                       lambda od, k: od.move_to_end(k)):
            for view in (lambda od: od, OrderedDict.values, OrderedDict.items):
                od = OrderedDict.fromkeys("abc", 0)
                with self.assertRaisesRegex(RuntimeError, "OrderedDict mutated during iteration"):
                    for item in view(od):
                        mutate(od, "a")
        od = OrderedDict.fromkeys("abc", 0)
        for k in od:
            od[k] += 1  # replacing a value is fine
        self.assertEqual(od, {"a": 1, "b": 1, "c": 1})

    def test_dict_level_changes(self):
        od = OrderedDict.fromkeys("abc", 1)
        dict.__delitem__(od, "b")
        with self.assertRaises(KeyError):
            list(od.values())
        with self.assertRaises(KeyError):
            repr(od)
        od = OrderedDict.fromkeys("ab", 1)
        dict.__setitem__(od, "q", 2)
        self.assertEqual(list(od), ["a", "b"])
        self.assertEqual(od["q"], 2)

    def test_equality(self):
        a = OrderedDict.fromkeys("abc")
        b = OrderedDict.fromkeys("cba")
        self.assertEqual(a, dict(b))
        self.assertNotEqual(a, b)
        self.assertEqual(repeat(lambda: a == OrderedDict.fromkeys("abc")), True)

        class Key:
            count = 0

            def __hash__(self):
                return -1

            def __eq__(self, other):
                Key.count += 1
                if Key.count == 2:
                    one.clear()
                return True

        one = OrderedDict(dict.fromkeys((0, Key(), 4.2)))
        two = OrderedDict(dict.fromkeys((0, Key(), 4.2)))
        with self.assertRaisesRegex(RuntimeError, "OrderedDict mutated during iteration"):
            one == two

    def test_subclass_dispatch(self):
        log = []

        class Logged(OrderedDict):
            def __setitem__(self, key, value):
                log.append(key)
                super().__setitem__(key, value)

            def __getitem__(self, key):
                log.append(("get", key))
                return super().__getitem__(key)

        od = Logged([(1, 1)], b=2)
        od.update({3: 3})
        od.setdefault(4, 4)
        self.assertEqual(log, [1, "b", 3, 4])
        log.clear()
        self.assertEqual(od.pop(1), 1)
        self.assertEqual(od.popitem(), (4, 4))
        self.assertEqual(log, [])
        self.assertEqual(list(od.copy()), ["b", 3])

    def test_pickle_and_copy(self):
        for cls in (OrderedDict, Slotted):
            od = cls([("a", 1), ("b", 2)])
            od.move_to_end("a")
            if cls is Slotted:
                od.x = 5
            self.assertIsNone(OrderedDict(a=1).__reduce__()[2])
            for dup in (copy.copy(od), copy.deepcopy(od), pickle.loads(pickle.dumps(od))):
                self.assertEqual(list(dup.items()), [("b", 2), ("a", 1)])
                self.assertIs(type(dup), cls)
                if cls is Slotted:
                    self.assertEqual(dup.x, 5)
        it = iter(OrderedDict(a=1, b=2, c=3).items())
        next(it)
        self.assertEqual(list(pickle.loads(pickle.dumps(it))), [("b", 2), ("c", 3)])
        self.assertIsNone(OrderedDict(a=1).__getstate__())


class OrderedDictNamespaceTests(unittest.TestCase):
    """Consumers that copy an OrderedDict see its order, and namespace
    binds into one go through its __setitem__."""

    @staticmethod
    def user(ns):
        return [k for k in ns if not k.startswith("__")]

    def test_copies_follow_order(self):
        od = OrderedDict([("a", 1), ("b", 2), ("c", 3)])
        od.move_to_end("a")
        self.assertEqual(self.user(type("C", (), od).__dict__), ["b", "c", "a"])
        self.assertEqual(list(dict(od)), ["b", "c", "a"])
        self.assertEqual(list({**od}), ["b", "c", "a"])
        self.assertEqual((lambda **kw: list(kw))(**od), ["b", "c", "a"])
        d = {}
        d.update(od)
        self.assertEqual(list(d), ["b", "c", "a"])

    def test_exec_and_eval_namespaces(self):
        ns = OrderedDict()
        exec("x = 1\ndef f(): pass\nclass K: pass\nfrom os import sep\ny = 2", ns)
        self.assertEqual(self.user(ns), ["x", "f", "K", "sep", "y"])
        ns = OrderedDict()
        exec("from string import *", ns)
        self.assertIn("ascii_letters", self.user(ns))
        ns = OrderedDict(a=1)
        self.assertEqual(eval("(v := 3) + 1", ns), 4)
        self.assertEqual(self.user(ns), ["a", "v"])

    def test_prepare_namespace(self):
        class Meta(type):
            @classmethod
            def __prepare__(mcls, name, bases):
                return OrderedDict()

            def __new__(mcls, name, bases, ns):
                cls = super().__new__(mcls, name, bases, dict(ns))
                cls.order = [k for k in ns if not k.startswith("__")]
                return cls

        class C(metaclass=Meta):
            b = 1
            a = 2

            def m(self):
                pass

        self.assertEqual(C.order, ["b", "a", "m"])

    def test_dict_level_insert(self):
        # As in CPython, a key the payload gained behind the order's back
        # is present but not iterated.
        od = OrderedDict(a=1)
        dict.__setitem__(od, "q", 2)
        self.assertEqual((list(od), od["q"], len(od)), (["a"], 2, 2))


class DefaultDictTests(unittest.TestCase):
    def test_factories(self):
        def run():
            out = []
            for factory in (int, list, set, dict, float, str, tuple, lambda: "x"):
                d = defaultdict(factory)
                out.append(d["k"])
                d["k"]
            return out

        self.assertEqual(repeat(run), [0, [], set(), {}, 0.0, "", (), "x"])
        d = defaultdict()
        with self.assertRaises(KeyError) as cm:
            d["missing"]
        self.assertEqual(cm.exception.args, ("missing",))

    def test_increment_and_append(self):
        def run():
            counts = defaultdict(int)
            groups = defaultdict(list)
            for i in range(20):
                counts[i % 3] += 1
                groups[i % 2].append(i)
            return dict(counts), dict(groups)

        self.assertEqual(repeat(run)[0], {0: 7, 1: 7, 2: 6})

    def test_factory_fills_key(self):
        def factory():
            d["k"] = "filled"
            return "factory"

        d = defaultdict(factory)
        self.assertEqual(d["k"], "filled")

    def test_default_factory_member(self):
        d = defaultdict(list)
        del d.default_factory
        self.assertIsNone(d.default_factory)
        d.default_factory = 5
        self.assertEqual(d.default_factory, 5)
        self.assertIsNone(defaultdict.__new__(defaultdict).default_factory)
        with self.assertRaises(AttributeError):
            d.attr = 1
        with self.assertRaises(TypeError):
            defaultdict(5)

    def test_subclass_missing(self):
        class D(defaultdict):
            def __missing__(self, key):
                return "custom"

        d = D(int)
        self.assertEqual(repeat(lambda: d["x"]), "custom")
        self.assertNotIn("x", d)
        d.extra = 1  # a subclass has a __dict__

    def test_pickle_copy_repr(self):
        d = defaultdict(list, {"a": [1]})
        for dup in (d.copy(), copy.copy(d), copy.deepcopy(d), pickle.loads(pickle.dumps(d))):
            self.assertEqual(dup, d)
            self.assertIs(dup.default_factory, list)
        self.assertEqual(repr(d), "defaultdict(<class 'list'>, {'a': [1]})")
        r = defaultdict(None)
        r.default_factory = r.copy
        self.assertIsInstance(repr(r), str)


class CounterTests(unittest.TestCase):
    def test_tally(self):
        self.assertEqual(repeat(lambda: Counter("abracadabra")),
                         {"a": 5, "b": 2, "r": 2, "c": 1, "d": 1})
        c = Counter({"a": 1.5})
        c.update("aa")
        self.assertEqual(c["a"], 3.5)
        self.assertEqual(Counter(x % 3 for x in range(10)), {0: 4, 1: 3, 2: 3})
        with self.assertRaises(TypeError):
            Counter([[1]])

    def test_partial_tally_on_error(self):
        def gen():
            yield "a"
            yield "a"
            raise ValueError("boom")

        c = Counter()
        with self.assertRaises(ValueError):
            c.update(gen())
        self.assertEqual(c, {"a": 2})

    def test_subclass_setitem(self):
        seen = []

        class Logged(Counter):
            def __setitem__(self, key, value):
                seen.append((key, value))
                super().__setitem__(key, value)

        Logged("aab")
        self.assertEqual(seen, [("a", 1), ("a", 2), ("b", 1)])

    def test_increment(self):
        def run():
            c = Counter()
            for w in "the cat the dog the end".split():
                c[w] += 1
            return c.most_common(1)

        self.assertEqual(repeat(run), [("the", 3)])


class DictSubclassTests(unittest.TestCase):
    def test_class_changes_after_warmup(self):
        class D(dict):
            pass

        d = D(a=1)

        def get():
            return d["a"]

        def put():
            d["b"] = 2
            return d["b"]

        self.assertEqual(repeat(get), 1)
        self.assertEqual(repeat(put), 2)
        D.__getitem__ = lambda self, key: "patched"
        self.assertEqual(repeat(get), "patched")
        stored = []
        D.__setitem__ = lambda self, key, value: stored.append(key)
        repeat(put, 2)
        self.assertEqual(stored, ["b", "b"])
        D.__missing__ = lambda self, key: "missing"
        del D.__getitem__
        self.assertEqual(repeat(lambda: d["nope"]), "missing")

    def test_missing_and_errors(self):
        class M(dict):
            def __missing__(self, key):
                return key * 2

        m = M()
        self.assertEqual(repeat(lambda: m[21]), 42)
        with self.assertRaisesRegex(TypeError, "unhashable"):
            m[[1]]


class ChainMapTests(unittest.TestCase):
    def test_lookups(self):
        base = {"a": 1}
        cm = ChainMap({"b": 2}, base)
        self.assertEqual(repeat(lambda: (cm["a"], cm["b"], "a" in cm, "z" in cm)),
                         (1, 2, True, False))
        with self.assertRaises(KeyError):
            cm["z"]
        cm.maps = [{"a": "new"}]
        self.assertEqual(cm["a"], "new")

    def test_other_mappings(self):
        class Odd(Exception):
            pass

        class Mapping:
            def __getitem__(self, key):
                if key == "boom":
                    raise Odd
                raise KeyError(key)

            def __contains__(self, key):
                return key == "here"

        cm = ChainMap(Mapping(), defaultdict(lambda: "dd"))
        self.assertEqual(cm["x"], "dd")
        with self.assertRaises(Odd):
            cm["boom"]
        self.assertIn("here", cm)

        class Missing(ChainMap):
            def __missing__(self, key):
                return "fallback"

        self.assertEqual(repeat(lambda: Missing({})["k"]), "fallback")


if __name__ == "__main__":
    unittest.main()
