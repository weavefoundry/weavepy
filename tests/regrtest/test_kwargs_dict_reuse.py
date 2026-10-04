"""A keyword leaf call's `**kwargs` dictionary may be reused across calls.

The dictionary of a call nothing else kept is emptied and serves the next
call. Each call must still see a fresh, correct dictionary, and one that
escaped (returned, stored, captured) must stay intact and distinct.
"""

import unittest

WARM = 3000


def total(a, **kw):
    return a + kw.get("delta", 0)


def count(**kw):
    return len(kw)


def echo(**kw):
    return kw


def add_key(**kw):
    kw["extra"] = 1
    return len(kw)


kept = []


def keep(**kw):
    kept.append(kw)
    return 0


class KwargsDictReuse(unittest.TestCase):
    def test_values_per_call(self):
        acc = 0
        for i in range(WARM):
            acc += total(i, delta=2)
        self.assertEqual(acc, sum(range(WARM)) + 2 * WARM)
        for i in range(WARM):
            self.assertEqual(count(a=i, b=i), 2)
            self.assertEqual(count(a=i), 1)

    def test_no_keys_leak_between_calls(self):
        for i in range(WARM):
            self.assertEqual(add_key(a=i), 2)
            self.assertEqual(count(b=i), 1)

    def test_returned_dicts_stay_distinct(self):
        seen = [echo(n=i) for i in range(WARM)]
        self.assertEqual([d["n"] for d in seen], list(range(WARM)))
        self.assertEqual(len({id(d) for d in seen}), WARM)

    def test_stored_dicts_stay_intact(self):
        kept.clear()
        for i in range(WARM):
            keep(n=i, m=-i)
        self.assertEqual([(d["n"], d["m"]) for d in kept], [(i, -i) for i in range(WARM)])

    def test_captured_dict(self):
        def make(**kw):
            return lambda: kw

        getters = [make(v=i) for i in range(WARM)]
        self.assertEqual([g()["v"] for g in getters], list(range(WARM)))

    def test_identity_inside_call(self):
        ids = []

        def ident(**kw):
            ids.append(id(kw))
            return kw

        held = [ident(x=i) for i in range(50)]
        self.assertEqual(len(set(ids)), len(held))


if __name__ == "__main__":
    unittest.main()
