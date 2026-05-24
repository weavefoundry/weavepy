"""Tiny n-body simulation — float-heavy arithmetic dominates."""

import os


def _advance(bodies, dt):
    pairs = []
    n = len(bodies)
    i = 0
    while i < n:
        j = i + 1
        while j < n:
            pairs.append((i, j))
            j += 1
        i += 1
    for i, j in pairs:
        bi = bodies[i]
        bj = bodies[j]
        dx = bi[0] - bj[0]
        dy = bi[1] - bj[1]
        dz = bi[2] - bj[2]
        d2 = dx * dx + dy * dy + dz * dz
        mag = dt / (d2 * (d2 ** 0.5))
        bm = bj[6] * mag
        bi[3] -= dx * bm
        bi[4] -= dy * bm
        bi[5] -= dz * bm
        am = bi[6] * mag
        bj[3] += dx * am
        bj[4] += dy * am
        bj[5] += dz * am
    for b in bodies:
        b[0] += dt * b[3]
        b[1] += dt * b[4]
        b[2] += dt * b[5]


def _energy(bodies):
    e = 0.0
    n = len(bodies)
    for i in range(n):
        b = bodies[i]
        e += 0.5 * b[6] * (b[3] * b[3] + b[4] * b[4] + b[5] * b[5])
        for j in range(i + 1, n):
            c = bodies[j]
            dx = b[0] - c[0]
            dy = b[1] - c[1]
            dz = b[2] - c[2]
            e -= b[6] * c[6] / (dx * dx + dy * dy + dz * dz) ** 0.5
    return e


def bench(n):
    bodies = [
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.001],
        [0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.001],
    ]
    for _ in range(n):
        _advance(bodies, 0.01)
    return _energy(bodies)


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1"))
    bench(n)
