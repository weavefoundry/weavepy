"""A small ray tracer: vector classes, method calls, and float math."""

import math

WORK = 80


class Vector:
    __slots__ = ("x", "y", "z")

    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

    def dot(self, other):
        return self.x * other.x + self.y * other.y + self.z * other.z

    def magnitude(self):
        return math.sqrt(self.dot(self))

    def __add__(self, other):
        return Vector(self.x + other.x, self.y + other.y, self.z + other.z)

    def __sub__(self, other):
        return Vector(self.x - other.x, self.y - other.y, self.z - other.z)

    def scale(self, f):
        return Vector(self.x * f, self.y * f, self.z * f)

    def normalized(self):
        return self.scale(1.0 / self.magnitude())

    def reflect_through(self, normal):
        d = normal.scale(self.dot(normal))
        return self - d.scale(2)


class Ray:
    def __init__(self, point, vector):
        self.point = point
        self.vector = vector.normalized()

    def point_at(self, t):
        return self.point + self.vector.scale(t)


class Sphere:
    def __init__(self, centre, radius):
        self.centre = centre
        self.radius = radius

    def intersection_time(self, ray):
        cp = self.centre - ray.point
        v = cp.dot(ray.vector)
        disc = self.radius * self.radius - (cp.dot(cp) - v * v)
        if disc < 0:
            return None
        return v - math.sqrt(disc)

    def normal_at(self, p):
        return (p - self.centre).normalized()


class Plane:
    def __init__(self, point, normal):
        self.point = point
        self.normal = normal.normalized()

    def intersection_time(self, ray):
        v = ray.vector.dot(self.normal)
        if v == 0:
            return None
        return -(ray.point - self.point).dot(self.normal) / v

    def normal_at(self, p):
        return self.normal


class Scene:
    def __init__(self):
        self.objects = []
        self.lights = []
        self.eye = Vector(0, 1.8, 10)
        self.look = Vector(0, 3, 0)

    def add(self, obj, colour):
        self.objects.append((obj, colour))

    def first_hit(self, ray):
        best = None
        for obj, colour in self.objects:
            t = obj.intersection_time(ray)
            if t is not None and t > 1e-6 and (best is None or t < best[0]):
                best = (t, obj, colour)
        return best

    def trace(self, ray, depth):
        hit = self.first_hit(ray)
        if hit is None:
            return (0.1, 0.1, 0.15)
        t, obj, colour = hit
        p = ray.point_at(t)
        n = obj.normal_at(p)
        r, g, b = 0.0, 0.0, 0.0
        for light in self.lights:
            to_light = (light - p).normalized()
            lambert = to_light.dot(n)
            if lambert <= 0:
                continue
            if self.first_hit(Ray(p, to_light)) is not None:
                continue
            r += colour[0] * lambert
            g += colour[1] * lambert
            b += colour[2] * lambert
        if depth < 2:
            rr, rg, rb = self.trace(Ray(p, ray.vector.reflect_through(n)), depth + 1)
            r += 0.3 * rr
            g += 0.3 * rg
            b += 0.3 * rb
        return (r, g, b)

    def render(self, w, h):
        fwd = (self.look - self.eye).normalized()
        right = Vector(fwd.z, 0, -fwd.x).normalized()
        up = Vector(0, 1, 0)
        total = 0.0
        for y in range(h):
            for x in range(w):
                dx = (x - w / 2) / w
                dy = (h / 2 - y) / h
                d = fwd + right.scale(dx) + up.scale(dy)
                r, g, b = self.trace(Ray(self.eye, d), 0)
                total += min(r, 1.0) + min(g, 1.0) + min(b, 1.0)
        return total


def bench(n):
    s = Scene()
    s.add(Sphere(Vector(0, 3, 0), 1), (1.0, 0.2, 0.2))
    s.add(Sphere(Vector(-3, 1.5, -1), 1.2), (0.2, 1.0, 0.2))
    s.add(Sphere(Vector(3, 1, 1), 0.8), (0.2, 0.2, 1.0))
    s.add(Plane(Vector(0, 0, 0), Vector(0, 1, 0)), (0.8, 0.8, 0.8))
    s.lights.append(Vector(30, 30, 10))
    s.lights.append(Vector(-10, 100, 30))
    return round(s.render(n, n), 6)
