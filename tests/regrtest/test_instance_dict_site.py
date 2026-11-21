"""`obj.__dict__` from a site that has read it before.

Once a site has read an instance's managed dict for a class at its
version, it reads the dict again directly; another class, a class whose
version moved, a `__dict__` property, or a dict subclass's instance takes
the full read.
"""

import unittest

N = 3000


class A:
    def __init__(self):
        self.x = 1


class B:
    @property
    def __dict__(self):
        return {"fake": True}


class H:
    def __init__(self, o):
        self.o = o


class InstanceDictSiteTests(unittest.TestCase):
    def test_local_and_stack_reads(self):
        a = A()
        h = H(a)
        for _ in range(N):
            d = a.__dict__
            e = h.o.__dict__
        self.assertIs(d, e)
        self.assertEqual(d, {"x": 1})
        d["y"] = 2
        self.assertEqual(a.y, 2)

    def test_mixed_classes_at_one_site(self):
        objs = [A(), B(), A()]
        out = []
        for i in range(N):
            o = objs[i % 3]
            out.append(o.__dict__)
        self.assertEqual(out[0], {"x": 1})
        self.assertEqual(out[1], {"fake": True})

    def test_class_change(self):
        class C:
            pass

        c = C()
        c.v = 1
        out = []
        for i in range(N):
            if i == N // 2:
                C.other = 1
                c.w = 2
            out.append(c.__dict__)
        self.assertEqual(out[0], {"v": 1, "w": 2})
        self.assertIs(out[-1], out[0])
        self.assertEqual(c.__dict__, {"v": 1, "w": 2})


if __name__ == "__main__":
    unittest.main()
