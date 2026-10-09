"""Calls of callables an instance holds as attributes (`self.fn(...)`).

The method-form load finds the instance's own attribute before the
class's, and calls it without binding the instance. A class method of
the same name, a later rebinding, and a deleted attribute must resolve
as the ordinary lookup does.
"""

import unittest


class Node:
    OPS = {"+": lambda a, b: a + b, "*": lambda a, b: a * b}

    def __init__(self, op, left, right):
        self.fn = self.OPS[op]
        self.left = left
        self.right = right

    def eval(self):
        return self.fn(self.left, self.right)


class Shadow:
    def act(self):
        return "class"


class InstanceCallableMethodTest(unittest.TestCase):
    def test_stored_callables(self):
        nodes = [Node("+" if i % 2 else "*", i, 3) for i in range(1000)]
        total = 0
        for _ in range(3):
            for n in nodes:
                total += n.eval()
        self.assertEqual(total, 3 * sum((i + 3) if i % 2 else (i * 3) for i in range(1000)))

    def test_shadowing_and_rebinding(self):
        s = Shadow()
        out = []
        for i in range(2000):
            if i == 500:
                s.act = lambda: "instance"
            if i == 1500:
                del s.act
            out.append(s.act())
        self.assertEqual(out[499], "class")
        self.assertEqual(out[500], "instance")
        self.assertEqual(out[1499], "instance")
        self.assertEqual(out[1500], "class")


if __name__ == "__main__":
    unittest.main()
