"""`copy.deepcopy` of trees of plain dicts, lists and tuples.

A tree of exact dicts, lists and tuples over `None`, bools, ints, floats,
strings and bytes is copied natively, as `copy`'s own table copies it:
equal values, new containers, shared parts still shared, cycles kept, and
a tuple whose items all copy to themselves returned itself. Anything else
in the tree, a memo, a very deep tree, or a trace hook takes the Python
function.
"""

import copy
import sys
import unittest


class DeepcopyPlainTests(unittest.TestCase):
    def test_values_and_identity(self):
        src = {"a": [1, 2.5, "x", None, True, b"b"], "t": (1, "s"), "n": {"k": [(), [3]]}, 7: 2**80}
        for _ in range(100):
            out = copy.deepcopy(src)
            self.assertEqual(out, src)
            self.assertIsNot(out, src)
            self.assertIsNot(out["a"], src["a"])
            self.assertIs(out["t"], src["t"])
            self.assertIsNot(out["n"]["k"], src["n"]["k"])
            self.assertIs(out["n"]["k"][0], ())
            self.assertIs(out["a"][2], src["a"][2])
            self.assertEqual(list(out), list(src))
        nested = ([1], [2])
        out = copy.deepcopy(nested)
        self.assertIsNot(out, nested)
        self.assertEqual(out, nested)
        self.assertIsNot(out[0], nested[0])

    def test_sharing_and_cycles(self):
        shared = [1, 2]
        src = [shared, shared, {"s": shared}]
        out = copy.deepcopy(src)
        self.assertIs(out[0], out[1])
        self.assertIs(out[2]["s"], out[0])
        self.assertIsNot(out[0], shared)
        loop = [1]
        loop.append(loop)
        out = copy.deepcopy(loop)
        self.assertIs(out[1], out)
        d = {}
        d["self"] = d
        out = copy.deepcopy(d)
        self.assertIs(out["self"], out)
        t = ([],)
        t[0].append(t)
        out = copy.deepcopy(t)
        self.assertIs(out[0][0], out)

    def test_other_shapes(self):
        class C:
            def __init__(self):
                self.v = [1]

        c = C()
        out = copy.deepcopy([c, {1: c}])
        self.assertIsNot(out[0], c)
        self.assertIs(out[1][1], out[0])
        self.assertEqual(out[0].v, [1])

        class L(list):
            pass

        lst = L([1, [2]])
        out = copy.deepcopy({"l": lst})
        self.assertIs(type(out["l"]), L)
        self.assertIsNot(out["l"][1], lst[1])
        memo = {}
        x = [1, [2]]
        out = copy.deepcopy(x, memo)
        self.assertIn(id(x), memo)
        self.assertEqual(copy.deepcopy({1: {2, 3}}), {1: {2, 3}})
        self.assertEqual(copy.deepcopy([1j, 2]), [1j, 2])
        deep = []
        cur = deep
        for _ in range(300):
            nxt = []
            cur.append(nxt)
            cur = nxt
        out = copy.deepcopy(deep)
        depth = 0
        while out:
            out = out[0]
            depth += 1
        self.assertEqual(depth, 300)

    def test_trace_sees_copy_frames(self):
        seen = []

        def tracer(frame, event, arg):
            if event == "call" and frame.f_code.co_filename.endswith("copy.py"):
                seen.append(frame.f_code.co_name)
            return None

        sys.settrace(tracer)
        try:
            copy.deepcopy({"a": [1, 2]})
        finally:
            sys.settrace(None)
        self.assertIn("deepcopy", seen)
        self.assertIn("_deepcopy_dict", seen)


if __name__ == "__main__":
    unittest.main()
