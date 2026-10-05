"""Chaos-game fractal with spline curves: OO numeric code with many small
methods, properties, and list indexing."""

import random

WORK = 6000


class GVector:
    def __init__(self, x=0, y=0, z=0):
        self.x = x
        self.y = y
        self.z = z

    def mag(self):
        return (self.x ** 2 + self.y ** 2 + self.z ** 2) ** 0.5

    def dist(self, other):
        return ((self.x - other.x) ** 2 + (self.y - other.y) ** 2 + (self.z - other.z) ** 2) ** 0.5

    def __add__(self, other):
        return GVector(self.x + other.x, self.y + other.y, self.z + other.z)

    def __sub__(self, other):
        return self + other * -1

    def __mul__(self, other):
        return GVector(self.x * other, self.y * other, self.z * other)

    __rmul__ = __mul__

    def linear_combination(self, other, l1, l2=None):
        if l2 is None:
            l2 = 1 - l1
        return GVector(self.x * l1 + other.x * l2, self.y * l1 + other.y * l2, self.z * l1 + other.z * l2)


class Spline:
    def __init__(self, points, degree, knots):
        self.points = points
        self.degree = degree
        self.knots = knots

    @property
    def domain(self):
        return (self.knots[self.degree - 1], self.knots[len(self.knots) - self.degree])

    def __call__(self, u):
        dom = self.domain
        for ii in range(self.degree - 1, len(self.knots) - self.degree):
            if self.knots[ii] <= u <= self.knots[ii + 1]:
                break
        else:
            ii = dom[1] - 1
        I = ii
        d = [self.points[I - self.degree + 1 + ii] for ii in range(self.degree + 1)]
        U = self.knots
        for ik in range(1, self.degree + 1):
            for ii in range(I - self.degree + ik + 1, I + 2):
                ua = U[ii + self.degree - ik]
                ub = U[ii - 1]
                co1 = (ua - u) / (ua - ub)
                co2 = (u - ub) / (ua - ub)
                index = ii - I + self.degree - ik - 1
                d[index] = d[index].linear_combination(d[index + 1], co1, co2)
        return d[0]


class Chaosgame:
    def __init__(self, splines, thickness=0.1):
        self.splines = splines
        self.thickness = thickness
        self.minx = min(p.x for s in splines for p in s.points)
        self.miny = min(p.y for s in splines for p in s.points)
        self.maxx = max(p.x for s in splines for p in s.points)
        self.maxy = max(p.y for s in splines for p in s.points)
        self.height = self.maxy - self.miny
        self.width = self.maxx - self.minx
        self.num_trafos = []
        maxlength = thickness * self.width / self.height
        for spl in splines:
            length = 0
            curr = spl(0)
            for i in range(1, 1000):
                last = curr
                t = 1 / 999 * i
                curr = spl(t)
                length += curr.dist(last)
            self.num_trafos.append(max(1, int(length / maxlength * 1.5)))
        self.num_total = sum(self.num_trafos)

    def get_random_trafo(self, rng):
        r = rng.randrange(int(self.num_total) + 1)
        l = 0
        for i in range(len(self.num_trafos)):
            if l <= r < l + self.num_trafos[i]:
                return i, rng.randrange(self.num_trafos[i])
            l += self.num_trafos[i]
        return len(self.num_trafos) - 1, rng.randrange(self.num_trafos[-1])

    def transform_point(self, point, rng):
        x = (point.x - self.minx) / self.width
        y = (point.y - self.miny) / self.height
        trafo = self.get_random_trafo(rng)
        start, end = self.splines[trafo[0]].domain
        length = end - start
        seg_length = length / self.num_trafos[trafo[0]]
        t = start + seg_length * trafo[1] + seg_length * x
        basepoint = self.splines[trafo[0]](t)
        if t + 1 / 50000 > end:
            neighbour = self.splines[trafo[0]](t - 1 / 50000)
            derivative = neighbour - basepoint
        else:
            neighbour = self.splines[trafo[0]](t + 1 / 50000)
            derivative = basepoint - neighbour
        if derivative.mag() != 0:
            basepoint.x += derivative.y / derivative.mag() * (y - 0.5) * self.thickness
            basepoint.y += -derivative.x / derivative.mag() * (y - 0.5) * self.thickness
        return basepoint

    def run(self, n, rng):
        point = GVector((self.maxx + self.minx) / 2, (self.maxy + self.miny) / 2, 0)
        acc = 0.0
        for _ in range(n):
            point = self.transform_point(point, rng)
            acc += point.x * 3 + point.y
        return acc


def bench(n):
    rng = random.Random(1234)
    splines = [
        Spline([GVector(1.597350, 3.304460, 0.0), GVector(1.575810, 4.123260, 0.0),
                GVector(1.313210, 5.288350, 0.0), GVector(1.618900, 5.329910, 0.0),
                GVector(2.889940, 5.502700, 0.0), GVector(2.373060, 4.381830, 0.0),
                GVector(1.662000, 4.360280, 0.0)], 3, [0, 0, 0, 1, 1, 1, 2, 2, 2]),
        Spline([GVector(2.804500, 4.017350, 0.0), GVector(2.550500, 3.525230, 0.0),
                GVector(1.979010, 2.620360, 0.0), GVector(1.979010, 2.620360, 0.0)],
               3, [0, 0, 0, 1, 1, 1]),
        Spline([GVector(2.001670, 4.011320, 0.0), GVector(2.335040, 3.312830, 0.0),
                GVector(2.366800, 3.233460, 0.0), GVector(2.366800, 3.233460, 0.0)],
               3, [0, 0, 0, 1, 1, 1]),
    ]
    game = Chaosgame(splines, 0.25)
    return round(game.run(n, rng), 4)
