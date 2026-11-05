"""Named tuple fields read from compiled code (served from their sites'
stamps) keep CPython's results: each field of each instance, a subclass's
instances, `_replace`'s new tuples, and a field the class replaces later
with a property or plain value."""
from collections import namedtuple

Point = namedtuple("Point", "x y label")


class Labeled(Point):
    __slots__ = ()

    @property
    def size(self):
        return self.x + self.y


def run(n):
    t = 0
    for i in range(n):
        p = Point(i % 5, i % 3, "p%d" % (i % 2))
        q = Labeled(i, 1, "q")
        t += p.x + p.y + len(p.label) + q.x + q.size + p._replace(x=10).x
    return t


def expect(n):
    return sum(i % 5 + i % 3 + 2 + i + (i + 1) + 10 for i in range(n))


# Past the frame JIT's threshold, so later rounds run compiled code.
for _ in range(300):
    assert run(20) == expect(20)

# The class replaces a field: every later read sees the new attribute.
Point.label = property(lambda self: "longer")
assert run(20) == expect(20) + 20 * 4
Point.y = 7
assert run(20) == sum(i % 5 + 7 + 6 + i + (i + 7) + 10 for i in range(20))
print("ok")
