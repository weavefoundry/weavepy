"""Object, str and bytes fields read in hot loops give the field's value.

A compiled loop that reads `obj.name` (a string, bytes or instance field)
keeps a handle on the value it read last and takes the same handle again
while the field still holds that value. Whenever the field holds
something else (another object of the lane's type, an equal but distinct
one, `None`, a value of another type, nothing) the read gives what the
field holds now.
"""

import unittest

N = 5000


class Node:
    def __init__(self, label, payload=b"p", nxt=None):
        self.label = label
        self.payload = payload
        self.next = nxt
        self.count = 0


class SlotNode:
    __slots__ = ("label", "payload", "next", "count")

    def __init__(self, label, payload=b"p", nxt=None):
        self.label = label
        self.payload = payload
        self.next = nxt
        self.count = 0


def count_label(obj, want, n):
    hits = 0
    for _ in range(n):
        if obj.label == want:
            hits += 1
    return hits


def label_lengths(obj, n):
    total = 0
    for _ in range(n):
        total += len(obj.label)
    return total


def payload_lengths(obj, n):
    total = 0
    for _ in range(n):
        total += len(obj.payload)
    return total


def follow(obj, n):
    total = 0
    for _ in range(n):
        total += obj.next.count
    return total


def labels_equal(a, b, n):
    same = 0
    for _ in range(n):
        if a.label == b.label:
            same += 1
    return same


class FieldReadTests(unittest.TestCase):
    kinds = (Node, SlotNode)

    def test_stable_fields(self):
        for cls in self.kinds:
            o = cls("abc", b"12345")
            self.assertEqual(count_label(o, "abc", N), N)
            self.assertEqual(count_label(o, "abd", N), 0)
            self.assertEqual(label_lengths(o, N), 3 * N)
            self.assertEqual(payload_lengths(o, N), 5 * N)
            self.assertEqual(labels_equal(o, cls("abc"), N), N)

    def test_field_rebound_between_calls(self):
        for cls in self.kinds:
            o = cls("abc")
            self.assertEqual(label_lengths(o, N), 3 * N)
            o.label = "longer label"
            self.assertEqual(label_lengths(o, N), 12 * N)
            o.label = "".join(["a", "bc"])  # equal to the first, another object
            self.assertEqual(label_lengths(o, N), 3 * N)
            self.assertEqual(count_label(o, "abc", N), N)
            o.payload = b""
            self.assertEqual(payload_lengths(o, N), 0)

    def test_field_rebound_inside_the_loop(self):
        for cls in self.kinds:
            o = cls("a")
            labels = ["a", "bb", "ccc", "a", "dddd"]
            total = 0
            for i in range(N):
                o.label = labels[i % 5]
                total += len(o.label)
            self.assertEqual(total, (N // 5) * (1 + 2 + 3 + 1 + 4))

    def test_alternating_receivers(self):
        for cls in self.kinds:
            objs = [cls("x"), cls("yy"), cls("zzz")]
            total = 0
            for i in range(N):
                total += len(objs[i % 3].label)
            self.assertEqual(total, (N // 3) * 6 + sum((1, 2, 3)[: N % 3]))

    def test_instance_field(self):
        for cls in self.kinds:
            leaf = cls("leaf")
            leaf.count = 7
            head = cls("head", nxt=leaf)
            self.assertEqual(follow(head, N), 7 * N)
            other = cls("other")
            other.count = 2
            head.next = other
            self.assertEqual(follow(head, N), 2 * N)
            head.next = leaf
            leaf.count = 1
            self.assertEqual(follow(head, N), N)
            self.assertIs(head.next, leaf)

    def test_none_and_other_types(self):
        for cls in self.kinds:
            leaf = cls("leaf")
            head = cls("head", nxt=leaf)
            self.assertEqual(follow(head, N), 0)
            head.next = None
            with self.assertRaises(AttributeError):
                follow(head, 3)
            head.next = leaf
            self.assertEqual(follow(head, 10), 0)
            o = cls("abc")
            self.assertEqual(label_lengths(o, N), 3 * N)
            o.label = [1, 2, 3, 4]
            self.assertEqual(label_lengths(o, 10), 40)
            o.label = b"xy"
            self.assertEqual(label_lengths(o, 10), 20)
            o.label = 5
            with self.assertRaises(TypeError):
                label_lengths(o, 3)
            o.label = "back"
            self.assertEqual(label_lengths(o, N), 4 * N)

    def test_deleted_field(self):
        for cls in self.kinds:
            o = cls("abc")
            self.assertEqual(label_lengths(o, N), 3 * N)
            del o.label
            with self.assertRaises(AttributeError):
                label_lengths(o, 3)
            o.label = "re"
            self.assertEqual(label_lengths(o, N), 2 * N)

    def test_the_value_read_is_the_object_stored(self):
        for cls in self.kinds:
            first = "".join(["id", "entity"])
            second = "".join(["id", "entity"])
            self.assertIsNot(first, second)
            o = cls(first)
            seen = None
            for i in range(N):
                seen = o.label
            self.assertIs(seen, first)
            o.label = second
            for i in range(N):
                seen = o.label
            self.assertIs(seen, second)

    def test_nested_calls_of_one_loop(self):
        # The same code runs in several activations at once, each with
        # its own receivers.
        def walk(node, depth):
            total = 0
            for _ in range(40):
                total += len(node.label)
            if depth:
                total += walk(node.next, depth - 1)
            return total

        for cls in self.kinds:
            chain = None
            for k in range(12, 0, -1):
                chain = cls("n" * k, nxt=chain)
            want = 40 * sum(range(1, 13))
            for _ in range(200):
                self.assertEqual(walk(chain, 11), want)

    def test_released_values_are_not_kept_alive_forever(self):
        import gc
        import weakref

        class Payload:
            pass

        for cls in self.kinds:
            leaf = Payload()
            leaf.count = 3
            ref = weakref.ref(leaf)
            head = cls("head", nxt=leaf)
            self.assertEqual(follow(head, N), 3 * N)
            head.next = None
            del leaf
            gc.collect()
            self.assertIsNone(ref())


if __name__ == "__main__":
    unittest.main()
