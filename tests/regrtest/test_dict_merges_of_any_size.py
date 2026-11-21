"""Merging one dict into another, whatever their sizes.

`d.update(other)`, `d |= other`, `d | other`, `{**d, **other}`,
`dict(d)` and `d.copy()` give the same dict whether the two are small or
large, whether the keys are strings, ints, tuples or objects with their
own `__eq__`, and whether `other` is a dict or an iterable of pairs. An
existing key keeps its place and its key object and takes the new value;
new keys follow in the source's order.
"""

import unittest


class Key:
    """A key whose hash and equality are Python code."""

    calls = 0

    def __init__(self, name):
        self.name = name

    def __hash__(self):
        return hash(self.name)

    def __eq__(self, other):
        Key.calls += 1
        return isinstance(other, Key) and self.name == other.name

    def __repr__(self):
        return f"Key({self.name!r})"


def model_merge(dst_items, src_items):
    """The merged items, by the language's rules, on lists of pairs."""
    out = []
    for k, v in list(dst_items) + list(src_items):
        for i, (ok, _) in enumerate(out):
            if ok == k:
                out[i] = (ok, v)
                break
        else:
            out.append((k, v))
    return out


def sources():
    yield "strs", [(f"k{i}", i) for i in range(40)], [(f"k{i}", -i) for i in range(20, 70)]
    yield "ints", [(i, i) for i in range(40)], [(i, -i) for i in range(20, 70)]
    yield "mixed numbers", [(1, "a"), (2.0, "b"), (True, "c")], [(1.0, "x"), (2, "y"), (3, "z")]
    yield "tuples", [((i, str(i)), i) for i in range(30)], [((i, str(i)), -i) for i in range(15, 45)]
    yield "objects", [(Key(i), i) for i in range(30)], [(Key(i), -i) for i in range(15, 45)]
    yield "strs then objects", (
        [(f"k{i}", i) for i in range(30)] + [(Key(i), i) for i in range(5)]
    ), [(f"k{i}", -i) for i in range(20, 40)] + [(Key(3), "new"), ("tail", 1), (Key(9), 2)]
    yield "into empty", [], [(f"k{i}", i) for i in range(50)]
    yield "from empty", [(f"k{i}", i) for i in range(50)], []
    yield "one each", [("a", 1)], [("a", 2)]
    yield "large", [(f"k{i}", i) for i in range(3000)], [(f"k{i}", -i) for i in range(1500, 4500)]


MERGERS = {
    "update": lambda d, s: (d.update(s), d)[1],
    "ior": lambda d, s: d.__ior__(s),
    "or": lambda d, s: d | s,
    "unpack": lambda d, s: {**d, **s},
    "update pairs": lambda d, s: (d.update(list(s.items())), d)[1],
    "update zip": lambda d, s: (d.update(zip(s.keys(), s.values())), d)[1],
    "update lists": lambda d, s: (d.update([k, v] for k, v in s.items()), d)[1],
}


class MergeTests(unittest.TestCase):
    def test_merges_match_the_model(self):
        for label, dst_items, src_items in sources():
            want = model_merge(dst_items, src_items)
            for name, merge in MERGERS.items():
                with self.subTest(label=label, merge=name):
                    d = dict(dst_items)
                    s = dict(src_items)
                    out = merge(d, s)
                    self.assertEqual(list(out.items()), want)
                    self.assertEqual(len(out), len(want))
                    # The source is as it was.
                    self.assertEqual(list(s.items()), list(dict(src_items).items()))
                    for k, v in want:
                        self.assertEqual(out[k], v)

    def test_an_existing_key_object_is_kept(self):
        a, b = 1, 1.0
        d = {a: "int"}
        d.update({b: "float"})
        self.assertEqual(list(d.items()), [(1, "float")])
        self.assertIs(type(next(iter(d))), int)
        first = Key("x")
        d = {first: 0}
        d |= {Key("x"): 1}
        self.assertIs(next(iter(d)), first)
        self.assertEqual(d[first], 1)

    def test_in_place_merges_keep_the_dict(self):
        d = {"a": 1}
        alias = d
        d |= {"b": 2}
        self.assertIs(d, alias)
        self.assertEqual(alias, {"a": 1, "b": 2})
        self.assertIsNone(d.update({"c": 3}))
        self.assertEqual(alias, {"a": 1, "b": 2, "c": 3})
        merged = d | {"d": 4}
        self.assertIsNot(merged, d)
        self.assertNotIn("d", d)

    def test_a_dict_merged_into_itself(self):
        d = {f"k{i}": i for i in range(100)}
        before = list(d.items())
        d.update(d)
        self.assertEqual(list(d.items()), before)
        d |= d
        self.assertEqual(list(d.items()), before)
        self.assertEqual(d | d, d)
        # (Not a change: an iteration in progress goes on.)
        seen = 0
        for _ in d:
            d.update(d)
            d |= d
            seen += 1
        self.assertEqual(seen, 100)

    def test_new_keys_stop_an_iteration_and_new_values_do_not(self):
        d = {f"k{i}": i for i in range(50)}
        same_keys = {f"k{i}": -i for i in range(50)}
        seen = 0
        for _ in d:
            d.update(same_keys)
            d |= same_keys
            seen += 1
        self.assertEqual(seen, 50)
        self.assertEqual(d, same_keys)
        with self.assertRaises(RuntimeError):
            for _ in d:
                d.update({"new": 1})
        d = {"a": 1}
        with self.assertRaises(RuntimeError):
            for _ in d:
                d |= {"b": 1}

    def test_copies_are_independent(self):
        for n in (0, 1, 5, 9, 200):
            with self.subTest(n=n):
                d = {f"k{i}": [i] for i in range(n)}
                for copy in (dict(d), d.copy(), {**d}, d | {}):
                    self.assertEqual(copy, d)
                    self.assertIsNot(copy, d)
                    self.assertEqual(list(copy), list(d))
                    self.assertTrue(all(copy[k] is d[k] for k in d))
                    copy["extra"] = 1
                    self.assertNotIn("extra", d)
                    if n:
                        del copy["k0"]
                        self.assertIn("k0", d)

    def test_a_copy_of_a_dict_with_removed_keys(self):
        d = {i: str(i) for i in range(300)}
        for i in range(0, 300, 3):
            del d[i]
        copy = d.copy()
        self.assertEqual(list(copy.items()), list(d.items()))
        self.assertEqual(len(copy), 200)
        copy[0] = "back"
        self.assertEqual(list(copy)[-1], 0)
        self.assertNotIn(0, d)
        target = {}
        target.update(d)
        self.assertEqual(list(target.items()), list(d.items()))

    def test_values_replaced_and_dropped(self):
        import weakref

        class Value:
            pass

        old = Value()
        ref = weakref.ref(old)
        d = {f"k{i}": i for i in range(100)}
        d["k50"] = old
        del old
        self.assertIsNotNone(ref())
        d.update({"k50": 1, "k200": 2})
        self.assertIsNone(ref())
        self.assertEqual(d["k50"], 1)

    def test_keyword_updates(self):
        d = {"a": 1}
        d.update(a=2, b=3)
        self.assertEqual(d, {"a": 2, "b": 3})
        d.update({"c": 4}, a=5)
        self.assertEqual(list(d.items()), [("a", 5), ("b", 3), ("c", 4)])
        d.update([("z", 0)], y=1)
        self.assertEqual(list(d)[-2:], ["z", "y"])
        base = {f"k{i}": i for i in range(50)}
        built = dict(base, k1="one", extra=True)
        self.assertEqual(len(built), 51)
        self.assertEqual(built["k1"], "one")
        self.assertEqual(list(built)[:50], list(base))
        self.assertEqual(base["k1"], 1)


class PairSourceTests(unittest.TestCase):
    def test_pairs_of_any_sequence_kind(self):
        d = {}
        d.update([("a", 1), ["b", 2], "c3", iter(("d", 4)), {"e": 0, 5: 0}, range(6, 8)])
        self.assertEqual(d, {"a": 1, "b": 2, "c": "3", "d": 4, "e": 5, 6: 7})

    def test_a_pair_of_the_wrong_length(self):
        for bad, n in ((("a",), 1), (("a", 1, 2), 3), ([], 0), ([1, 2, 3, 4], 4), ("abc", 3)):
            with self.subTest(bad=bad):
                d = {"keep": 1}
                with self.assertRaises(ValueError) as cm:
                    d.update([("ok", 1), bad])
                self.assertEqual(
                    str(cm.exception),
                    f"dictionary update sequence element #1 has length {n}; 2 is required",
                )
                # The pairs before the bad one went in.
                self.assertEqual(d, {"keep": 1, "ok": 1})

    def test_an_item_that_is_not_a_sequence(self):
        d = {}
        with self.assertRaises(TypeError):
            d.update([("a", 1), 5])
        self.assertEqual(d, {"a": 1})

    def test_an_unhashable_key_in_a_pair(self):
        d = {}
        with self.assertRaises(TypeError):
            d.update([("a", 1), ([], 2), ("b", 3)])
        self.assertEqual(d, {"a": 1})

    def test_a_list_pair_changed_by_a_key(self):
        # A pair is read before its key is hashed.
        pair = [None, "value"]

        class Sly:
            def __hash__(self):
                pair.append("extra")
                return 1

        pair[0] = Sly()
        d = {}
        d.update([pair])
        self.assertEqual(list(d.values()), ["value"])

    def test_the_constructor_reads_the_same_pairs(self):
        for label, dst_items, src_items in sources():
            pairs = list(dst_items) + list(src_items)
            want = model_merge(pairs, [])
            for name, build in (
                ("list of tuples", lambda: dict(pairs)),
                ("tuple of lists", lambda: dict(tuple([k, v] for k, v in pairs))),
                ("generator", lambda: dict((k, v) for k, v in pairs)),
                ("zip", lambda: dict(zip([k for k, _ in pairs], [v for _, v in pairs]))),
                ("items view", lambda: dict(dict(pairs).items())),
                ("update list", lambda: (lambda d: (d.update(pairs), d)[1])({})),
                ("update items view", lambda: (lambda d: (d.update(dict(pairs).items()), d)[1])({})),
                ("mixed pairs", lambda: dict([p if i % 3 else list(p) for i, p in enumerate(pairs)])),
            ):
                with self.subTest(label=label, build=name):
                    self.assertEqual(list(build().items()), want)

    def test_constructor_errors(self):
        with self.assertRaises(ValueError) as cm:
            dict([("a", 1), ("b", 2, 3)])
        self.assertEqual(
            str(cm.exception),
            "dictionary update sequence element #1 has length 3; 2 is required",
        )
        with self.assertRaises(TypeError):
            dict([("a", 1), ([], 2)])
        with self.assertRaises(TypeError):
            dict([("a", 1), 7])
        self.assertEqual(dict(["ab", ("c", "d"), ["e", "f"]]), {"a": "b", "c": "d", "e": "f"})

    def test_an_items_view_of_the_dict_itself(self):
        d = {f"k{i}": i for i in range(30)}
        before = list(d.items())
        d.update(d.items())
        self.assertEqual(list(d.items()), before)
        copy = dict(d.items())
        self.assertEqual(copy, d)
        self.assertIsNot(copy, d)
        copy["new"] = 1
        self.assertNotIn("new", d)
        with self.assertRaises((TypeError, ValueError)):
            dict({"ab": 1, "cde": 2}.keys())
        self.assertEqual(dict({"ab": 1, "cd": 2}.keys()), {"a": "b", "c": "d"})

    def test_many_pairs(self):
        keys = [f"k{i}" for i in range(5000)]
        d = {}
        d.update(zip(keys, range(5000)))
        self.assertEqual(len(d), 5000)
        self.assertEqual(list(d)[:3], ["k0", "k1", "k2"])
        self.assertEqual(d["k4999"], 4999)
        d.update((k, -v) for k, v in list(d.items()))
        self.assertEqual(d["k4999"], -4999)
        self.assertEqual(list(d)[:3], ["k0", "k1", "k2"])
        d.update(enumerate(keys))
        self.assertEqual(len(d), 10000)
        self.assertEqual(d[17], "k17")


class PythonKeyTests(unittest.TestCase):
    def test_a_key_that_changes_the_source_stops_the_merge(self):
        class Mutator:
            def __init__(self, target):
                self.target = target

            def __hash__(self):
                return 7

            def __eq__(self, other):
                self.target.clear()
                return False

        src = {}
        src["a"] = 1
        src[Mutator(src)] = 2
        dst = {"z": 0}

        class Seven:
            def __hash__(self):
                return 7

            def __eq__(self, other):
                return NotImplemented

        dst[Seven()] = 1
        with self.assertRaises(RuntimeError):
            dst.update(src)

    def test_python_keys_compare_as_often_as_needed(self):
        d = {Key(i): i for i in range(20)}
        s = {Key(i): -i for i in range(10, 30)}
        Key.calls = 0
        d.update(s)
        self.assertEqual(len(d), 30)
        self.assertGreaterEqual(Key.calls, 10)
        self.assertEqual([k.name for k in d], list(range(30)))
        self.assertEqual([d[Key(i)] for i in (0, 10, 29)], [0, -10, -29])


class SplatTests(unittest.TestCase):
    def test_displays(self):
        for label, dst_items, src_items in sources():
            src = dict(src_items)
            with self.subTest(label=label):
                self.assertEqual(list({**src}.items()), list(src.items()))
                self.assertIsNot({**src}, src)
                both = {**dict(dst_items), **src}
                self.assertEqual(list(both.items()), model_merge(dst_items, src_items))
                self.assertEqual(list({"first": 0, **src}.items())[0], ("first", 0))
                self.assertEqual({**src, **src}, src)
        big = {str(i): i for i in range(5000)}
        copy = {**big}
        copy["new"] = 1
        self.assertNotIn("new", big)
        self.assertEqual(list(copy)[:3], ["0", "1", "2"])
        self.assertEqual({**{1: "x"}, 1.0: "y"}, {1: "y"})

    def test_keyword_splats(self):
        def f(**kw):
            return kw

        kw = {f"k{i}": i for i in range(300)}
        got = f(**kw)
        self.assertEqual(got, kw)
        self.assertIsNot(got, kw)
        got["extra"] = 1
        self.assertNotIn("extra", kw)
        self.assertEqual(f(a=1, **{"b": 2}), {"a": 1, "b": 2})
        with self.assertRaises(TypeError):
            f(**{"x": 1}, x=2)
        with self.assertRaises(TypeError):
            f(**{"a": 1}, **{"a": 2})
        with self.assertRaises(TypeError):
            f(**{1: 2})
        with self.assertRaises(TypeError):
            f(**[1, 2])

    def test_mappings_that_are_not_dicts(self):
        import collections
        import os

        self.assertEqual({**collections.OrderedDict(a=1)}, {"a": 1})
        self.assertIs(type({**os.environ}), dict)

        class Mapping:
            def keys(self):
                return ["a", "b"]

            def __getitem__(self, k):
                return k.upper()

        self.assertEqual({**Mapping()}, {"a": "A", "b": "B"})
        with self.assertRaises(TypeError):
            {**1}


if __name__ == "__main__":
    unittest.main()
