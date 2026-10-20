"""Per-site caches must stay with their own instruction.

The interpreter keeps its attribute, global, method and call caches in
tables with an entry per *site* instruction rather than per instruction,
so each lookup maps the instruction to its entry. These checks run warm
code whose neighboring sites read different names from the same objects
(a cache answered for the wrong site returns the other name's value), and
code with more sites than the tables index.
"""


class A:
    __slots__ = ()
    k = "A.k"

    def __init__(self):
        pass


class P:
    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

    def total(self):
        return self.x + self.y + self.z

    def swap(self):
        return P(self.y, self.x, self.z)


class Q(P):
    def total(self):
        return -(self.x + self.y + self.z)


class R:
    def __init__(self, x, y, z):
        self.z = z
        self.y = y
        self.x = x

    def total(self):
        return self.x * 100 + self.y * 10 + self.z


SCALE = 3


def neighbors(objs):
    out = []
    for o in objs:
        # Adjacent attribute sites with different names on one receiver,
        # separated by non-site instructions.
        a = o.x
        b = o.y
        c = o.z
        out.append((a, b, c, o.total(), SCALE, len(out)))
    return out


class Ctx:
    def __init__(self, log, tag):
        self.log = log
        self.tag = tag

    def __enter__(self):
        self.log.append("enter " + self.tag)
        return self.tag

    def __exit__(self, *exc):
        self.log.append("exit " + self.tag)
        return False


class Ctx2(Ctx):
    def __enter__(self):
        self.log.append("enter2 " + self.tag)
        return self.tag * 2


def contexts(n):
    log = []
    got = []
    for i in range(n):
        with Ctx(log, "a") as x:
            got.append(x)
        with (Ctx2 if i % 2 else Ctx)(log, "b") as y:
            got.append(y)
    return log, got


for round_ in range(40):
    objs = [P(1, 2, 3), Q(4, 5, 6), R(7, 8, 9), P(10, 20, 30)]
    if round_ == 20:
        SCALE = 4
    got = neighbors(objs)
    scale = 3 if round_ < 20 else 4
    assert got == [
        (1, 2, 3, 6, scale, 0),
        (4, 5, 6, -15, scale, 1),
        (7, 8, 9, 789, scale, 2),
        (10, 20, 30, 60, scale, 3),
    ], (round_, got)
    swapped = [o.swap() for o in objs if type(o) is P]
    assert [(s.x, s.y, s.z) for s in swapped] == [(2, 1, 3), (20, 10, 30)]
    assert A().k == "A.k"

log, got = None, None
for _ in range(20):
    log, got = contexts(6)
assert got == ["a", "b", "a", "bb"] * 3, got
assert log[:4] == ["enter a", "exit a", "enter b", "exit b"], log[:4]
assert log[6:8] == ["enter2 b", "exit b"], log[6:8]

# More attribute sites than the tables index (65,535): the ones past
# the limit run uncached, and every site still reads its own name.
lines = ["def many(o):", "    t = 0"]
for i in range(13200):
    lines.append("    t += o.x - o.z + o.y - o.y + o.z")
lines.append("    return t, o.y, o.x")
ns = {}
exec("\n".join(lines), ns)
many = ns["many"]
for o in (P(5, 7, 2), R(5, 7, 2), Q(5, 7, 2)) * 2:
    assert many(o) == (13200 * 5, 7, 5), many(o)
