"""Protocol 5 bytearrays through the native pickler and unpickler.

`BYTEARRAY8` is written natively (a large one outside the frame, as the
Python pickler writes it) and read back natively; the bytes must equal the
Python pickler's exactly, identity must survive the memo, and protocol 4
keeps its `copyreg` reduction.
"""

import pickle
import pickletools
import unittest


class PickleBytearrayTests(unittest.TestCase):
    def check(self, value, proto):
        data = pickle.dumps(value, proto)
        self.assertEqual(data, pickle._dumps(value, proto))
        back = pickle.loads(data)
        self.assertEqual(back, value)
        self.assertEqual(pickle._loads(data), value)
        return data, back

    def test_sizes(self):
        for n in (0, 1, 255, 256, 65535, 65536, 70000):
            value = bytearray(bytes(range(256)) * (n // 256 + 1))[:n]
            for proto in (4, 5):
                _, back = self.check(value, proto)
                self.assertIs(type(back), bytearray)

    def test_opcode(self):
        data = pickle.dumps(bytearray(b"abc"), 5)
        ops = [op.name for op, _, _ in pickletools.genops(data)]
        self.assertIn("BYTEARRAY8", ops)
        data4 = pickle.dumps(bytearray(b"abc"), 4)
        self.assertNotIn("BYTEARRAY8", [op.name for op, _, _ in pickletools.genops(data4)])

    def test_memo_identity(self):
        shared = bytearray(b"shared")
        value = [shared, shared, (shared, b"x"), {"k": shared}]
        _, back = self.check(value, 5)
        self.assertIs(back[0], back[1])
        self.assertIs(back[0], back[2][0])
        self.assertIs(back[0], back[3]["k"])
        back[0].append(33)
        self.assertEqual(back[1], bytearray(b"shared!"))

    def test_nested(self):
        value = {"data": bytearray(b"v" * 1000), "meta": (1, "two", b"three"), "list": [bytearray()]}
        for _ in range(200):
            self.check(value, 5)

    def test_independent_copies(self):
        a = bytearray(b"abc")
        back = pickle.loads(pickle.dumps(a, 5))
        back[0] = 0
        self.assertEqual(a, bytearray(b"abc"))


if __name__ == "__main__":
    unittest.main()
