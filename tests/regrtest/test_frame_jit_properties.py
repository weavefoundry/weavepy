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
