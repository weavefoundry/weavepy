"""getattr(obj, name, default) and hasattr(obj, name) from compiled loops
when the attribute is missing: on modules (with and without a module
__getattr__, and a module that gains the name or the hook mid-loop), on
strings and containers (a method that is there, a name that isn't, and a
classmethod reached through an instance), and the built-in leaves for
int.bit_count, bytes.find and bytes.hex."""

import types
import unittest


def module_misses(m, n):
    got = []
    for i in range(n):
        got.append(getattr(m, "nope", i))
        got.append(hasattr(m, "nope"))
    return got


def builtin_misses(n):
    out = []
    for i in range(n):
        d = {}
        s = "x"
        out.append((
            getattr(d, "nope", None),
            getattr(s, "nope", 0),
            getattr([], "nope", 1),
            getattr(b"", "nope", 2),
            getattr(d, "fromkeys", None) is not None,
            getattr(b"", "fromhex", None) is not None,
            getattr(s, "maketrans", None) is not None,
            getattr(s, "upper", None)(),
        ))
    return out


class Tests(unittest.TestCase):
    def test_module_without_hook(self):
        m = types.ModuleType("m")
        got = module_misses(m, 2000)
        self.assertEqual(got[:4], [0, False, 1, False])
        self.assertEqual(got[-2:], [1999, False])

    def test_module_with_hook(self):
        m = types.ModuleType("m")
        m.__getattr__ = lambda name: "hooked:" + name
        got = module_misses(m, 2000)
        self.assertEqual(got[:2], ["hooked:nope", True])
        self.assertEqual(got[-2:], ["hooked:nope", True])

    def test_module_changes_mid_loop(self):
        m = types.ModuleType("m")
        got = []
        for i in range(3000):
            if i == 1000:
                m.nope = "here"
            if i == 2000:
                del m.nope
                m.__getattr__ = lambda name: "late"
            got.append(getattr(m, "nope", None))
        self.assertEqual(got[999], None)
        self.assertEqual(got[1000], "here")
        self.assertEqual(got[1999], "here")
        self.assertEqual(got[2000], "late")
        self.assertEqual(got[-1], "late")

    def test_hook_raising_attribute_error(self):
        m = types.ModuleType("m")

        def hook(name):
            raise AttributeError(name)

        m.__getattr__ = hook
        self.assertEqual(module_misses(m, 1000)[-2:], [999, False])

    def test_builtin_values(self):
        out = builtin_misses(2000)
        self.assertEqual(out[0], (None, 0, 1, 2, True, True, True, "X"))
        self.assertEqual(out[-1], out[0])

    def test_leaves(self):
        acc = []
        for i in range(2000):
            b = bytes([i % 256, 2, 3])
            acc.append((
                (i * 2654435761).bit_count(),
                b.find(b"\x03"),
                b.find(3, 1, 3),
                b.find(b"\x09"),
                b.hex(),
                b.hex(":"),
                b.hex(b"-", 2),
            ))
        i = 1999
        b = bytes([i % 256, 2, 3])
        self.assertEqual(acc[-1], (bin(i * 2654435761).count("1"), 2, 2, -1,
                                   b.hex(), b.hex(":"), b.hex(b"-", 2)))
        self.assertEqual(acc[5][5], "05:02:03")
        self.assertEqual(acc[5][6], "05-0203")
        caught = 0
        for i in range(500):
            try:
                b"ab".hex("::")
            except ValueError:
                caught += 1
        self.assertEqual(caught, 500)


if __name__ == "__main__":
    unittest.main()
