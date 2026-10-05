"""int() and float() of text, and builtin methods the interpreter
dispatches by name, called from hot loops."""

import unittest


def loop(fn, n=300):
    out = None
    for i in range(n):
        out = fn(i)
    return out


class ParseTest(unittest.TestCase):
    CASES = ["12", "-7", "+3", "007", "1_0", " 5", "5 ", "1e5", ".5", "5.", "-0",
             "inf", "nan", "1e400", "+.5e-3", "e5", "1.2.3", "--1", "", "１２",
             "-+1", "+-5", "0x-1", "9" * 18, "9" * 19, "-" + "9" * 19]

    def outcome(self, t, v):
        try:
            return repr(t(v))
        except Exception as e:
            return type(e).__name__

    def test_int_and_float(self):
        expected = {
            ("int", "12"): "12", ("int", "--1"): "ValueError", ("int", "-+1"): "ValueError",
            ("int", "1_0"): "10", ("int", " 5"): "5", ("int", "１２"): "12",
            ("float", "1e400"): "inf", ("float", "e5"): "ValueError", ("float", "-0"): "-0.0",
            ("int", "9" * 19): "9" * 19, ("int", "-" + "9" * 19): "-" + "9" * 19,
        }
        for v in self.CASES:
            for t in (int, float):
                got = loop(lambda i: self.outcome(t, v), 3)
                if (t.__name__, v) in expected:
                    self.assertEqual(got, expected[(t.__name__, v)], (t, v))

    def test_int_subclass_and_str_subclass(self):
        class S(str):
            pass

        class I(int):
            pass

        self.assertEqual(loop(lambda i: int(S("42"))), 42)
        self.assertEqual(loop(lambda i: I("42")), 42)
        self.assertIs(type(loop(lambda i: I("42"))), I)


class ExtendTest(unittest.TestCase):
    def test_extend_shapes(self):
        def body(i):
            x = []
            x.extend((1, 2))
            x.extend(range(2))
            x.extend(c for c in "ab")
            return x

        self.assertEqual(loop(body), [1, 2, 0, 1, "a", "b"])

    def test_set_and_str_methods(self):
        def body(i):
            s = {1, 2}
            s.update([3])
            return (sorted(s), s.issubset({1, 2, 3, 4}), "-".join(["a", "b"]))

        self.assertEqual(loop(body), ([1, 2, 3], True, "a-b"))

    def test_extend_raises(self):
        def body(i):
            x = []
            try:
                x.extend(5)
            except TypeError as e:
                return str(e)

        self.assertIn("not iterable", loop(body))



class BoundBuiltinTest(unittest.TestCase):
    def test_local_bound_append(self):
        y = []
        append = y.append
        for i in range(300):
            append(i)
        self.assertEqual(y, list(range(300)))

    def test_bound_append_of_list_subclass(self):
        log = []

        class L(list):
            def append(self, v):
                log.append(v)
                super().append(v * 2)

        y = L()
        append = y.append
        for i in range(5):
            append(i)
        self.assertEqual(y, [0, 2, 4, 6, 8])
        self.assertEqual(log, [0, 1, 2, 3, 4])

    def test_bound_methods_with_arguments(self):
        d = {}
        setdefault = d.setdefault
        s = set()
        add = s.add
        ins = []
        insert = ins.insert
        for i in range(50):
            setdefault(i % 5, i)
            add(i % 7)
            insert(0, i)
        self.assertEqual(d, {0: 0, 1: 1, 2: 2, 3: 3, 4: 4})
        self.assertEqual(s, set(range(7)))
        self.assertEqual(ins[:3], [49, 48, 47])

    def test_bound_append_releases(self):
        import weakref

        class Item:
            pass

        y = []
        append = y.append
        for i in range(10):
            append(Item())
        r = weakref.ref(y[0])
        y.clear()
        self.assertIsNone(r())

if __name__ == "__main__":
    unittest.main()
