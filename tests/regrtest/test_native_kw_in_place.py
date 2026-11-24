"""Keyword calls of native methods and functions from compiled loops,
which read their operands where they sit on the stack: the results, the
order in which the operands are released, keyword values that are heap
objects the call drops last, callbacks that raise, bound classmethods,
and a receiver that only the bound method holds."""

import gc
import unittest


class Noisy:
    """Records its finalization in a shared log."""

    def __init__(self, log, tag, value=0):
        self.log = log
        self.tag = tag
        self.value = value

    def __del__(self):
        self.log.append(self.tag)

    def __lt__(self, other):
        return self.value < other.value


def to_from_bytes(n):
    acc = 0
    for i in range(n):
        x = 258 + i
        b = x.to_bytes(2, byteorder="big")
        acc += int.from_bytes(b, byteorder="big")
        acc += int.from_bytes(b, byteorder="little", signed=False)
        acc += len(x.to_bytes(length=4, byteorder="little"))
    return acc


def str_kw(n):
    out = []
    for i in range(n):
        s = "a.b.c\nd"
        out.append((
            s.rsplit(".", maxsplit=1),
            s.splitlines(keepends=True),
            s.split(sep=".", maxsplit=i % 3),
            s.expandtabs(tabsize=4),
        ))
    return out


def sort_with_key(n):
    out = None
    for i in range(n):
        out = sorted([3, -1, 2, -i], key=abs, reverse=True)
    return out


def raising_key(n):
    caught = 0
    for i in range(n):
        try:
            sorted([1, 2, 3], key=lambda v: 1 // (v - 2))
        except ZeroDivisionError:
            caught += 1
    return caught


def wrong_keyword(n):
    caught = 0
    for i in range(n):
        try:
            (258).to_bytes(2, byteorder="big", bogus=1)
        except TypeError as e:
            caught += "bogus" in str(e)
    return caught


def release_order(log, n):
    for i in range(n):
        items = [Noisy(log, "item-a", 2), Noisy(log, "item-b", 1)]
        key = Noisy(log, "key")
        # The list and the key function's holder die with the call.
        result = sorted(items, key=lambda v, k=key: v.value, reverse=False)
        del items, key
        log.append("call")
        del result
        log.append("after")


class MyInt(int):
    pass


def subclass_from_bytes(n):
    out = None
    for i in range(n):
        out = MyInt.from_bytes(b"\x01\x02", byteorder="big")
    return out


def keyword_halves(n):
    out = []
    for i in range(n):
        v = -(i % 300) - 1
        out.append((
            v.to_bytes(2, byteorder="little", signed=True),
            (i % 256).to_bytes(byteorder="big"),
            True.to_bytes(length=2, byteorder="big"),
            (2**70 + i).to_bytes(length=9, byteorder="big"),
            "a\r\nb\nc".splitlines(keepends=i % 2),
            "a b  c".rsplit(maxsplit=1),
            "a-b-c".rsplit(sep="-"),
            int.from_bytes(bytearray(b"\xff\x01"), byteorder="little", signed=True),
            int.from_bytes(b"\x01\x02", byteorder="big"),
        ))
    return out


def keyword_half_errors(n):
    caught = []
    for i in range(n):
        for f in (
            lambda: (300).to_bytes(1, byteorder="big"),
            lambda: (1).to_bytes(-1, byteorder="big"),
            lambda: (1).to_bytes(2, byteorder="middle"),
            lambda: (-1).to_bytes(2, byteorder="big"),
            lambda: (1).to_bytes(2, "big", True),
            lambda: "a".rsplit(maxsplit="1"),
            lambda: int.from_bytes(b"\x01", "big", True),
            lambda: int.from_bytes(b"\x01", byteorder="big", bogus=1),
            lambda: int.from_bytes(b"\x01", byteorder="middle"),
        ):
            try:
                f()
            except (OverflowError, ValueError, TypeError) as e:
                caught.append(type(e).__name__)
    return caught


def bound_receiver(n):
    acc = 0
    for i in range(n):
        split = "x y z".split
        acc += len(split(maxsplit=1))
        acc += len(bytes.fromhex("0102").hex(sep=":"))
    return acc


class Tests(unittest.TestCase):
    def test_to_from_bytes(self):
        n = 3000
        want = sum(
            (258 + i) + int.from_bytes((258 + i).to_bytes(2, "big"), "little") + 4
            for i in range(n)
        )
        self.assertEqual(to_from_bytes(n), want)

    def test_str_methods(self):
        out = str_kw(2000)
        self.assertEqual(out[0], (["a.b", "c\nd"], ["a.b.c\n", "d"], ["a.b.c\nd"], "a.b.c\nd"))
        self.assertEqual(out[1][2], ["a", "b.c\nd"])
        self.assertEqual(out[2][2], ["a", "b", "c\nd"])

    def test_sort_key(self):
        self.assertEqual(sort_with_key(2000), [-1999, 3, 2, -1])

    def test_raising_callback(self):
        self.assertEqual(raising_key(2000), 2000)

    def test_wrong_keyword(self):
        self.assertEqual(wrong_keyword(2000), 2000)

    def test_release_order(self):
        log = []
        release_order(log, 1500)
        gc.collect()
        # Each round: the key's holder dies when the lambda (an operand)
        # goes, as the call returns; the sorted copy keeps the items alive
        # until it goes, and releases its last item first.
        self.assertEqual(log[:10], ["key", "call", "item-a", "item-b", "after"] * 2)
        self.assertEqual(log[-5:], ["key", "call", "item-a", "item-b", "after"])
        self.assertEqual(log.count("call"), 1500)
        self.assertEqual(log.count("key"), 1500)
        self.assertEqual(log.count("item-a"), 1500)

    def test_keyword_halves(self):
        out = keyword_halves(1000)
        for i, row in enumerate(out):
            v = -(i % 300) - 1
            self.assertEqual(row[0], v.to_bytes(2, "little", signed=True))
            self.assertEqual(row[1], bytes([i % 256]))
            self.assertEqual(row[2], b"\x00\x01")
            self.assertEqual(row[3], (2**70 + i).to_bytes(9, "big"))
            self.assertEqual(row[4], ["a\r\n", "b\n", "c"] if i % 2 else ["a", "b", "c"])
            self.assertEqual(row[5], ["a b", "c"])
            self.assertEqual(row[6], ["a", "b", "c"])
            self.assertEqual(row[7], 511)
            self.assertEqual(row[8], 258)

    def test_keyword_half_errors(self):
        caught = keyword_half_errors(500)
        self.assertEqual(
            caught[:9],
            ["OverflowError", "ValueError", "ValueError", "OverflowError", "TypeError",
             "TypeError", "TypeError", "TypeError", "ValueError"],
        )
        self.assertEqual(len(caught), 4500)

    def test_subclass_from_bytes(self):
        r = subclass_from_bytes(2000)
        self.assertIs(type(r), MyInt)
        self.assertEqual(r, 258)

    def test_bound_receiver(self):
        self.assertEqual(bound_receiver(2000), 2000 * (2 + 5))


if __name__ == "__main__":
    unittest.main()
