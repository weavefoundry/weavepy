"""A call that spreads more arguments than its caller's stack has room for
(`f(*xs)` with a long list), made from compiled code through the core
loop's lane, keeps CPython's results and the caller's stack sound: the
callee's sum, a method's, a property's that spreads in turn (as
`pathlib`'s `_raw_path` spreads its segments into `os.path.join`), and
the caller's own values afterwards."""
import posixpath


def total(*xs):
    t = 0
    for x in xs:
        t += x
    return t


class Box:
    def __init__(self, parts):
        self.parts = parts

    @property
    def joined(self):
        return posixpath.join(*self.parts)

    def tot(self, *xs):
        return total(*xs)


def call(xs, box):
    before = (len(xs), xs[0])
    r = total(*xs) + box.tot(*xs)
    j = box.joined
    return r, j, before


for i in range(3000):
    n = 1 + i % 97
    xs = list(range(n))
    parts = ["p%d" % k for k in range(1 + i % 41)]
    r, j, before = call(xs, Box(parts))
    assert r == n * (n - 1), (r, n)
    assert j == "/".join(parts) and before == (n, 0)
print("ok")
