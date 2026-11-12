"""Loops dense with calls of float-working methods and constructors (which
tier 2 keeps native, see `float_work`) compute what the interpreter does:
fields written and read as floats, `math` calls, a callee whose float
field turns into an int or a str partway, and an int-only callee."""

import math


class P:
    def __init__(self, i):
        self.x = math.sin(i)
        self.y = math.cos(i) * 3.0
        self.z = (self.x * self.x) / 2.0

    def norm(self):
        n = math.sqrt(self.x * self.x + self.y * self.y + self.z * self.z)
        self.x /= n
        self.y /= n
        self.z /= n

    def big(self, o):
        self.x = self.x if self.x > o.x else o.x
        return self


class C:
    def __init__(self):
        self.n = 0

    def bump(self, i):
        self.n += i


def run(n):
    ps = [None] * n
    for i in range(n):
        ps[i] = P(i)
    for p in ps:
        p.norm()
    m = ps[0]
    for p in ps[1:]:
        m = m.big(p)
    c = C()
    for i in range(n):
        c.bump(i)
    return m.x, sum(p.y for p in ps), c.n


def ref(n):
    xs = []
    for i in range(n):
        x = math.sin(i)
        y = math.cos(i) * 3.0
        z = (x * x) / 2.0
        k = math.sqrt(x * x + y * y + z * z)
        xs.append((x / k, y / k))
    return max(x for x, _ in xs), sum(y for _, y in xs), n * (n - 1) // 2


for _ in range(3):
    got, want = run(5000), ref(5000)
    assert got[0] == want[0] and got[2] == want[2], (got, want)
    assert abs(got[1] - want[1]) < 1e-9, (got, want)


def shifting(n):
    ps = [P(i) for i in range(n)]
    for i, p in enumerate(ps):
        if i == n // 2:
            p.x = 7
        if i == n - 1:
            p.y = "s"
    t = 0.0
    for p in ps[: n - 1]:
        t += p.big(ps[0]).x
    return t, ps[-1].y


t, y = shifting(4000)
assert y == "s" and t > 0
print("ok")
