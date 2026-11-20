"""One attribute site read over three or more receiver classes.

A compiled read remembers two classes' field positions in line and up to
four more in the site's table. A read through the table must give what
the full lookup gives: after a class gains a property or a
`__getattribute__`, after an instance drops the attribute or publishes its
dict, for receivers in a local and receivers left on the stack.
"""

import unittest

ROUNDS = 3000


class Base:
    kind = "base"

    def __init__(self, n):
        self.dest = "d%d" % n
        self.nargs = n


class A(Base):
    pass


class B(Base):
    def __init__(self, n):
        self.extra = None
        super().__init__(n)


class C(Base):
    def __init__(self, n):
        self.first = 1
        self.second = 2
        super().__init__(n)


class D(Base):
    __slots__ = ("slot",)

    def __init__(self, n):
        self.slot = n
        super().__init__(n)


class E(Base):
    def __init__(self, n):
        super().__init__(n)
        self.tail = n


def read_local(objs):
    total = 0
    names = []
    for o in objs:
        total += o.nargs
        names.append(o.dest)
    return total, names


def make(n):
    return [A(n), B(n + 1), C(n + 2), D(n + 3), E(n + 4)][n % 5]


def read_stack(n):
    # The receiver is a temporary: it leaves with the read.
    return make(n).nargs + len(make(n).dest)


def read_kind(objs):
    return [o.kind for o in objs]


class PolymorphicSiteTests(unittest.TestCase):
    def setUp(self):
        self.objs = [A(1), B(2), C(3), D(4), E(5)]

    def test_five_classes_at_one_site(self):
        want = (15, ["d1", "d2", "d3", "d4", "d5"])
        for _ in range(ROUNDS):
            self.assertEqual(read_local(self.objs), want)

    def test_stack_receivers(self):
        for i in range(ROUNDS):
            k = i % 5
            self.assertEqual(read_stack(i), i + k + len("d%d" % (i + k)))

    def test_fresh_instances_each_round(self):
        for i in range(ROUNDS):
            objs = [A(i), B(i), C(i), D(i), E(i)]
            total, names = read_local(objs)
            self.assertEqual(total, 5 * i)
            self.assertEqual(names, ["d%d" % i] * 5)

    def test_instance_drops_the_attribute(self):
        for _ in range(ROUNDS):
            read_local(self.objs)
        del self.objs[2].nargs
        with self.assertRaises(AttributeError):
            read_local(self.objs)
        C.nargs = 100
        try:
            self.assertEqual(read_local(self.objs)[0], 1 + 2 + 100 + 4 + 5)
        finally:
            del C.nargs
        self.objs[2].nargs = 3
        self.assertEqual(read_local(self.objs)[0], 15)

    def test_class_gains_a_property(self):
        for _ in range(ROUNDS):
            read_local(self.objs)
        E.nargs = property(lambda self: -1, lambda self, v: None)
        try:
            for _ in range(50):
                self.assertEqual(read_local(self.objs)[0], 1 + 2 + 3 + 4 - 1)
        finally:
            del E.nargs
        for _ in range(50):
            self.assertEqual(read_local(self.objs)[0], 15)

    def test_class_gains_getattribute(self):
        for _ in range(ROUNDS):
            read_local(self.objs)

        def hook(self, name):
            if name == "nargs":
                return 1000
            return object.__getattribute__(self, name)

        B.__getattribute__ = hook
        try:
            for _ in range(50):
                self.assertEqual(read_local(self.objs)[0], 1 + 1000 + 3 + 4 + 5)
        finally:
            del B.__getattribute__
        for _ in range(50):
            self.assertEqual(read_local(self.objs)[0], 15)

    def test_published_dict(self):
        for _ in range(ROUNDS):
            read_local(self.objs)
        d = vars(self.objs[3])
        d["nargs"] = 40
        self.assertEqual(read_local(self.objs)[0], 1 + 2 + 3 + 40 + 5)
        d["zzz"] = 0
        del d["dest"]
        with self.assertRaises(AttributeError):
            read_local(self.objs)
        d["dest"] = "back"
        self.assertEqual(read_local(self.objs), (51, ["d1", "d2", "d3", "back", "d5"]))

    def test_class_attribute_under_many_classes(self):
        for _ in range(ROUNDS):
            self.assertEqual(read_kind(self.objs), ["base"] * 5)
        self.objs[1].kind = "mine"
        C.kind = "c"
        try:
            self.assertEqual(read_kind(self.objs), ["base", "mine", "c", "base", "base"])
        finally:
            del C.kind
        self.assertEqual(read_kind(self.objs), ["base", "mine", "base", "base", "base"])

    def test_last_reference_leaves_with_the_read(self):
        dropped = []

        class F(Base):
            def __del__(self):
                dropped.append(self.nargs)

        def one(n):
            return F(n).nargs

        for i in range(200):
            read_local(self.objs)
            self.assertEqual(one(i), i)
        self.assertEqual(dropped, list(range(200)))


if __name__ == "__main__":
    unittest.main()
