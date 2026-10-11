"""An instance's split attributes becoming its real `__dict__`.

The dictionary is built in one pass from the split values: its order,
lookups (small and large tables), later stores, deletes and identity
must be exactly a dictionary's.
"""

import unittest


def make(n, tag=0):
    class C:
        pass

    objs = []
    for k in range(3):
        o = C()
        for i in range(n):
            setattr(o, "a%d" % i, (tag, k, i))
        objs.append(o)
    return objs


class MaterializeTest(unittest.TestCase):
    def check(self, n):
        for o in make(n, n):
            d = o.__dict__
            self.assertIs(o.__dict__, d)
            names = ["a%d" % i for i in range(n)]
            self.assertEqual(list(d), names)
            for i, name in enumerate(names):
                self.assertEqual(d[name][2], i)
                self.assertIn(name, d)
            self.assertNotIn("missing", d)
            o.extra = 1
            self.assertEqual(d["extra"], 1)
            if n:
                del o.a0
                self.assertNotIn("a0", d)
                d["a0"] = "back"
                self.assertEqual(o.a0, "back")
                self.assertEqual(list(d)[-1], "a0")
            self.assertEqual(len(d), n + 1)
            self.assertEqual("%(extra)s" % d, "1")

    def test_sizes(self):
        for n in (0, 1, 7, 8, 9, 15, 16, 17, 29, 30, 31, 40):
            self.check(n)

    def test_vars_and_hash_collisions(self):
        class K:
            pass

        k = K()
        # Names whose hashes share low bits still land and resolve.
        names = ["x%d" % i for i in range(64)]
        for n in names:
            setattr(k, n, n)
        v = vars(k)
        self.assertEqual(sorted(v), sorted(names))
        for n in names:
            self.assertEqual(v[n], n)


if __name__ == "__main__":
    unittest.main()
