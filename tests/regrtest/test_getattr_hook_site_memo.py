"""A read site that a class's pure `__getattr__` answered tries the hook
first; reads the default lookup can answer, class changes, and hooks that
raise still behave as the full lookup does."""

import unittest


class G:
    def __init__(self):
        self.d = {"a": 1}

    def __getattr__(self, k):
        return self.d.get(k, 0)


class R:
    def __getattr__(self, k):
        if k == "boom":
            raise AttributeError(k)
        return len(k)


class HookSiteMemo(unittest.TestCase):
    def test_reads(self):
        g = G()
        for _ in range(500):
            self.assertEqual(g.a, 1)
            self.assertEqual(g.zz, 0)

    def test_instance_gains_attribute(self):
        g = G()
        out = []
        for i in range(400):
            if i == 200:
                g.a = 99
            out.append(g.a)
        self.assertEqual(out[:200], [1] * 200)
        self.assertEqual(out[200:], [99] * 200)
        del g.a
        self.assertEqual(g.a, 1)

    def test_class_gains_attribute(self):
        class H(G):
            pass
        h = H()
        for _ in range(300):
            self.assertEqual(h.q, 0)
        H.q = "class"
        self.assertEqual(h.q, "class")
        del H.q
        self.assertEqual(h.q, 0)
        H.__getattr__ = lambda self, k: "new"
        self.assertEqual(h.q, "new")

    def test_hook_raises(self):
        r = R()
        for _ in range(300):
            self.assertEqual(r.abc, 3)
            with self.assertRaises(AttributeError):
                r.boom
            self.assertEqual(getattr(r, "boom", "d"), "d")

    def test_mixed_receivers(self):
        class P:
            a = 5
        objs = [G(), P(), G(), P()]
        for _ in range(200):
            self.assertEqual([o.a for o in objs], [1, 5, 1, 5])


if __name__ == "__main__":
    unittest.main()
