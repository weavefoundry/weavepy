"""Property reads and `super()` lookups in compiled code.

A compiled `obj.prop` whose getter the site has cached runs the getter
directly (a pure leaf in place), and `super().name` resolves without
leaving native code. A getter that raises, a class whose property is
replaced mid-loop, and cooperative `super()` chains over a diamond must
all behave as the interpreter runs them.
"""

import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


class Box:
    def __init__(self, w):
        self._w = w

    @property
    def w(self):
        return self._w

    @property
    def doubled(self):
        return [self._w, self._w]

    @property
    def fragile(self):
        if self._w < 0:
            raise ValueError("negative")
        return self._w


def read_props(boxes, n):
    t = 0
    for i in range(n):
        b = boxes[i % len(boxes)]
        t += b.w + len(b.doubled)
    return t


def read_fragile(boxes, n):
    t = 0
    for i in range(n):
        t += boxes[i % len(boxes)].fragile
    return t


def read_swapped(n):
    class C:
        @property
        def v(self):
            return 1

    c = C()
    t = 0
    for i in range(n):
        if i == n // 2:
            C.v = property(lambda self: 10)
        t += c.v
    return t


class Wide(Box):
    @property
    def w(self):
        return self._w * 10


class Lazy:
    def __getattr__(self, name):
        return 7


def counted():
    def wrapper():
        wrapper.calls += 1
        return wrapper.calls

    wrapper.calls = 0
    return wrapper


def other_attrs(objs, n):
    w = counted()
    t = 0
    for i in range(n):
        o = objs[i % len(objs)]
        t += type(o).__name__ == "Box"
        t += w()
        t += o.w
    return t, w.calls


def missing_attr(obj, n):
    caught = 0
    for i in range(n):
        try:
            obj.nope
        except AttributeError:
            caught += 1
    return caught


def getattr_fallback(obj, n):
    out = 0
    for i in range(n):
        x = obj.anything
        out += x
    return out


class Base:
    def __init__(self, tag):
        self.tags = [tag]

    def name(self):
        return "base"


class Left(Base):
    def __init__(self, tag):
        super().__init__(tag)
        self.tags.append("left")

    def name(self):
        return "left>" + super().name()


class Right(Base):
    def __init__(self, tag):
        super().__init__(tag)
        self.tags.append("right")

    def name(self):
        return "right>" + super().name()


class Diamond(Left, Right):
    def __init__(self):
        super().__init__("d")

    def name(self):
        return "diamond>" + super().name()


def build(n):
    out = None
    names = set()
    for _ in range(n):
        out = Diamond()
        names.add(out.name())
    return out.tags, names


class PropertyAndSuperTest(unittest.TestCase):
    def test_props(self):
        boxes = [Box(1), Box(2), Box(3)]
        self.assertEqual(read_props(boxes, N), sum((i % 3 + 1) + 2 for i in range(N)))

    def test_raise(self):
        self.assertEqual(read_fragile([Box(1)], N), N)
        with self.assertRaisesRegex(ValueError, "negative"):
            read_fragile([Box(1), Box(-1)], N)

    def test_swapped_property(self):
        self.assertEqual(read_swapped(4000), 2000 + 2000 * 10)

    def test_other_attributes(self):
        objs = [Box(1), Wide(2)]
        t, calls = other_attrs(objs, N)
        self.assertEqual(calls, N)
        self.assertEqual(t, N // 2 + sum(range(1, N + 1)) + N // 2 * 1 + N // 2 * 20)

    def test_missing_attribute(self):
        self.assertEqual(missing_attr(Box(1), N), N)

    def test_getattr_in_place(self):
        self.assertEqual(getattr_fallback(Lazy(), 50), 350)

    def test_super_chains(self):
        tags, names = build(N)
        self.assertEqual(tags, ["d", "right", "left"])
        self.assertEqual(names, {"diamond>left>right>base"})

    def test_forced_frame_jit(self):
        if FORCED in sys.argv:
            self.skipTest("already forced")
        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
